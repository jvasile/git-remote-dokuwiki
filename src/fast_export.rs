//! Parse git fast-export stream and push changes to DokuWiki

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::io;

use crate::dokuwiki::DokuWikiClient;
use crate::verbosity::Verbosity;

/// A file modification from the fast-export stream
#[derive(Debug)]
struct FileChange {
    path: String,
    content: Option<String>, // None means delete
}

/// A commit from the fast-export stream
#[derive(Debug)]
struct ExportCommit {
    message: String,
    files: Vec<FileChange>,
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

/// Parse and process a fast-export stream
pub fn process<I: Iterator<Item = io::Result<String>>>(
    client: &mut DokuWikiClient,
    namespace: Option<&str>,
    verbosity: Verbosity,
    lines: &mut I,
) -> Result<()> {

    let mut blobs: HashMap<String, String> = HashMap::new(); // mark -> content
    let mut commits: Vec<ExportCommit> = Vec::new();

    while let Some(line) = lines.next() {
        let line = line?;

        if line.starts_with("blob") {
            // Read blob
            let mark = parse_mark(lines)?;
            let content = read_data(lines)?;
            blobs.insert(mark, content);
        } else if line.starts_with("commit ") {
            // Read commit
            let commit = parse_commit(lines, &blobs)?;
            commits.push(commit);
        } else if line == "done" {
            break;
        }
        // Skip other lines (reset, tag, etc.)
    }

    verbosity.info(&format!("Parsed {} commits to push", commits.len()));

    // Push each commit's changes to the wiki
    for commit in commits {
        for file in commit.files {
            if let Some(page_id) = path_to_page_id(&file.path, namespace) {
                match file.content {
                    Some(content) => {
                        verbosity.info(&format!("  Updating {}...", page_id));
                        client.put_page(&page_id, &content, &commit.message)?;
                    }
                    None => {
                        // Delete = save empty content
                        verbosity.info(&format!("  Deleting {}...", page_id));
                        client.put_page(&page_id, "", &format!("Deleted: {}", commit.message))?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn parse_mark<I: Iterator<Item = io::Result<String>>>(lines: &mut I) -> Result<String> {
    while let Some(line) = lines.next() {
        let line = line?;
        if line.starts_with("mark :") {
            return Ok(line[6..].to_string());
        }
        if line.starts_with("data ") {
            // No mark, generate a placeholder
            // But we need to consume the data first
            let size: usize = line[5..].parse()?;
            let mut buf = vec![0u8; size];
            // This is tricky with line iterators...
            // For now, return error
            return Err(anyhow!("Blob without mark not supported"));
        }
    }
    Err(anyhow!("Unexpected end of stream looking for mark"))
}

fn read_data<I: Iterator<Item = io::Result<String>>>(lines: &mut I) -> Result<String> {
    while let Some(line) = lines.next() {
        let line = line?;
        if line.starts_with("data ") {
            let size: usize = line[5..].parse()?;

            // Read exactly `size` bytes
            // Since we're using line iterator, we need to accumulate lines
            let mut content = String::new();
            let mut remaining = size;

            while remaining > 0 {
                if let Some(next_line) = lines.next() {
                    let next_line = next_line?;
                    if content.is_empty() {
                        content = next_line;
                    } else {
                        content.push('\n');
                        content.push_str(&next_line);
                    }
                    // Account for the newline that was stripped
                    remaining = remaining.saturating_sub(content.len() + 1);
                } else {
                    break;
                }
            }

            // Trim to exact size
            if content.len() > size {
                content.truncate(size);
            }

            return Ok(content);
        }
    }
    Err(anyhow!("Unexpected end of stream looking for data"))
}

fn parse_commit<I: Iterator<Item = io::Result<String>>>(
    lines: &mut I,
    blobs: &HashMap<String, String>,
) -> Result<ExportCommit> {
    let mut message = String::new();
    let mut files: Vec<FileChange> = Vec::new();

    while let Some(line) = lines.next() {
        let line = line?;

        if line.is_empty() {
            // End of commit
            break;
        } else if line.starts_with("mark ") {
            // Skip commit mark
        } else if line.starts_with("author ") || line.starts_with("committer ") {
            // Skip author/committer lines
        } else if line.starts_with("from ") || line.starts_with("merge ") {
            // Skip parent refs
        } else if line.starts_with("data ") {
            let size: usize = line[5..].parse()?;

            // Read commit message
            let mut msg = String::new();
            let mut remaining = size;

            while remaining > 0 {
                if let Some(next_line) = lines.next() {
                    let next_line = next_line?;
                    if msg.is_empty() {
                        msg = next_line;
                    } else {
                        msg.push('\n');
                        msg.push_str(&next_line);
                    }
                    remaining = remaining.saturating_sub(msg.len() + 1);
                } else {
                    break;
                }
            }

            if msg.len() > size {
                msg.truncate(size);
            }

            message = msg;
        } else if line.starts_with("M ") {
            // File modification: M <mode> <dataref> <path>
            let parts: Vec<&str> = line.splitn(4, ' ').collect();
            if parts.len() >= 4 {
                let dataref = parts[2];
                let path = parts[3].to_string();

                let content = if dataref.starts_with(':') {
                    // Reference to a blob by mark
                    blobs.get(&dataref[1..]).cloned()
                } else {
                    // Inline data or SHA - not supported yet
                    None
                };

                if let Some(content) = content {
                    files.push(FileChange {
                        path,
                        content: Some(content),
                    });
                }
            }
        } else if line.starts_with("D ") {
            // File deletion: D <path>
            let path = line[2..].to_string();
            files.push(FileChange {
                path,
                content: None,
            });
        }
    }

    Ok(ExportCommit { message, files })
}
