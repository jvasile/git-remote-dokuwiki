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

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: git-remote-dokuwiki <remote-name> <url>");
        eprintln!("This is a git remote helper and should be invoked by git.");
        std::process::exit(1);
    }

    let _remote_name = &args[1];
    let url = &args[2];
    let verbosity = Verbosity::from_env();

    let mut helper = RemoteHelper::new(url, verbosity)?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut in_import_batch = false;

    let mut lines = stdin.lock().lines();

    while let Some(line) = lines.next() {
        let line = line.context("Failed to read from stdin")?;

        match parse_command(&line) {
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
                helper.export(&mut stdout, &mut lines)?;
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
    imported: bool,
    verbosity: Verbosity,
}

impl RemoteHelper {
    fn new(url: &str, verbosity: Verbosity) -> Result<Self> {
        let (wiki_url, user, namespace) = parse_url(url)?;

        let mut client = DokuWikiClient::new(&wiki_url, &user, verbosity)?;
        client.ensure_authenticated()?;

        Ok(Self { client, namespace, imported: false, verbosity })
    }

    fn capabilities<W: Write>(&self, out: &mut W) -> Result<()> {
        writeln!(out, "import")?;
        writeln!(out, "export")?;
        writeln!(out, "option")?;
        writeln!(out, "refspec refs/heads/*:refs/remotes/origin/*")?;
        writeln!(out)?;
        Ok(())
    }

    fn set_option<W: Write>(&self, name: &str, value: &str, out: &mut W) -> Result<()> {
        match name {
            "verbosity" => {
                // git sends: no flag=1, -v=2, -vv=3
                if let Ok(level) = value.parse::<u8>() {
                    self.verbosity.set_level(level);
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
        // Use ? to indicate we don't know the SHA yet (git will figure it out from import)
        writeln!(out, "? refs/heads/main")?;
        writeln!(out)?;
        Ok(())
    }

    fn import<W: Write>(&mut self, ref_name: &str, out: &mut W) -> Result<()> {
        if self.imported {
            return Ok(());
        }

        // Check if we already have commits - get the latest timestamp and SHA
        let since_timestamp = self.get_latest_commit_timestamp();
        let parent_sha = self.get_main_sha();

        let latest_revision = fast_import::generate(&mut self.client, self.namespace.as_deref(), since_timestamp, parent_sha.as_deref(), self.verbosity, out)?;

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
            .args(["rev-parse", "refs/heads/main"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn export<W: Write, I: Iterator<Item = io::Result<String>>>(
        &mut self,
        out: &mut W,
        lines: &mut I,
    ) -> Result<()> {
        self.verbosity.info("Exporting to wiki...");

        // Read fast-export stream from the line iterator
        fast_export::process(&mut self.client, self.namespace.as_deref(), self.verbosity, lines)?;

        // Signal completion
        writeln!(out, "done")?;
        Ok(())
    }
}

/// Parse a dokuwiki URL like `dokuwiki::user@host/namespace`
fn parse_url(url: &str) -> Result<(String, String, Option<String>)> {
    // Remove dokuwiki:: prefix if present
    let url = url.strip_prefix("dokuwiki::").unwrap_or(url);

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
    let (host, namespace) = if let Some(slash_pos) = rest.find('/') {
        let host = &rest[..slash_pos];
        let ns = &rest[slash_pos + 1..];
        // Convert path to namespace (slashes to colons)
        let ns = ns.replace('/', ":");
        (host.to_string(), Some(ns))
    } else {
        (rest.to_string(), None)
    };

    // Build wiki URL
    let wiki_url = format!("https://{}", host);

    Ok((wiki_url, user, namespace))
}
