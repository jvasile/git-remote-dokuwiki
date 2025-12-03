//! Push changes to DokuWiki
//!
//! Instead of parsing git's fast-export stream (which includes full tree state),
//! we use git commands to find what actually changed and push only those files.

use anyhow::{anyhow, Error, Result};
use std::io::BufRead;
use std::process::Command;

use crate::dokuwiki::DokuWikiClient;
use crate::verbosity::Verbosity;

/// Create a detailed error message for push failures
fn push_error(failed_item: &str, error: Error, pushed: &[String], pending: &[String]) -> Error {
    let mut msg = format!("Push failed while trying to {}\nError: {}\n", failed_item, error);

    if !pushed.is_empty() {
        msg.push_str("\nSuccessfully pushed:\n");
        for item in pushed {
            msg.push_str(&format!("  - {}\n", item));
        }
    }

    if !pending.is_empty() {
        msg.push_str("\nNot yet pushed:\n");
        for item in pending {
            msg.push_str(&format!("  - {}\n", item));
        }
    }

    msg.push_str("\nThe wiki may be in an inconsistent state. ");
    msg.push_str("Fix the issue and push again to complete the update.");

    anyhow!(msg)
}

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
fn path_to_page_id(path: &str, namespace: Option<&str>, extension: &str) -> Option<String> {
    // Only handle files with the configured extension
    let suffix = format!(".{}", extension);
    let path = path.strip_suffix(&suffix)?;

    // Convert path separators to colons
    let page_id = path.replace('/', ":");

    // Prepend namespace if specified
    if let Some(ns) = namespace {
        Some(format!("{}:{}", ns, page_id))
    } else {
        Some(page_id)
    }
}

/// Check if a path is a media file (not a page with the configured extension)
fn is_media_file(path: &str, extension: &str) -> bool {
    let page_suffix = format!(".{}", extension);
    !path.ends_with(&page_suffix)
}

/// Convert a media file path back to a DokuWiki media ID
fn path_to_media_id(path: &str, namespace: Option<&str>) -> Option<String> {
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
/// Returns Some(ref) if something was pushed, None if nothing to push
pub fn process<R: BufRead>(
    client: &mut DokuWikiClient,
    namespace: Option<&str>,
    extension: &str,
    verbosity: Verbosity,
    dry_run: bool,
    reader: &mut R,
) -> Result<Option<String>> {
    // We need to consume the fast-export stream even though we won't use it directly
    // Parse it to find the ref being pushed
    // The stream contains binary blob data, so we need to handle it carefully
    let mut target_ref = String::new();
    let mut line_buf = Vec::new();

    loop {
        line_buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut line_buf)?;
        if bytes_read == 0 {
            break; // EOF
        }

        // Convert to string lossily - we only care about text commands
        let line = String::from_utf8_lossy(&line_buf);
        let line = line.trim_end();

        if line.starts_with("commit ") {
            target_ref = line[7..].to_string();
        } else if line == "done" {
            break;
        } else if line.starts_with("data ") {
            // Skip binary data - parse the length and skip that many bytes
            if let Ok(len) = line[5..].parse::<usize>() {
                let mut skip_buf = vec![0u8; len];
                reader.read_exact(&mut skip_buf)?;
            }
        }
        // Consume but ignore other lines
    }

    // If no commits in the stream, nothing to push
    if target_ref.is_empty() {
        return Ok(None);
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
        if dry_run {
            eprintln!("Nothing to push");
        } else {
            verbosity.info("No commits to push");
        }
        return Ok(None);
    }

    if dry_run {
        eprintln!("Would push {} commit(s)", commits.len());
    } else {
        verbosity.info(&format!("Pushing {} commit(s)", commits.len()));
    }

    // Track what we're pushing for error recovery
    let mut pending_items: Vec<String> = Vec::new();
    let mut pushed_items: Vec<String> = Vec::new();

    // First, collect all items to be pushed
    for commit in &commits {
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

            let item_desc = if let Some(page_id) = path_to_page_id(path, namespace, extension) {
                // It's a page file (has the configured extension)
                match status {
                    "D" => Some(format!("delete page {}", page_id)),
                    "A" | "M" => Some(format!("update page {}", page_id)),
                    _ => None,
                }
            } else if is_media_file(path, extension) {
                // It's a media file (doesn't have the page extension)
                if let Some(media_id) = path_to_media_id(path, namespace) {
                    match status {
                        "D" => Some(format!("delete media {}", media_id)),
                        "A" | "M" => Some(format!("update media {}", media_id)),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(desc) = item_desc {
                if !pending_items.contains(&desc) {
                    pending_items.push(desc);
                }
            }
        }
    }

    // Now push each item, tracking progress
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

            // Check if it's a page (has the configured extension)
            if let Some(page_id) = path_to_page_id(path, namespace, extension) {
                let item_desc = match status {
                    "D" => format!("delete page {}", page_id),
                    "A" | "M" => format!("update page {}", page_id),
                    _ => continue,
                };

                if dry_run {
                    let action = match status {
                        "D" => "Would delete",
                        "A" | "M" => "Would update",
                        _ => continue,
                    };
                    eprintln!("  {} page {}", action, page_id);
                } else {
                    let result = match status {
                        "D" => {
                            verbosity.info(&format!("  Deleting page {}...", page_id));
                            client.put_page(&page_id, "", &format!("Deleted: {}", message))
                        }
                        "A" | "M" => {
                            let content_output = Command::new("git")
                                .args(["show", &format!("{}:{}", commit, path)])
                                .output()?;

                            if content_output.status.success() {
                                let content = String::from_utf8_lossy(&content_output.stdout);
                                verbosity.info(&format!("  Updating page {}...", page_id));
                                client.put_page(&page_id, &content, &message)
                            } else {
                                continue;
                            }
                        }
                        _ => continue,
                    };

                    if let Err(e) = result {
                        return Err(push_error(&item_desc, e, &pushed_items, &pending_items));
                    }
                }

                // Move from pending to pushed
                pending_items.retain(|x| x != &item_desc);
                if !pushed_items.contains(&item_desc) {
                    pushed_items.push(item_desc);
                }
            }
            // Check if it's a media file (doesn't have the page extension)
            else if is_media_file(path, extension) {
                let Some(media_id) = path_to_media_id(path, namespace) else {
                    continue;
                };
                let item_desc = match status {
                    "D" => format!("delete media {}", media_id),
                    "A" | "M" => format!("update media {}", media_id),
                    _ => continue,
                };

                if dry_run {
                    let action = match status {
                        "D" => "Would delete",
                        "A" | "M" => "Would update",
                        _ => continue,
                    };
                    eprintln!("  {} media {}", action, media_id);
                } else {
                    let result = match status {
                        "D" => {
                            verbosity.info(&format!("  Deleting media {}...", media_id));
                            client.delete_attachment(&media_id)
                        }
                        "A" | "M" => {
                            let content_output = Command::new("git")
                                .args(["show", &format!("{}:{}", commit, path)])
                                .output()?;

                            if content_output.status.success() {
                                verbosity.info(&format!("  Updating media {}...", media_id));
                                client.put_attachment(&media_id, &content_output.stdout, true)
                            } else {
                                continue;
                            }
                        }
                        _ => continue,
                    };

                    if let Err(e) = result {
                        return Err(push_error(&item_desc, e, &pushed_items, &pending_items));
                    }
                }

                // Move from pending to pushed
                pending_items.retain(|x| x != &item_desc);
                if !pushed_items.contains(&item_desc) {
                    pushed_items.push(item_desc);
                }
            }
        }
    }

    // Update last revision timestamp to the wiki's latest timestamp
    // This ensures our own changes don't appear as "new remote changes" on next push
    // Skip this in dry-run mode since we didn't actually push anything
    if !dry_run {
        if let Ok(changes) = client.get_recent_changes(0) {
            if let Some(latest) = changes.last() {
                set_last_revision_timestamp(latest.version);
            }
        }
    }

    Ok(Some(target_ref))
}
