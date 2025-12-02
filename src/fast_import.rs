//! Generate git fast-import stream from DokuWiki history

use anyhow::Result;
use std::collections::HashMap;
use std::io::Write;

use crate::dokuwiki::DokuWikiClient;
use crate::verbosity::Verbosity;

/// A revision to be imported (page or media)
#[derive(Debug)]
struct Revision {
    id: String,        // page_id or media_id
    version: i64,      // timestamp
    author: String,
    summary: String,
    revision_type: String, // "E" for edit, "D" for delete, "C" for create
    is_media: bool,
}

/// Convert a page ID to a file path
fn page_id_to_path(page_id: &str, namespace: Option<&str>, extension: &str) -> String {
    let mut id = page_id.to_string();

    // Strip namespace prefix if present
    if let Some(ns) = namespace {
        if let Some(stripped) = id.strip_prefix(&format!("{}:", ns)) {
            id = stripped.to_string();
        }
    }

    // Convert colons to path separators and add extension
    let parts: Vec<&str> = id.split(':').collect();
    let mut path = parts.join("/");
    path.push('.');
    path.push_str(extension);
    path
}

/// Convert a media ID to a file path (alongside pages, no media/ prefix)
fn media_id_to_path(media_id: &str, namespace: Option<&str>) -> String {
    let mut id = media_id.to_string();

    // Strip namespace prefix if present
    if let Some(ns) = namespace {
        if let Some(stripped) = id.strip_prefix(&format!("{}:", ns)) {
            id = stripped.to_string();
        }
    }

    // Convert colons to path separators
    let parts: Vec<&str> = id.split(':').collect();
    parts.join("/")
}

/// Generate fast-import stream for wiki history
/// If `since_timestamp` is provided, only generate commits newer than that timestamp
/// If `parent_sha` is provided, use it as the parent for the first incremental commit
/// If `depth` is provided, limit the number of revisions per page/media
/// Returns the latest revision timestamp that was imported, if any
pub fn generate<W: Write>(
    client: &mut DokuWikiClient,
    namespace: Option<&str>,
    since_timestamp: Option<i64>,
    parent_sha: Option<&str>,
    wiki_host: &str,
    extension: &str,
    depth: Option<u32>,
    verbosity: Verbosity,
    out: &mut W,
) -> Result<Option<i64>> {
    let mut all_revisions: Vec<Revision> = Vec::new();

    // For incremental fetches, use getRecentChanges to find changed items,
    // then get history for summaries and types
    // For full fetches, enumerate all items and their versions
    if let Some(since) = since_timestamp {
        verbosity.info("Checking for recent changes...");
        verbosity.debug(&format!("Looking for revisions > {}", since));

        // Get recent page changes
        let page_changes = client.get_recent_changes(since)?;

        if !page_changes.is_empty() {
            verbosity.info(&format!("Found {} recent page changes", page_changes.len()));
        }

        // Collect unique page IDs
        let mut page_ids: Vec<String> = page_changes
            .iter()
            .filter_map(|c| c.page_id.clone())
            .filter(|page_id| {
                if let Some(ns) = namespace {
                    page_id.starts_with(&format!("{}:", ns)) || page_id == ns
                } else {
                    true
                }
            })
            .collect();
        page_ids.sort();
        page_ids.dedup();

        // For each page, get versions
        for page_id in page_ids {
            verbosity.debug(&format!("  Fetching history for {}...", page_id));

            match client.get_page_versions(&page_id) {
                Ok(versions) => {
                    for ver in versions {
                        if ver.version > since {
                            all_revisions.push(Revision {
                                id: page_id.clone(),
                                version: ver.version,
                                author: if ver.author.is_empty() { "unknown".to_string() } else { ver.author },
                                summary: ver.summary,
                                revision_type: ver.revision_type,
                                is_media: false,
                            });
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Warning: could not get history for {}: {}", page_id, e);
                }
            }
        }

        // Get recent media changes
        let media_changes = client.get_recent_media_changes(since)?;

        if !media_changes.is_empty() {
            verbosity.info(&format!("Found {} recent media changes", media_changes.len()));
        }

        // Collect unique media IDs
        let mut media_ids: Vec<String> = media_changes
            .iter()
            .map(|m| m.id.clone())
            .filter(|media_id| {
                if let Some(ns) = namespace {
                    media_id.starts_with(&format!("{}:", ns))
                } else {
                    true
                }
            })
            .collect();
        media_ids.sort();
        media_ids.dedup();

        // For each media, get versions
        for media_id in media_ids {
            verbosity.debug(&format!("  Fetching media history for {}...", media_id));

            match client.get_media_versions(&media_id) {
                Ok(versions) => {
                    for ver in versions {
                        if ver.version > since {
                            all_revisions.push(Revision {
                                id: media_id.clone(),
                                version: ver.version,
                                author: if ver.author.is_empty() { "unknown".to_string() } else { ver.author },
                                summary: ver.summary,
                                revision_type: ver.revision_type,
                                is_media: true,
                            });
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Warning: could not get media history for {}: {}", media_id, e);
                }
            }
        }

        if all_revisions.is_empty() {
            verbosity.info("No changes since last fetch");
            return Ok(None);
        }
    } else {
        // Full fetch - get all pages and media with their complete history
        verbosity.info("Fetching page list...");

        let pages = if let Some(ns) = namespace {
            client.get_page_list(ns)?
        } else {
            client.get_all_pages()?
        };

        verbosity.info(&format!("Found {} pages", pages.len()));

        for page in &pages {
            if let Some(ns) = namespace {
                if !page.id.starts_with(&format!("{}:", ns)) && page.id != ns {
                    continue;
                }
            }

            verbosity.debug(&format!("  Fetching history for {}...", page.id));

            match client.get_page_versions(&page.id) {
                Ok(versions) => {
                    if !versions.is_empty() {
                        verbosity.debug(&format!("    {} has {} versions", page.id, versions.len()));
                    }
                    for ver in versions {
                        all_revisions.push(Revision {
                            id: page.id.clone(),
                            version: ver.version,
                            author: if ver.author.is_empty() { "unknown".to_string() } else { ver.author },
                            summary: ver.summary,
                            revision_type: ver.revision_type,
                            is_media: false,
                        });
                    }
                }
                Err(e) => {
                    eprintln!("Warning: could not get history for {}: {}", page.id, e);
                    all_revisions.push(Revision {
                        id: page.id.clone(),
                        version: page.revision,
                        author: if page.author.is_empty() { "unknown".to_string() } else { page.author.clone() },
                        summary: "current version".to_string(),
                        revision_type: "E".to_string(),
                        is_media: false,
                    });
                }
            }
        }

        // Fetch media files
        verbosity.info("Fetching media list...");
        let media_files = client.get_attachments(namespace.unwrap_or(""))?;

        // Filter by namespace
        let media_files: Vec<_> = if let Some(ns) = namespace {
            media_files.into_iter().filter(|m| m.id.starts_with(&format!("{}:", ns))).collect()
        } else {
            media_files
        };

        verbosity.info(&format!("Found {} media files", media_files.len()));

        for media in &media_files {
            verbosity.debug(&format!("  Fetching media history for {}...", media.id));

            match client.get_media_versions(&media.id) {
                Ok(versions) => {
                    if versions.is_empty() {
                        // Media exists but has no history (e.g., system files)
                        // Fall back to current version using the revision from listMedia
                        all_revisions.push(Revision {
                            id: media.id.clone(),
                            version: media.revision,
                            author: if media.author.is_empty() { "unknown".to_string() } else { media.author.clone() },
                            summary: "current version".to_string(),
                            revision_type: "C".to_string(),
                            is_media: true,
                        });
                    } else {
                        verbosity.debug(&format!("    {} has {} versions", media.id, versions.len()));
                        for ver in versions {
                            all_revisions.push(Revision {
                                id: media.id.clone(),
                                version: ver.version,
                                author: if ver.author.is_empty() { "unknown".to_string() } else { ver.author },
                                summary: ver.summary,
                                revision_type: ver.revision_type,
                                is_media: true,
                            });
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Warning: could not get media history for {}: {}", media.id, e);
                    // Fall back to current version
                    all_revisions.push(Revision {
                        id: media.id.clone(),
                        version: media.revision,
                        author: if media.author.is_empty() { "unknown".to_string() } else { media.author.clone() },
                        summary: "current version".to_string(),
                        revision_type: "E".to_string(),
                        is_media: true,
                    });
                }
            }
        }
    }

    // Apply depth limit if specified - keep only the most recent N revisions per item
    if let Some(max_depth) = depth {
        // Group revisions by item ID
        let mut revisions_by_id: HashMap<String, Vec<Revision>> = HashMap::new();
        for rev in all_revisions {
            revisions_by_id.entry(rev.id.clone()).or_default().push(rev);
        }

        // For each item, sort by version (newest first) and keep only max_depth
        all_revisions = Vec::new();
        for (_, mut revs) in revisions_by_id {
            revs.sort_by_key(|r| std::cmp::Reverse(r.version));
            revs.truncate(max_depth as usize);
            all_revisions.extend(revs);
        }

        verbosity.debug(&format!("Limited to {} revisions per item (depth={})", max_depth, max_depth));
    }

    // Sort by timestamp (oldest first)
    all_revisions.sort_by_key(|r| r.version);

    verbosity.info(&format!("Found {} total revisions", all_revisions.len()));

    if all_revisions.is_empty() {
        verbosity.info("No revisions to import");
        return Ok(None);
    }

    verbosity.info("Generating git history...");

    // Group revisions by timestamp
    let mut revisions_by_time: HashMap<i64, Vec<&Revision>> = HashMap::new();
    for rev in &all_revisions {
        revisions_by_time.entry(rev.version).or_default().push(rev);
    }

    // Track current file contents
    let mut current_files: HashMap<String, Vec<u8>> = HashMap::new();

    let mut mark: u64 = 1;
    let mut last_commit_mark: Option<u64> = None;
    let mut commit_count = 0;
    let mut latest_timestamp: i64 = 0;

    let mut timestamps: Vec<i64> = revisions_by_time.keys().copied().collect();
    timestamps.sort();

    for timestamp in timestamps {
        let revs = &revisions_by_time[&timestamp];

        // Collect authors and summaries
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
                    Some(format!("{}: {}", r.id, r.summary))
                }
            })
            .collect();

        let message = if summaries.is_empty() {
            let ids: Vec<&str> = revs.iter().map(|r| r.id.as_str()).collect();
            if ids.len() == 1 {
                format!("Edit {}", ids[0])
            } else {
                format!("Edit {} items", ids.len())
            }
        } else {
            summaries.join("\n")
        };

        // Fetch content for each file at this revision
        let mut blobs: Vec<(String, u64)> = Vec::new();
        let mut deleted_paths: Vec<String> = Vec::new();

        for rev in revs {
            let path = if rev.is_media {
                media_id_to_path(&rev.id, namespace)
            } else {
                page_id_to_path(&rev.id, namespace, extension)
            };

            // Check if this is a delete revision
            if rev.revision_type == "D" {
                current_files.remove(&path);
                deleted_paths.push(path);
                continue;
            }

            // Fetch content
            let content_result = if rev.is_media {
                client.get_attachment_version(&rev.id, rev.version)
            } else {
                client.get_page_version(&rev.id, rev.version).map(|s| s.into_bytes())
            };

            match content_result {
                Ok(data) => {
                    let blob_mark = mark;
                    mark += 1;

                    writeln!(out, "blob")?;
                    writeln!(out, "mark :{}", blob_mark)?;
                    writeln!(out, "data {}", data.len())?;
                    out.write_all(&data)?;
                    writeln!(out)?;

                    current_files.insert(path.clone(), data);
                    blobs.push((path, blob_mark));
                }
                Err(e) => {
                    eprintln!("Warning: could not fetch {}@{}: {}", rev.id, rev.version, e);
                }
            }
        }

        if blobs.is_empty() && deleted_paths.is_empty() {
            continue;
        }

        // Write commit
        let commit_mark = mark;
        mark += 1;

        let email = format!("{}@{}", author.replace(' ', ".").replace(',', ""), wiki_host);

        writeln!(out, "commit refs/dokuwiki/origin/heads/main")?;
        writeln!(out, "mark :{}", commit_mark)?;
        writeln!(out, "author {} <{}> {} +0000", author, email, timestamp)?;
        writeln!(out, "committer {} <{}> {} +0000", author, email, timestamp)?;
        writeln!(out, "data {}", message.len())?;
        write!(out, "{}", message)?;
        writeln!(out)?;

        if let Some(parent) = last_commit_mark {
            writeln!(out, "from :{}", parent)?;
        } else if let Some(sha) = parent_sha {
            writeln!(out, "from {}", sha)?;
        }

        // Write file modifications
        for (path, blob_mark) in &blobs {
            writeln!(out, "M 100644 :{} {}", blob_mark, path)?;
        }

        // Write file deletions
        for path in &deleted_paths {
            writeln!(out, "D {}", path)?;
        }

        writeln!(out)?;

        last_commit_mark = Some(commit_mark);
        commit_count += 1;
        latest_timestamp = latest_timestamp.max(timestamp);

        if commit_count % 100 == 0 {
            verbosity.info(&format!("  {} commits...", commit_count));
        }
    }

    verbosity.info(&format!("Generated {} commits", commit_count));

    Ok(if latest_timestamp > 0 { Some(latest_timestamp) } else { None })
}
