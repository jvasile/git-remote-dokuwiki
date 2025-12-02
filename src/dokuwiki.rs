//! DokuWiki JSON-RPC client with cookie-based authentication

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use cookie_store::CookieStore;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::verbosity::Verbosity;

/// Minimum required API version
const MIN_API_VERSION: i64 = 14;

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
    pub page_id: Option<String>,
    pub version: i64,
    pub author: String,
    pub summary: String,
    pub size: i64,
    pub revision_type: String, // "E" for edit, "D" for delete, "C" for create
}

/// Information about a media file
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub id: String,
    pub size: i64,
    pub revision: i64,
    pub author: String,
}

/// A single revision of a media file
#[derive(Debug, Clone)]
pub struct MediaVersion {
    pub version: i64,
    pub author: String,
    pub summary: String,
    pub revision_type: String,
}

/// JSON-RPC request structure
#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    method: String,
    params: Value,
    id: u64,
}

/// JSON-RPC response structure
#[derive(Deserialize)]
struct JsonRpcResponse {
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

/// DokuWiki JSON-RPC client
pub struct DokuWikiClient {
    wiki_url: String,
    rpc_url: String,
    user: String,
    client: Client,
    cookie_store: Arc<RwLock<CookieStore>>,
    cookie_path: PathBuf,
    has_loaded_cookies: bool,
    verbosity: Verbosity,
    request_id: u64,
}

impl DokuWikiClient {
    /// Create a new client for the given wiki URL
    pub fn new(wiki_url: &str, user: &str, verbosity: Verbosity) -> Result<Self> {
        let wiki_url = wiki_url.trim_end_matches('/').to_string();
        let rpc_url = format!("{}/lib/exe/jsonrpc.php", wiki_url);

        let load_path = get_cookie_load_path();

        let mut has_loaded_cookies = false;
        let cookie_store = if let Ok(ref path) = load_path {
            if path.exists() {
                if let Ok(file) = fs::File::open(path) {
                    let reader = BufReader::new(file);
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

        let cookie_path = get_repo_cookie_path().unwrap_or_else(|_| {
            load_path.unwrap_or_else(|_| PathBuf::from(".git/dokuwiki-cookies.json"))
        });

        let cookie_store = Arc::new(RwLock::new(cookie_store));

        let client = Client::builder()
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
            request_id: 1,
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

        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&store, &mut writer)
            .map_err(|e| anyhow!("Failed to save cookies: {}", e))?;

        Ok(())
    }

    /// Get cookie header for requests
    fn get_cookie_header(&self) -> String {
        let store = self.cookie_store.read().unwrap();
        let url: url::Url = self.rpc_url.parse().unwrap();
        store
            .get_request_values(&url)
            .map(|(name, value)| format!("{}={}", name, value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Store cookies from response
    fn store_cookies(&self, response: &reqwest::blocking::Response) {
        for cookie_header in response.headers().get_all(SET_COOKIE) {
            if let Ok(cookie_str) = cookie_header.to_str() {
                let url: url::Url = self.rpc_url.parse().unwrap();
                let mut store = self.cookie_store.write().unwrap();
                let _ = store.parse(cookie_str, &url);
            }
        }
    }

    /// Make a JSON-RPC call (internal, no retry)
    fn call_inner(&mut self, method: &str, params: Value) -> Result<Value> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
            id: self.request_id,
        };
        self.request_id += 1;

        let cookie_header = self.get_cookie_header();

        let mut req = self.client.post(&self.rpc_url)
            .header(CONTENT_TYPE, "application/json");
        if !cookie_header.is_empty() {
            req = req.header(COOKIE, cookie_header);
        }

        let body = serde_json::to_string(&request)?;
        let response = req
            .body(body)
            .send()
            .map_err(|e| anyhow!("HTTP request failed: {}", e))?;

        self.store_cookies(&response);

        let status = response.status();
        let body_text = response.text().map_err(|e| anyhow!("Failed to read response body: {}", e))?;

        if !status.is_success() {
            return Err(anyhow!("HTTP error {}: {}", status, body_text));
        }

        let body: JsonRpcResponse = serde_json::from_str(&body_text)
            .map_err(|e| anyhow!("JSON parse error: {} - body was: {}", e, &body_text[..body_text.len().min(200)]))?;

        if let Some(error) = body.error {
            return Err(anyhow!("API error {}: {}", error.code, error.message));
        }

        body.result.ok_or_else(|| anyhow!("No result in response"))
    }

    /// Make a JSON-RPC call with automatic re-auth on error
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        match self.call_inner(method, params.clone()) {
            Ok(value) => Ok(value),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("401") || err_str.contains("Unauthorized") || err_str.contains("not logged in") {
                    self.reauthenticate()?;
                    self.call_inner(method, params)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Re-authenticate after a session expiry
    fn reauthenticate(&mut self) -> Result<()> {
        self.verbosity.info("Session expired, re-authenticating...");

        if self.cookie_path.exists() {
            let _ = std::fs::remove_file(&self.cookie_path);
        }
        *self.cookie_store.write().unwrap() = CookieStore::new(None);

        let (user, password) = self.get_credentials()?;
        self.login(&user, &password)?;
        self.user = user;
        self.save_cookies()?;

        Ok(())
    }

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

    /// Ensure we're authenticated and API version is sufficient
    pub fn ensure_authenticated(&mut self) -> Result<()> {
        if self.has_cached_session() {
            self.verbosity.info(&format!("Using cached session for {}", self.user));
            // If cookies were loaded from env var but we're saving to .git/, copy them
            if !self.cookie_path.exists() {
                let _ = self.save_cookies();
            }
        } else {
            let (user, password) = self.get_credentials()?;
            self.login(&user, &password)?;
            self.user = user;
            self.save_cookies()?;
        }

        // Check API version
        let version = self.get_api_version()?;
        if version < MIN_API_VERSION {
            return Err(anyhow!(
                "DokuWiki API version {} is too old. Minimum required: {}. Please upgrade DokuWiki.",
                version,
                MIN_API_VERSION
            ));
        }

        self.verbosity.debug(&format!("API version: {}", version));
        Ok(())
    }

    /// Get credentials using git credential helper or environment
    fn get_credentials(&self) -> Result<(String, String)> {
        use std::env;
        use std::process::{Command, Stdio};

        if let Ok(password) = env::var("DOKUWIKI_PASSWORD") {
            let user = if self.user.is_empty() {
                env::var("DOKUWIKI_USER").unwrap_or_else(|_| "admin".to_string())
            } else {
                self.user.clone()
            };
            self.verbosity.info("Using credentials from environment");
            return Ok((user, password));
        }

        let url: url::Url = self.rpc_url.parse()?;
        let host = url.host_str().unwrap_or("unknown");

        let mut input = format!("protocol=https\nhost={}\n", host);
        if !self.user.is_empty() {
            input.push_str(&format!("username={}\n", self.user));
        }
        input.push('\n');

        let mut child = Command::new("git")
            .args(["credential", "fill"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Failed to run git credential")?;

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
    fn login(&mut self, user: &str, password: &str) -> Result<()> {
        let result = self.call_inner("core.login", json!({
            "user": user,
            "pass": password
        }))?;

        match result.as_bool() {
            Some(true) => Ok(()),
            Some(false) => Err(anyhow!("Login failed: invalid credentials")),
            _ => Err(anyhow!("Unexpected login response: {:?}", result)),
        }
    }

    /// Get API version
    fn get_api_version(&mut self) -> Result<i64> {
        let result = self.call("core.getAPIVersion", json!({}))?;
        result.as_i64().ok_or_else(|| anyhow!("Invalid API version response"))
    }

    /// Get list of all pages (recursively, all namespaces)
    pub fn get_all_pages(&mut self) -> Result<Vec<PageInfo>> {
        let result = self.call("dokuwiki.getPagelist", json!({
            "ns": "",
            "opts": { "depth": 0 }
        }))?;
        parse_page_list(&result)
    }

    /// Get list of pages in a namespace
    pub fn get_page_list(&mut self, namespace: &str) -> Result<Vec<PageInfo>> {
        let result = self.call("dokuwiki.getPagelist", json!({
            "ns": namespace,
            "opts": { "depth": 0 }
        }))?;
        parse_page_list(&result)
    }

    /// Get recent page changes since a given timestamp
    pub fn get_recent_changes(&mut self, since: i64) -> Result<Vec<PageVersion>> {
        let result = self.call("core.getRecentPageChanges", json!({
            "timestamp": since
        }))?;

        let arr = result.as_array().ok_or_else(|| anyhow!("Expected array"))?;

        let mut changes = Vec::new();
        for item in arr {
            let page_id = item["id"].as_str().unwrap_or_default().to_string();
            let version = item["revision"].as_i64().unwrap_or(0);
            let author = item["author"].as_str().unwrap_or_default().to_string();
            let summary = item["summary"].as_str().unwrap_or_default().to_string();
            let revision_type = item["type"].as_str().unwrap_or("E").to_string();

            if !page_id.is_empty() {
                changes.push(PageVersion {
                    page_id: Some(page_id),
                    version,
                    author,
                    summary,
                    size: 0,
                    revision_type,
                });
            }
        }

        Ok(changes)
    }

    /// Get all versions of a page
    pub fn get_page_versions(&mut self, page_id: &str) -> Result<Vec<PageVersion>> {
        let result = self.call("core.getPageHistory", json!({
            "page": page_id
        }))?;

        let arr = result.as_array().ok_or_else(|| anyhow!("Expected array"))?;

        let mut versions = Vec::new();
        for item in arr {
            let version = item["revision"].as_i64().unwrap_or(0);
            let author = item["author"].as_str().unwrap_or_default().to_string();
            let summary = item["summary"].as_str().unwrap_or_default().to_string();
            let size = item["sizechange"].as_i64().unwrap_or(0);
            let revision_type = item["type"].as_str().unwrap_or("E").to_string();

            versions.push(PageVersion {
                page_id: None,
                version,
                author,
                summary,
                size,
                revision_type,
            });
        }

        Ok(versions)
    }

    /// Get page content at a specific version
    pub fn get_page_version(&mut self, page_id: &str, version: i64) -> Result<String> {
        let result = self.call("core.getPage", json!({
            "page": page_id,
            "rev": version
        }))?;

        result.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("Expected string from getPage"))
    }

    /// Get current page content
    pub fn get_page(&mut self, page_id: &str) -> Result<String> {
        let result = self.call("core.getPage", json!({ "page": page_id }))?;

        result.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("Expected string from getPage"))
    }

    /// Save page content
    pub fn put_page(&mut self, page_id: &str, content: &str, summary: &str) -> Result<()> {
        self.call("core.savePage", json!({
            "page": page_id,
            "text": content,
            "summary": summary
        }))?;
        Ok(())
    }

    /// Get list of all media files in a namespace
    pub fn get_attachments(&mut self, namespace: &str) -> Result<Vec<MediaInfo>> {
        let result = self.call("core.listMedia", json!({
            "namespace": namespace,
            "depth": 0  // 0 = unlimited depth, list all media recursively
        }))?;

        let arr = result.as_array().ok_or_else(|| anyhow!("Expected array"))?;

        let mut media = Vec::new();
        for item in arr {
            let id = item["id"].as_str().unwrap_or_default().to_string();
            let size = item["size"].as_i64().unwrap_or(0);
            let revision = item["rev"].as_i64().unwrap_or(0);
            let author = item["user"].as_str().unwrap_or_default().to_string();

            if !id.is_empty() {
                media.push(MediaInfo {
                    id,
                    size,
                    revision,
                    author,
                });
            }
        }

        Ok(media)
    }

    /// Get recent media changes since a given timestamp
    pub fn get_recent_media_changes(&mut self, since: i64) -> Result<Vec<MediaInfo>> {
        let result = self.call("core.getRecentMediaChanges", json!({
            "timestamp": since
        }))?;

        let arr = result.as_array().ok_or_else(|| anyhow!("Expected array"))?;

        let mut media = Vec::new();
        for item in arr {
            let id = item["id"].as_str().unwrap_or_default().to_string();
            let size = item["size"].as_i64().unwrap_or(0);
            let revision = item["revision"].as_i64().unwrap_or(0);
            let author = item["author"].as_str().unwrap_or_default().to_string();

            if !id.is_empty() {
                media.push(MediaInfo {
                    id,
                    size,
                    revision,
                    author,
                });
            }
        }

        Ok(media)
    }

    /// Get all versions of a media file
    pub fn get_media_versions(&mut self, media_id: &str) -> Result<Vec<MediaVersion>> {
        let result = self.call("core.getMediaHistory", json!({
            "media": media_id
        }))?;

        let arr = result.as_array().ok_or_else(|| anyhow!("Expected array"))?;

        let mut versions = Vec::new();
        for item in arr {
            let version = item["revision"].as_i64().unwrap_or(0);
            let author = item["author"].as_str().unwrap_or_default().to_string();
            let summary = item["summary"].as_str().unwrap_or_default().to_string();
            let revision_type = item["type"].as_str().unwrap_or("E").to_string();

            versions.push(MediaVersion {
                version,
                author,
                summary,
                revision_type,
            });
        }

        Ok(versions)
    }

    /// Get media file content (current version)
    pub fn get_attachment(&mut self, media_id: &str) -> Result<Vec<u8>> {
        let result = self.call("core.getMedia", json!({ "media": media_id }))?;

        let base64_data = result.as_str()
            .ok_or_else(|| anyhow!("Expected base64 string from getMedia"))?;

        BASE64.decode(base64_data)
            .map_err(|e| anyhow!("Failed to decode base64: {}", e))
    }

    /// Get media file content at a specific version
    pub fn get_attachment_version(&mut self, media_id: &str, version: i64) -> Result<Vec<u8>> {
        let result = self.call("core.getMedia", json!({
            "media": media_id,
            "rev": version
        }))?;

        let base64_data = result.as_str()
            .ok_or_else(|| anyhow!("Expected base64 string from getMedia"))?;

        BASE64.decode(base64_data)
            .map_err(|e| anyhow!("Failed to decode base64: {}", e))
    }

    /// Save media file
    pub fn put_attachment(&mut self, media_id: &str, data: &[u8], overwrite: bool) -> Result<()> {
        let base64_data = BASE64.encode(data);

        self.call("core.saveMedia", json!({
            "media": media_id,
            "base64": base64_data,
            "overwrite": overwrite
        }))?;
        Ok(())
    }

    /// Delete media file
    pub fn delete_attachment(&mut self, media_id: &str) -> Result<()> {
        self.call("core.deleteMedia", json!({ "media": media_id }))?;
        Ok(())
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
    if let Ok(cookie_path) = std::env::var("DOKUWIKI_COOKIE_FILE") {
        return Ok(PathBuf::from(cookie_path));
    }
    get_repo_cookie_path()
}

fn parse_page_list(result: &Value) -> Result<Vec<PageInfo>> {
    let arr = result.as_array().ok_or_else(|| anyhow!("Expected array"))?;

    let mut pages = Vec::new();
    for item in arr {
        let id = item["id"].as_str().unwrap_or_default().to_string();
        let revision = item["rev"].as_i64().unwrap_or(0);
        let last_modified = item["mtime"].as_i64().unwrap_or(0);
        let author = item["user"].as_str().unwrap_or_default().to_string();
        let size = item["size"].as_i64().unwrap_or(0);

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

    Ok(pages)
}
