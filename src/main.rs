//! Git remote helper for DokuWiki
//!
//! This allows using git to interact with a DokuWiki:
//! ```
//! git clone dokuwiki::user@wiki.example.com
//! git push origin main
//! git pull
//! ```

mod dokuwiki;
mod fast_export;
mod fast_import;
mod protocol;
mod verbosity;

use verbosity::Verbosity;

use anyhow::{Context, Result};
use std::env;
use std::io::{self, BufRead, Write};
use std::process::Command as ProcessCommand;

use crate::dokuwiki::DokuWikiClient;
use crate::protocol::{parse_command, Command};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    // Handle --version flag
    if args.len() == 2 && (args[1] == "--version" || args[1] == "-V") {
        println!("git-remote-dokuwiki {}", VERSION);
        return Ok(());
    }

    if args.len() < 3 {
        eprintln!("git-remote-dokuwiki {}", VERSION);
        eprintln!();
        eprintln!("Usage: git-remote-dokuwiki <remote-name> <url>");
        eprintln!("This is a git remote helper and should be invoked by git.");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  git clone dokuwiki::user@wiki.example.com");
        eprintln!("  git clone dokuwiki::user@wiki.example.com/namespace");
        std::process::exit(1);
    }

    let _remote_name = &args[1];
    let url = &args[2];
    let verbosity = Verbosity::from_env();

    let mut helper = RemoteHelper::new(url, verbosity)?;

    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = io::stdout();
    let mut in_import_batch = false;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = stdin.read_line(&mut line).context("Failed to read from stdin")?;
        if bytes_read == 0 {
            break; // EOF
        }
        let line = line.trim_end();

        match parse_command(line) {
            Command::Capabilities => {
                helper.capabilities(&mut stdout)?;
            }
            Command::List => {
                helper.list(&mut stdout)?;
            }
            Command::Option { name, value } => {
                helper.set_option(&name, &value, &mut stdout)?;
            }
            Command::Import(ref_name) => {
                in_import_batch = true;
                helper.import(&ref_name, &mut stdout)?;
            }
            Command::Export => {
                // Export reads the fast-export stream directly from the remaining stdin
                helper.export(&mut stdout, &mut stdin)?;
                return Ok(());
            }
            Command::Empty => {
                // Empty line ends a batch
                if in_import_batch {
                    // Import batch complete, exit cleanly
                    return Ok(());
                }
                // Continue to next batch
            }
            Command::Unknown(cmd) => {
                eprintln!("Unknown command: {}", cmd);
            }
        }
        stdout.flush()?;
    }

    Ok(())
}

struct RemoteHelper {
    client: DokuWikiClient,
    namespace: Option<String>,
    extension: String,
    imported: bool,
    verbosity: Verbosity,
    depth: Option<u32>,
}

impl RemoteHelper {
    fn new(url: &str, verbosity: Verbosity) -> Result<Self> {
        let (wiki_url, user, namespace, extension) = parse_url(url)?;

        let mut client = DokuWikiClient::new(&wiki_url, &user, verbosity)?;
        client.ensure_authenticated()?;

        Ok(Self { client, namespace, extension, imported: false, verbosity, depth: None })
    }

    fn capabilities<W: Write>(&self, out: &mut W) -> Result<()> {
        writeln!(out, "import")?;
        writeln!(out, "export")?;
        writeln!(out, "option")?;
        writeln!(out, "refspec refs/heads/*:refs/dokuwiki/origin/heads/*")?;
        writeln!(out)?;
        Ok(())
    }

    fn set_option(&mut self, name: &str, value: &str, out: &mut impl Write) -> Result<()> {
        match name {
            "verbosity" => {
                // git sends: no flag=1, -v=2, -vv=3
                if let Ok(level) = value.parse::<u8>() {
                    self.verbosity.set_level(level);
                }
                writeln!(out, "ok")?;
            }
            "depth" => {
                if let Ok(d) = value.parse::<u32>() {
                    self.depth = Some(d);
                }
                writeln!(out, "ok")?;
            }
            _ => {
                // Unsupported option
                writeln!(out, "unsupported")?;
            }
        }
        Ok(())
    }

    fn list<W: Write>(&mut self, out: &mut W) -> Result<()> {
        // DokuWiki doesn't have branches, we simulate a single 'main' branch
        // Check if there are new changes on the wiki since our last fetch
        let has_new_changes = if let Some(since) = self.get_latest_commit_timestamp() {
            // Check for changes newer than our last known revision
            self.client.get_recent_changes(since + 1)
                .map(|changes| !changes.is_empty())
                .unwrap_or(false)
        } else {
            true // No previous fetch, need to import
        };

        if has_new_changes {
            // Return ? to force git to call import
            writeln!(out, "@refs/heads/main HEAD")?;
            writeln!(out, "? refs/heads/main")?;
        } else if let Some(sha) = self.get_main_sha() {
            // No changes, return the current SHA
            writeln!(out, "{} refs/heads/main", sha)?;
        } else {
            // No SHA yet, need to import
            writeln!(out, "@refs/heads/main HEAD")?;
            writeln!(out, "? refs/heads/main")?;
        }
        writeln!(out)?;
        Ok(())
    }

    fn import<W: Write>(&mut self, _ref_name: &str, out: &mut W) -> Result<()> {
        if self.imported {
            return Ok(());
        }

        // Check if we already have commits - get the latest timestamp and SHA
        let since_timestamp = self.get_latest_commit_timestamp();
        let parent_sha = self.get_main_sha();

        let wiki_host = self.client.wiki_host().to_string();
        let latest_revision = fast_import::generate(&mut self.client, self.namespace.as_deref(), since_timestamp, parent_sha.as_deref(), &wiki_host, &self.extension, self.depth, self.verbosity, out)?;

        // Store the latest revision timestamp for future incremental fetches
        if let Some(ts) = latest_revision {
            self.set_latest_revision_timestamp(ts);
        }

        self.imported = true;
        // Note: 'done' is written after all import commands are processed
        Ok(())
    }

    /// Get the timestamp of the latest imported revision
    /// We store this in git config since the wiki's lastModified field is unreliable
    fn get_latest_commit_timestamp(&self) -> Option<i64> {
        let output = ProcessCommand::new("git")
            .args(["config", "dokuwiki.lastRevision"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let timestamp_str = String::from_utf8_lossy(&output.stdout);
        timestamp_str.trim().parse().ok()
    }

    /// Store the timestamp of the latest imported revision
    fn set_latest_revision_timestamp(&self, timestamp: i64) {
        let _ = ProcessCommand::new("git")
            .args(["config", "dokuwiki.lastRevision", &timestamp.to_string()])
            .output();
    }

    /// Get the SHA of the current main branch tip, if any
    fn get_main_sha(&self) -> Option<String> {
        let output = ProcessCommand::new("git")
            .args(["rev-parse", "refs/dokuwiki/origin/heads/main"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn export<W: Write, R: io::BufRead>(
        &mut self,
        out: &mut W,
        reader: &mut R,
    ) -> Result<()> {
        self.verbosity.info("Exporting to wiki...");

        // Process the push and get the ref that was pushed
        let pushed_ref = fast_export::process(&mut self.client, self.namespace.as_deref(), &self.extension, self.verbosity, reader)?;

        // Tell git the push succeeded
        writeln!(out, "ok {}", pushed_ref)?;
        writeln!(out)?;
        Ok(())
    }
}

/// Default file extension for wiki pages
const DEFAULT_EXTENSION: &str = "md";

/// Parse a dokuwiki URL like `dokuwiki::user@host/namespace?ext=txt`
/// Returns (wiki_url, user, namespace, extension)
fn parse_url(url: &str) -> Result<(String, String, Option<String>, String)> {
    // Remove dokuwiki:: prefix if present
    let url = url.strip_prefix("dokuwiki::").unwrap_or(url);

    // Extract query parameters (e.g., ?ext=txt)
    let (url, extension) = if let Some(query_pos) = url.find('?') {
        let query = &url[query_pos + 1..];
        let url = &url[..query_pos];

        // Parse ext parameter
        let ext = query
            .split('&')
            .find_map(|param| {
                param.strip_prefix("ext=")
            })
            .unwrap_or(DEFAULT_EXTENSION);

        (url, ext.to_string())
    } else {
        (url, DEFAULT_EXTENSION.to_string())
    };

    // Parse user@host/path
    let (user, rest) = if let Some(at_pos) = url.find('@') {
        let user = &url[..at_pos];
        let rest = &url[at_pos + 1..];
        (user.to_string(), rest)
    } else {
        // No user in URL, will prompt later
        (String::new(), url)
    };

    // Split host and namespace path
    let (host, namespace) = if let Some(sep_pos) = rest.find('/') {
        let host = &rest[..sep_pos];
        let ns = &rest[sep_pos + 1..];
        // Convert path to namespace (slashes to colons)
        let ns = ns.replace('/', ":");
        (host.to_string(), Some(ns))
    } else {
        (rest.to_string(), None)
    };

    // Build wiki URL - use HTTP for localhost, HTTPS otherwise
    let protocol = if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
        "http"
    } else {
        "https"
    };
    let wiki_url = format!("{}://{}", protocol, host);

    Ok((wiki_url, user, namespace, extension))
}
