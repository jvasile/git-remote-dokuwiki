//! Push changes to DokuWiki
//!
//! Instead of parsing git's fast-export stream (which includes full tree state),
//! we use git commands to find what actually changed and push only those files.

use anyhow::{anyhow, Result};
use std::io;
use std::process::Command;

use crate::dokuwiki::DokuWikiClient;
use crate::verbosity::Verbosity;

/// Get the timestamp of the latest imported revision from git config
fn get_last_revision_timestamp() -> Option<i64> {
    let output = Command::new("git")
        .args(["config", "dokuwiki.lastRevision"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let timestamp_str = String::from_utf8_lossy(&output.stdout);
    timestamp_str.trim().parse().ok()
}

/// Update the last revision timestamp in git config
fn set_last_revision_timestamp(timestamp: i64) {
    let _ = Command::new("git")
        .args(["config", "dokuwiki.lastRevision", &timestamp.to_string()])
        .output();
}

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

/// Convert a media file path back to a DokuWiki media ID
fn path_to_media_id(path: &str, namespace: Option<&str>) -> Option<String> {
    // Only handle files in media/ directory
    let path = path.strip_prefix("media/")?;

    // Convert path separators to colons
    let media_id = path.replace('/', ":");

    // Prepend namespace if specified
    if let Some(ns) = namespace {
        Some(format!("{}:{}", ns, media_id))
    } else {
        Some(media_id)
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
        return Ok("refs/heads/main".to_string());
    }

    verbosity.debug(&format!("Pushing to {}", target_ref));

    // Only allow pushing to main branch - DokuWiki has no concept of branches or tags
    if target_ref.starts_with("refs/tags/") {
        return Err(anyhow!(
            "Cannot push tags. DokuWiki does not support tags."
        ));
    }
    if target_ref != "refs/heads/main" {
        return Err(anyhow!(
            "Can only push to main branch. DokuWiki does not support branches."
        ));
    }

    // Check that origin/main is an ancestor of HEAD (i.e., we've merged/rebased remote changes)
    let ancestor_check = Command::new("git")
        .args(["merge-base", "--is-ancestor", "origin/main", "HEAD"])
        .output()?;

    if !ancestor_check.status.success() {
        return Err(anyhow!(
            "Remote changes not integrated. Please merge or rebase origin/main first."
        ));
    }

    // Check for remote changes before pushing
    // Use since + 1 because getRecentChanges returns changes >= timestamp,
    // and we've already imported the change at exactly `since`
    if let Some(since) = get_last_revision_timestamp() {
        let changes = client.get_recent_changes(since + 1)?;

        // Filter by namespace if specified
        let relevant_changes: Vec<_> = if let Some(ns) = namespace {
            changes
                .into_iter()
                .filter(|c| {
                    let page_id = c.page_id.as_deref().unwrap_or("");
                    page_id.starts_with(&format!("{}:", ns)) || page_id == ns
                })
                .collect()
        } else {
            changes
        };

        if !relevant_changes.is_empty() {
            return Err(anyhow!(
                "Remote has {} new change(s). Please fetch/pull first.",
                relevant_changes.len()
            ));
        }
    }

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

            // Check if it's a page (.txt file)
            if let Some(page_id) = path_to_page_id(path, namespace) {
                match status {
                    "D" => {
                        // Delete
                        verbosity.info(&format!("  Deleting page {}...", page_id));
                        client.put_page(&page_id, "", &format!("Deleted: {}", message))?;
                    }
                    "A" | "M" => {
                        // Add or modify - get the content from git
                        let content_output = Command::new("git")
                            .args(["show", &format!("{}:{}", commit, path)])
                            .output()?;

                        if content_output.status.success() {
                            let content = String::from_utf8_lossy(&content_output.stdout);
                            verbosity.info(&format!("  Updating page {}...", page_id));
                            client.put_page(&page_id, &content, &message)?;
                        }
                    }
                    _ => {
                        verbosity.debug(&format!("  Skipping {} (status: {})", path, status));
                    }
                }
            }
            // Check if it's a media file (in media/ directory)
            else if let Some(media_id) = path_to_media_id(path, namespace) {
                match status {
                    "D" => {
                        // Delete media file
                        verbosity.info(&format!("  Deleting media {}...", media_id));
                        client.delete_attachment(&media_id)?;
                    }
                    "A" | "M" => {
                        // Add or modify - get the content from git
                        let content_output = Command::new("git")
                            .args(["show", &format!("{}:{}", commit, path)])
                            .output()?;

                        if content_output.status.success() {
                            verbosity.info(&format!("  Updating media {}...", media_id));
                            client.put_attachment(&media_id, &content_output.stdout, true)?;
                        }
                    }
                    _ => {
                        verbosity.debug(&format!("  Skipping media {} (status: {})", path, status));
                    }
                }
            }
        }
    }

    // Update last revision timestamp so future pushes don't see our changes as conflicts
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    set_last_revision_timestamp(now);

    Ok(target_ref)
}
