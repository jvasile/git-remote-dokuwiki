//! Generate git fast-import stream from DokuWiki history

use anyhow::Result;
use std::collections::HashMap;
use std::io::Write;

use crate::dokuwiki::DokuWikiClient;

/// A revision to be imported
#[derive(Debug)]
struct Revision {
    page_id: String,
    version: i64, // timestamp
    author: String,
    summary: String,
}

/// Convert a page ID to a file path
fn page_id_to_path(page_id: &str, namespace: Option<&str>) -> String {
    let mut id = page_id.to_string();

    // Strip namespace prefix if present
    if let Some(ns) = namespace {
        if let Some(stripped) = id.strip_prefix(&format!("{}:", ns)) {
            id = stripped.to_string();
        }
    }

    // Convert colons to path separators and add .txt extension
    let parts: Vec<&str> = id.split(':').collect();
    let mut path = parts.join("/");
    path.push_str(".txt");
    path
}

/// Generate fast-import stream for wiki history
/// If `since_timestamp` is provided, only generate commits newer than that timestamp
pub fn generate<W: Write>(
    client: &mut DokuWikiClient,
    namespace: Option<&str>,
    since_timestamp: Option<i64>,
    out: &mut W,
) -> Result<()> {
    eprintln!("Fetching page list...");

    // Get all pages
    let pages = if let Some(ns) = namespace {
        client.get_page_list(ns)?
    } else {
        client.get_all_pages()?
    };

    eprintln!("Found {} pages", pages.len());

    // For incremental updates, first check if any pages have been modified
    if let Some(since) = since_timestamp {
        let dominated_pages: Vec<_> = pages
            .iter()
            .filter(|p| p.revision > since)
            .collect();

        if dominated_pages.is_empty() {
            eprintln!("No pages modified since last fetch");
            eprintln!("Found 0 total revisions");
            eprintln!("No new revisions to import");
            return Ok(());
        }

        eprintln!("Found {} pages modified since last fetch", dominated_pages.len());
    }

    // Collect all revisions from all pages
    let mut all_revisions: Vec<Revision> = Vec::new();

    for page in &pages {
        // Filter by namespace if specified
        if let Some(ns) = namespace {
            if !page.id.starts_with(&format!("{}:", ns)) && page.id != ns {
                continue;
            }
        }

        // For incremental updates, skip pages that haven't been modified
        if let Some(since) = since_timestamp {
            if page.revision <= since {
                continue;
            }
        }

        eprintln!("  Fetching history for {}...", page.id);

        match client.get_page_versions(&page.id) {
            Ok(versions) => {
                for ver in versions {
                    all_revisions.push(Revision {
                        page_id: page.id.clone(),
                        version: ver.version,
                        author: if ver.author.is_empty() {
                            "unknown".to_string()
                        } else {
                            ver.author
                        },
                        summary: ver.summary,
                    });
                }
            }
            Err(e) => {
                eprintln!("    Warning: could not get history: {}", e);
                // Fall back to just current version
                all_revisions.push(Revision {
                    page_id: page.id.clone(),
                    version: page.revision,
                    author: if page.author.is_empty() {
                        "unknown".to_string()
                    } else {
                        page.author.clone()
                    },
                    summary: "current version".to_string(),
                });
            }
        }
    }

    // Sort by timestamp (oldest first)
    all_revisions.sort_by_key(|r| r.version);

    // Filter to only revisions newer than since_timestamp
    let all_revisions: Vec<Revision> = if let Some(since) = since_timestamp {
        all_revisions
            .into_iter()
            .filter(|r| r.version > since)
            .collect()
    } else {
        all_revisions
    };

    eprintln!("Found {} total revisions", all_revisions.len());

    if all_revisions.is_empty() {
        eprintln!("No new revisions to import");
        return Ok(());
    }

    eprintln!("Generating git history...");

    // Group revisions by timestamp
    let mut revisions_by_time: HashMap<i64, Vec<&Revision>> = HashMap::new();
    for rev in &all_revisions {
        revisions_by_time
            .entry(rev.version)
            .or_insert_with(Vec::new)
            .push(rev);
    }

    // Track current file contents for each path
    // This is needed because we need to output the full tree state in each commit
    let mut current_files: HashMap<String, String> = HashMap::new();

    let mut mark: u64 = 1;
    let mut last_commit_mark: Option<u64> = None;
    let mut commit_count = 0;
    let is_incremental = since_timestamp.is_some();

    let mut timestamps: Vec<i64> = revisions_by_time.keys().copied().collect();
    timestamps.sort();

    for timestamp in timestamps {
        let revs = &revisions_by_time[&timestamp];

        // Collect authors and summaries for this commit
        let mut authors: Vec<&str> = revs.iter().map(|r| r.author.as_str()).collect();
        authors.sort();
        authors.dedup();
        let author = authors.join(", ");

        let summaries: Vec<String> = revs
            .iter()
            .filter_map(|r| {
                if r.summary.is_empty() {
                    None
                } else {
                    Some(format!("{}: {}", r.page_id, r.summary))
                }
            })
            .collect();

        let message = if summaries.is_empty() {
            let page_ids: Vec<&str> = revs.iter().map(|r| r.page_id.as_str()).collect();
            if page_ids.len() == 1 {
                format!("Edit {}", page_ids[0])
            } else {
                format!("Edit {} pages", page_ids.len())
            }
        } else {
            summaries.join("\n")
        };

        // Fetch content for each file at this revision
        let mut blobs: Vec<(String, u64)> = Vec::new(); // (path, mark)

        for rev in revs {
            let path = page_id_to_path(&rev.page_id, namespace);

            match client.get_page_version(&rev.page_id, rev.version) {
                Ok(content) => {
                    if content.is_empty() {
                        // Page was deleted
                        current_files.remove(&path);
                    } else {
                        // Write blob
                        let blob_mark = mark;
                        mark += 1;

                        writeln!(out, "blob")?;
                        writeln!(out, "mark :{}", blob_mark)?;
                        writeln!(out, "data {}", content.len())?;
                        write!(out, "{}", content)?;
                        writeln!(out)?;

                        current_files.insert(path.clone(), content);
                        blobs.push((path, blob_mark));
                    }
                }
                Err(e) => {
                    eprintln!(
                        "    Warning: could not fetch {}@{}: {}",
                        rev.page_id, rev.version, e
                    );
                }
            }
        }

        if blobs.is_empty() {
            continue;
        }

        // Write commit
        let commit_mark = mark;
        mark += 1;

        // Format email from author
        let email = format!("{}@dokuwiki", author.replace(' ', ".").replace(',', ""));

        writeln!(out, "commit refs/heads/main")?;
        writeln!(out, "mark :{}", commit_mark)?;
        writeln!(out, "author {} <{}> {} +0000", author, email, timestamp)?;
        writeln!(out, "committer {} <{}> {} +0000", author, email, timestamp)?;
        writeln!(out, "data {}", message.len())?;
        write!(out, "{}", message)?;
        writeln!(out)?;

        if let Some(parent) = last_commit_mark {
            writeln!(out, "from :{}", parent)?;
        } else if is_incremental {
            // First commit in incremental update - parent is existing main
            writeln!(out, "from refs/heads/main")?;
        }

        // Write file modifications
        for (path, blob_mark) in &blobs {
            writeln!(out, "M 100644 :{} {}", blob_mark, path)?;
        }

        writeln!(out)?;

        last_commit_mark = Some(commit_mark);
        commit_count += 1;

        if commit_count % 100 == 0 {
            eprintln!("  {} commits...", commit_count);
        }
    }

    eprintln!("Generated {} commits", commit_count);

    Ok(())
}
