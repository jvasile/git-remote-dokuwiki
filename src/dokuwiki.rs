//! DokuWiki XML-RPC client with cookie-based authentication

use anyhow::{anyhow, Context, Result};
use cookie_store::CookieStore;
use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, CONTENT_TYPE, COOKIE, SET_COOKIE};
use std::error::Error as StdError;
use std::fs;
use std::io::{BufReader, BufWriter, Cursor};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use xmlrpc::{Request as XmlRpcRequest, Transport, Value};

use crate::verbosity::Verbosity;

/// Information about a wiki page
#[derive(Debug, Clone)]
pub struct PageInfo {
    pub id: String,
    pub revision: i64,
    pub last_modified: i64,
    pub author: String,
    pub size: i64,
}

/// A single revision of a page
#[derive(Debug, Clone)]
pub struct PageVersion {
    pub page_id: Option<String>, // Only set when returned from getRecentChanges
    pub version: i64,            // timestamp
    pub author: String,
    pub summary: String,
    pub size: i64,
}

/// Custom transport that handles cookies
struct CookieTransport<'a> {
    client: &'a Client,
    url: &'a str,
    cookie_store: &'a Arc<RwLock<CookieStore>>,
}

impl<'a> Transport for CookieTransport<'a> {
    type Stream = Cursor<Vec<u8>>;

    fn transmit(
        self,
        request: &XmlRpcRequest<'_>,
    ) -> std::result::Result<Self::Stream, Box<dyn StdError + Send + Sync>> {
        // Serialize request
        let mut body = Vec::new();
        request.write_as_xml(&mut body)?;

        // Get cookies
        let cookie_header = {
            let store = self.cookie_store.read().unwrap();
            let url: url::Url = self.url.parse().unwrap();
            store
                .get_request_values(&url)
                .map(|(name, value)| format!("{}={}", name, value))
                .collect::<Vec<_>>()
                .join("; ")
        };

        let mut req = self.client.post(self.url).body(body);

        if !cookie_header.is_empty() {
            req = req.header(COOKIE, cookie_header);
        }

        let response = req
            .header(CONTENT_TYPE, HeaderValue::from_static("text/xml"))
            .send()?;

        // Store any new cookies
        for cookie_header in response.headers().get_all(SET_COOKIE) {
            if let Ok(cookie_str) = cookie_header.to_str() {
                let url: url::Url = self.url.parse().unwrap();
                let mut store = self.cookie_store.write().unwrap();
                let _ = store.parse(cookie_str, &url);
            }
        }

        // Check for HTTP errors
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP error: {}", status).into());
        }

        let body = response.bytes()?.to_vec();
        Ok(Cursor::new(body))
    }
}

/// DokuWiki XML-RPC client
pub struct DokuWikiClient {
    wiki_url: String,
    rpc_url: String,
    user: String,
    client: Client,
    cookie_store: Arc<RwLock<CookieStore>>,
    cookie_path: PathBuf,
    has_loaded_cookies: bool,
    verbosity: Verbosity,
}

impl DokuWikiClient {
    /// Create a new client for the given wiki URL
    pub fn new(wiki_url: &str, user: &str, verbosity: Verbosity) -> Result<Self> {
        let wiki_url = wiki_url.trim_end_matches('/').to_string();
        let rpc_url = format!("{}/lib/exe/xmlrpc.php", wiki_url);

        // Load cookies from env var path if set, otherwise from repo path
        // Do this first because during clone, .git may not exist yet
        let load_path = get_cookie_load_path();

        // Load existing cookies or create empty store
        let mut has_loaded_cookies = false;
        let cookie_store = if let Ok(ref path) = load_path {
            if path.exists() {
                let file = fs::File::open(path).ok();
                if let Some(file) = file {
                    let reader = BufReader::new(file);
                    // Use new serde API and load all cookies including expired
                    match cookie_store::serde::json::load_all(reader) {
                        Ok(store) => {
                            has_loaded_cookies = true;
                            store
                        }
                        Err(_) => CookieStore::new(None),
                    }
                } else {
                    CookieStore::new(None)
                }
            } else {
                CookieStore::new(None)
            }
        } else {
            CookieStore::new(None)
        };

        // Determine where to save cookies (repo's .git directory)
        // This may fail during clone before .git exists, that's OK - we'll try again when saving
        let cookie_path = get_repo_cookie_path().unwrap_or_else(|_| {
            // Fall back to load path if available, otherwise a temp location
            load_path.unwrap_or_else(|_| PathBuf::from(".git/dokuwiki-cookies.json"))
        });

        let cookie_store = Arc::new(RwLock::new(cookie_store));

        let client = Client::builder()
            .cookie_store(true)
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            wiki_url,
            rpc_url,
            user: user.to_string(),
            client,
            cookie_store,
            cookie_path,
            has_loaded_cookies,
            verbosity,
        })
    }

    /// Save cookies to disk
    fn save_cookies(&self) -> Result<()> {
        if let Some(parent) = self.cookie_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let store = self.cookie_store.read().unwrap();
        let file = fs::File::create(&self.cookie_path)?;
        let mut writer = BufWriter::new(file);

        // Use the new serde API and include non-persistent (session) cookies
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&store, &mut writer)
            .map_err(|e| anyhow!("Failed to save cookies: {}", e))?;

        Ok(())
    }

    /// Make an XML-RPC call (internal, no retry)
    fn call_inner(&self, method: &str, params: &[Value]) -> Result<Value> {
        let mut request = XmlRpcRequest::new(method);
        for param in params {
            request = request.arg(param.clone());
        }

        // Create transport with cookies
        let transport = CookieTransport {
            client: &self.client,
            url: &self.rpc_url,
            cookie_store: &self.cookie_store,
        };

        let value = request
            .call(transport)
            .map_err(|e| anyhow!("XML-RPC call failed: {}", e))?;

        Ok(value)
    }

    /// Make an XML-RPC call with automatic re-auth on 401
    pub fn call(&mut self, method: &str, params: Vec<Value>) -> Result<Value> {
        match self.call_inner(method, &params) {
            Ok(value) => Ok(value),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("401") || err_str.contains("Unauthorized") {
                    // Session expired, re-authenticate and retry
                    self.reauthenticate()?;
                    self.call_inner(method, &params)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Re-authenticate after a session expiry
    fn reauthenticate(&mut self) -> Result<()> {
        self.verbosity.info("Session expired, re-authenticating...");

        // Clear old cookies
        if self.cookie_path.exists() {
            let _ = std::fs::remove_file(&self.cookie_path);
        }
        *self.cookie_store.write().unwrap() = CookieStore::new(None);

        // Get fresh credentials and login
        let (user, password) = self.get_credentials()?;
        self.login(&user, &password)?;
        self.user = user;
        self.save_cookies()?;

        Ok(())
    }

    /// Check if we have a cached session (cookies were loaded)
    fn has_cached_session(&self) -> bool {
        self.has_loaded_cookies
    }

    /// Get the wiki host (e.g., "wiki.example.com")
    pub fn wiki_host(&self) -> &str {
        self.wiki_url
            .strip_prefix("https://")
            .or_else(|| self.wiki_url.strip_prefix("http://"))
            .unwrap_or(&self.wiki_url)
    }

    /// Ensure we're authenticated, prompting for password if needed
    pub fn ensure_authenticated(&mut self) -> Result<()> {
        // If we have a cached session, trust it
        if self.has_cached_session() {
            self.verbosity.info(&format!("Using cached session for {}", self.user));
            return Ok(());
        }

        // Need to login - use git credential helper
        let (user, password) = self.get_credentials()?;

        self.login(&user, &password)?;
        self.user = user;
        self.save_cookies()?;

        Ok(())
    }

    /// Get credentials using git credential helper or environment
    fn get_credentials(&self) -> Result<(String, String)> {
        use std::env;
        use std::process::{Command, Stdio};

        // First check environment variable
        if let Ok(password) = env::var("DOKUWIKI_PASSWORD") {
            let user = if self.user.is_empty() {
                env::var("DOKUWIKI_USER").unwrap_or_else(|_| "admin".to_string())
            } else {
                self.user.clone()
            };
            self.verbosity.info("Using credentials from environment");
            return Ok((user, password));
        }

        // Parse host from URL
        let url: url::Url = self.rpc_url.parse()?;
        let host = url.host_str().unwrap_or("unknown");

        // Build credential request
        let mut input = format!("protocol=https\nhost={}\n", host);
        if !self.user.is_empty() {
            input.push_str(&format!("username={}\n", self.user));
        }
        input.push('\n');

        // Call git credential fill
        let mut child = Command::new("git")
            .args(["credential", "fill"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Failed to run git credential")?;

        // Write request
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(input.as_bytes())?;
        }

        let output = child.wait_with_output()?;

        if !output.status.success() {
            return Err(anyhow!(
                "git credential failed. Set DOKUWIKI_PASSWORD env var or configure git credentials for {}",
                host
            ));
        }

        // Parse response
        let response = String::from_utf8_lossy(&output.stdout);
        let mut username = String::new();
        let mut password = String::new();

        for line in response.lines() {
            if let Some(val) = line.strip_prefix("username=") {
                username = val.to_string();
            } else if let Some(val) = line.strip_prefix("password=") {
                password = val.to_string();
            }
        }

        if username.is_empty() || password.is_empty() {
            return Err(anyhow!(
                "git credential did not provide username/password. Set DOKUWIKI_PASSWORD env var."
            ));
        }

        Ok((username, password))
    }

    /// Login to the wiki
    fn login(&self, user: &str, password: &str) -> Result<()> {
        let params = vec![Value::String(user.to_string()), Value::String(password.to_string())];
        let result = self.call_inner("dokuwiki.login", &params)?;

        match result {
            Value::Bool(true) => Ok(()),
            Value::Bool(false) => Err(anyhow!("Login failed: invalid credentials")),
            _ => Err(anyhow!("Unexpected login response: {:?}", result)),
        }
    }

    /// Get list of all pages
    pub fn get_all_pages(&mut self) -> Result<Vec<PageInfo>> {
        let result = self.call("wiki.getAllPages", vec![])?;
        parse_page_list(result)
    }

    /// Get list of pages in a namespace
    pub fn get_page_list(&mut self, namespace: &str) -> Result<Vec<PageInfo>> {
        let result = self.call(
            "dokuwiki.getPagelist",
            vec![
                Value::String(namespace.to_string()),
                Value::Struct(
                    vec![("depth".to_string(), Value::Int(0))]
                        .into_iter()
                        .collect(),
                ),
            ],
        )?;
        parse_page_list(result)
    }

    /// Get recent changes since a given timestamp
    /// Returns a list of (page_id, version, author, summary) for all changes
    pub fn get_recent_changes(&mut self, since: i64) -> Result<Vec<PageVersion>> {
        let result = self.call(
            "wiki.getRecentChanges",
            vec![Value::Int(since as i32)],
        )?;

        let arr = match result {
            Value::Array(arr) => arr,
            _ => return Err(anyhow!("Expected array from getRecentChanges")),
        };

        let mut changes = Vec::new();
        for item in arr {
            if let Value::Struct(map) = item {
                let page_id = get_string(&map, "name").unwrap_or_default();
                let version = get_int(&map, "lastModified").or_else(|| get_datetime(&map, "lastModified")).unwrap_or(0);
                let author = get_string(&map, "author").unwrap_or_default();
                // getRecentChanges doesn't return summary, but we can leave it empty

                if !page_id.is_empty() {
                    changes.push(PageVersion {
                        page_id: Some(page_id),
                        version,
                        author,
                        summary: String::new(),
                        size: 0,
                    });
                }
            }
        }

        Ok(changes)
    }

    /// Get all versions of a page
    pub fn get_page_versions(&mut self, page_id: &str) -> Result<Vec<PageVersion>> {
        let result = self.call(
            "wiki.getPageVersions",
            vec![Value::String(page_id.to_string()), Value::Int(0)],
        )?;

        let arr = match result {
            Value::Array(arr) => arr,
            _ => return Err(anyhow!("Expected array from getPageVersions")),
        };

        let mut versions = Vec::new();
        for item in arr {
            if let Value::Struct(map) = item {
                let version = get_int(&map, "version").unwrap_or(0);
                // DokuWiki returns "user" not "author" in getPageVersions
                let author = get_string(&map, "user").unwrap_or_default();
                let summary = get_string(&map, "sum").unwrap_or_default();
                let size = get_int(&map, "size").unwrap_or(0);

                versions.push(PageVersion {
                    page_id: None,
                    version,
                    author,
                    summary,
                    size,
                });
            }
        }

        Ok(versions)
    }

    /// Get page content at a specific version
    pub fn get_page_version(&mut self, page_id: &str, version: i64) -> Result<String> {
        let result = self.call(
            "wiki.getPageVersion",
            vec![Value::String(page_id.to_string()), Value::Int(version as i32)],
        )?;

        match result {
            Value::String(content) => Ok(content),
            _ => Err(anyhow!("Expected string from getPageVersion")),
        }
    }

    /// Get current page content
    pub fn get_page(&mut self, page_id: &str) -> Result<String> {
        let result = self.call("wiki.getPage", vec![Value::String(page_id.to_string())])?;

        match result {
            Value::String(content) => Ok(content),
            _ => Err(anyhow!("Expected string from getPage")),
        }
    }

    /// Save page content
    pub fn put_page(&mut self, page_id: &str, content: &str, summary: &str) -> Result<()> {
        let attrs = vec![("sum".to_string(), Value::String(summary.to_string()))]
            .into_iter()
            .collect();

        let result = self.call(
            "wiki.putPage",
            vec![
                Value::String(page_id.to_string()),
                Value::String(content.to_string()),
                Value::Struct(attrs),
            ],
        )?;

        match result {
            Value::Bool(true) => Ok(()),
            _ => Err(anyhow!("Failed to save page: {:?}", result)),
        }
    }
}

/// Get the path in the repo's .git directory for storing cookies
fn get_repo_cookie_path() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .context("Failed to find .git directory")?;

    if !output.status.success() {
        return Err(anyhow!("Not in a git repository"));
    }

    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(git_dir).join("dokuwiki-cookies.json"))
}

/// Get the path to load cookies from (env var override or .git directory)
fn get_cookie_load_path() -> Result<PathBuf> {
    // Check for environment variable override (useful for cloning with existing session)
    if let Ok(cookie_path) = std::env::var("DOKUWIKI_COOKIE_FILE") {
        return Ok(PathBuf::from(cookie_path));
    }
    get_repo_cookie_path()
}


fn parse_page_list(result: Value) -> Result<Vec<PageInfo>> {
    let arr = match result {
        Value::Array(arr) => arr,
        _ => return Err(anyhow!("Expected array from page list")),
    };

    let mut pages = Vec::new();
    for item in arr {
        if let Value::Struct(map) = item {
            let id = get_string(&map, "id").unwrap_or_default();
            let revision = get_int(&map, "rev").or_else(|| get_int(&map, "version")).unwrap_or(0);
            let last_modified = get_datetime(&map, "lastModified").or_else(|| get_int(&map, "mtime")).unwrap_or(0);
            let author = get_string(&map, "author").unwrap_or_default();
            let size = get_int(&map, "size").unwrap_or(0);

            if !id.is_empty() {
                pages.push(PageInfo {
                    id,
                    revision,
                    last_modified,
                    author,
                    size,
                });
            }
        }
    }

    Ok(pages)
}

fn get_string(map: &std::collections::BTreeMap<String, Value>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_int(map: &std::collections::BTreeMap<String, Value>, key: &str) -> Option<i64> {
    match map.get(key) {
        Some(Value::Int(i)) => Some(*i as i64),
        Some(Value::Int64(i)) => Some(*i),
        _ => None,
    }
}

fn get_datetime(map: &std::collections::BTreeMap<String, Value>, key: &str) -> Option<i64> {
    match map.get(key) {
        Some(Value::Int(i)) => Some(*i as i64),
        Some(Value::Int64(i)) => Some(*i),
        Some(Value::DateTime(dt)) => {
            // iso8601::DateTime contains date and time fields
            // Extract year, month, day from date, and hour, minute, second from time
            if let iso8601::Date::YMD { year, month, day } = dt.date {
                // The formula was off by one day. Use a simpler approach.
                // Calculate days from 1970-01-01 to the given date
                let days = days_since_epoch(year, month as u32, day as u32);
                let seconds = days * 86400
                    + (dt.time.hour as i64 * 3600)
                    + (dt.time.minute as i64 * 60)
                    + (dt.time.second as i64);
                Some(seconds)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn days_since_epoch(year: i32, month: u32, day: u32) -> i64 {
    // Calculate days since Unix epoch (1970-01-01)
    // Using the formula from https://howardhinnant.github.io/date_algorithms.html
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = month as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe as i64 - 719468) as i64
}
