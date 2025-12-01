//! Push changes to DokuWiki
//!
//! Instead of parsing git's fast-export stream (which includes full tree state),
//! we use git commands to find what actually changed and push only those files.

use anyhow::{anyhow, Result};
use std::io;
use std::process::Command;

use crate::dokuwiki::DokuWikiClient;
use crate::verbosity::Verbosity;

/// Convert a file path back to a DokuWiki page ID
fn path_to_page_id(path: &str, namespace: Option<&str>) -> Option<String> {
    // Only handle .txt files
    let path = path.strip_suffix(".txt")?;

    // Convert path separators to colons
    let page_id = path.replace('/', ":");

    // Prepend namespace if specified
    if let Some(ns) = namespace {
        Some(format!("{}:{}", ns, page_id))
    } else {
        Some(page_id)
    }
}

/// Process push by finding changed files and updating the wiki
/// Consumes the fast-export stream but uses git diff to find actual changes
pub fn process<I: Iterator<Item = io::Result<String>>>(
    client: &mut DokuWikiClient,
    namespace: Option<&str>,
    verbosity: Verbosity,
    lines: &mut I,
) -> Result<String> {
    // We need to consume the fast-export stream even though we won't use it directly
    // Parse it to find the ref being pushed
    let mut target_ref = String::new();

    for line in lines {
        let line = line?;
        if line.starts_with("commit ") {
            target_ref = line[7..].to_string();
        } else if line == "done" {
            break;
        }
        // Consume but ignore other lines
    }

    // If no commits in the stream, nothing to push - that's OK
    if target_ref.is_empty() {
        return Ok("refs/dokuwiki/origin/heads/main".to_string());
    }

    verbosity.debug(&format!("Pushing to {}", target_ref));

    // Find what commits we're pushing: commits on HEAD not on origin/main
    let output = Command::new("git")
        .args(["rev-list", "--reverse", "origin/main..HEAD"])
        .output()?;

    if !output.status.success() {
        return Err(anyhow!("Failed to get commit list"));
    }

    let commits: Vec<&str> = std::str::from_utf8(&output.stdout)?
        .lines()
        .collect();

    if commits.is_empty() {
        verbosity.info("No commits to push");
        return Ok(target_ref);
    }

    verbosity.info(&format!("Pushing {} commit(s)", commits.len()));

    for commit in &commits {
        // Get commit message
        let msg_output = Command::new("git")
            .args(["log", "-1", "--format=%s", commit])
            .output()?;
        let message = std::str::from_utf8(&msg_output.stdout)?.trim().to_string();

        // Get changed files in this commit
        let diff_output = Command::new("git")
            .args(["diff-tree", "--no-commit-id", "--name-status", "-r", commit])
            .output()?;

        if !diff_output.status.success() {
            return Err(anyhow!("Failed to get diff for commit {}", commit));
        }

        let changes = std::str::from_utf8(&diff_output.stdout)?;

        for line in changes.lines() {
            let parts: Vec<&str> = line.splitn(2, '\t').collect();
            if parts.len() != 2 {
                continue;
            }

            let status = parts[0];
            let path = parts[1];

            if let Some(page_id) = path_to_page_id(path, namespace) {
                match status {
                    "D" => {
                        // Delete
                        verbosity.info(&format!("  Deleting {}...", page_id));
                        client.put_page(&page_id, "", &format!("Deleted: {}", message))?;
                    }
                    "A" | "M" => {
                        // Add or modify - get the content from git
                        let content_output = Command::new("git")
                            .args(["show", &format!("{}:{}", commit, path)])
                            .output()?;

                        if content_output.status.success() {
                            let content = String::from_utf8_lossy(&content_output.stdout);
                            verbosity.info(&format!("  Updating {}...", page_id));
                            client.put_page(&page_id, &content, &message)?;
                        }
                    }
                    _ => {
                        verbosity.debug(&format!("  Skipping {} (status: {})", path, status));
                    }
                }
            }
        }
    }

    Ok(target_ref)
}
