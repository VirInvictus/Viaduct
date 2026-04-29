// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::error::{Result, ViaductError};
use crate::models::{Feed, Folder};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct OpmlDocument {
    #[serde(rename = "@version")]
    pub version: String,
    pub head: Head,
    pub body: Body,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Head {
    pub title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Body {
    #[serde(rename = "outline", default)]
    pub outlines: Vec<Outline>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Outline {
    #[serde(rename = "@text", skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(rename = "@title", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "@type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(rename = "@xmlUrl", skip_serializing_if = "Option::is_none")]
    pub xml_url: Option<String>,
    #[serde(rename = "@htmlUrl", skip_serializing_if = "Option::is_none")]
    pub html_url: Option<String>,
    #[serde(rename = "outline", default)]
    pub outlines: Vec<Outline>,
}

pub struct OpmlFile {
    pub folders: Vec<Folder>,
    pub standalone_feeds: Vec<Feed>,
}

pub fn parse_opml(xml: &str) -> Result<OpmlFile> {
    let doc: OpmlDocument = quick_xml::de::from_str(xml)
        .map_err(|e| ViaductError::Parse(crate::error::ParseError::XmlDe(e.to_string())))?;

    let mut folders = Vec::new();
    let mut standalone_feeds = Vec::new();

    for outline in doc.body.outlines {
        if is_feed(&outline) {
            if let Some(feed) = parse_feed(&outline) {
                standalone_feeds.push(feed);
            }
        } else {
            // It's a folder
            let mut feeds = Vec::new();
            flatten_feeds(&outline.outlines, &mut feeds);

            let name = outline
                .title
                .or(outline.text)
                .unwrap_or_else(|| "Unnamed Folder".to_string());
            folders.push(Folder { name, feeds });
        }
    }

    Ok(OpmlFile {
        folders,
        standalone_feeds,
    })
}

/// Port of NNW `OPMLNormalizer.normalize`. Operates on the parsed-but-untrusted
/// outline tree of an *imported* OPML file (not our own saved `local.opml`).
///
/// Rules, matching the Swift behavior:
/// 1. Feeds are deduped by `xmlUrl` within their parent.
/// 2. A folder with no name (no `title` attribute, per NNW's
///    `titleFromAttributes` check) acts as a transparent wrapper — its
///    children promote one level up.
/// 3. Folders never nest more than one level deep. A named folder at any
///    nested position has its descendant feeds flattened into a single
///    feed list under that folder name.
///
/// Returns a flat-at-most-one-level `OpmlFile` ready to merge.
pub fn normalize_opml(file: OpmlFile) -> OpmlFile {
    use std::collections::HashSet;

    let outlines = opml_file_to_outlines(&file);
    let mut folders: Vec<Folder> = Vec::new();
    let mut standalone_feeds: Vec<Feed> = Vec::new();
    let mut top_seen: HashSet<String> = HashSet::new();

    walk_top_level(
        &outlines,
        &mut folders,
        &mut standalone_feeds,
        &mut top_seen,
    );

    OpmlFile {
        folders,
        standalone_feeds,
    }
}

fn walk_top_level(
    outlines: &[Outline],
    folders: &mut Vec<Folder>,
    standalone_feeds: &mut Vec<Feed>,
    top_seen: &mut std::collections::HashSet<String>,
) {
    for outline in outlines {
        if is_feed(outline) {
            let Some(feed) = parse_feed(outline) else {
                continue;
            };
            if top_seen.insert(feed.url.clone()) {
                standalone_feeds.push(feed);
            }
            continue;
        }
        match outline.title.clone() {
            // Nameless wrapper — promote children to top level.
            None => walk_top_level(&outline.outlines, folders, standalone_feeds, top_seen),
            Some(name) => {
                let mut feeds: Vec<Feed> = Vec::new();
                let mut folder_seen: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                flatten_named_folder(&outline.outlines, &mut feeds, &mut folder_seen);
                folders.push(Folder { name, feeds });
            }
        }
    }
}

fn flatten_named_folder(
    outlines: &[Outline],
    feeds: &mut Vec<Feed>,
    seen: &mut std::collections::HashSet<String>,
) {
    for outline in outlines {
        if is_feed(outline) {
            let Some(feed) = parse_feed(outline) else {
                continue;
            };
            if seen.insert(feed.url.clone()) {
                feeds.push(feed);
            }
        } else {
            // Both named and nameless nested folders flatten into the
            // current folder's feed list — NNW's folders-only-one-level-deep.
            flatten_named_folder(&outline.outlines, feeds, seen);
        }
    }
}

fn opml_file_to_outlines(file: &OpmlFile) -> Vec<Outline> {
    let mut out = Vec::with_capacity(file.standalone_feeds.len() + file.folders.len());
    for feed in &file.standalone_feeds {
        out.push(feed_to_outline(feed));
    }
    for folder in &file.folders {
        out.push(folder_to_outline(folder));
    }
    out
}

fn feed_to_outline(feed: &Feed) -> Outline {
    let display_name = feed
        .edited_name
        .clone()
        .or(feed.name.clone())
        .unwrap_or_default();
    Outline {
        text: Some(display_name.clone()),
        title: Some(display_name),
        type_: Some("rss".to_string()),
        xml_url: Some(feed.url.clone()),
        html_url: feed.home_page_url.clone(),
        outlines: Vec::new(),
    }
}

fn folder_to_outline(folder: &Folder) -> Outline {
    Outline {
        text: Some(folder.name.clone()),
        title: Some(folder.name.clone()),
        type_: None,
        xml_url: None,
        html_url: None,
        outlines: folder.feeds.iter().map(feed_to_outline).collect(),
    }
}

/// Merge a normalized incoming `OpmlFile` into `existing`, returning the new
/// state plus the list of feeds that were actually added (for refresh).
///
/// Rules (port of NNW `Account.addOPMLItems` behavior):
/// 1. Dedup feeds by `xmlUrl` against the union of every existing feed
///    (top-level + every folder's contents). NNW: `existingFeed(withURL:)`.
/// 2. New top-level feeds append to `standalone_feeds`.
/// 3. Folders match by name (case-sensitive). If a folder of that name
///    exists, merge feeds into it; if not, create it. NNW: `ensureFolder(with:)`.
/// 4. Never overwrite `edited_name` — NNW preserves user renames by writing
///    a feed's `editedName` independently of OPML. We don't carry that
///    in OPML at all, so the dedup-by-url skip is sufficient.
pub fn merge_opml(existing: &OpmlFile, incoming: OpmlFile) -> (OpmlFile, Vec<Feed>) {
    use std::collections::HashSet;

    let mut merged = OpmlFile {
        folders: existing.folders.clone(),
        standalone_feeds: existing.standalone_feeds.clone(),
    };
    let mut added: Vec<Feed> = Vec::new();

    let mut known_urls: HashSet<String> = HashSet::new();
    for f in &merged.standalone_feeds {
        known_urls.insert(f.url.clone());
    }
    for folder in &merged.folders {
        for f in &folder.feeds {
            known_urls.insert(f.url.clone());
        }
    }

    for feed in incoming.standalone_feeds {
        if !known_urls.contains(&feed.url) {
            known_urls.insert(feed.url.clone());
            added.push(feed.clone());
            merged.standalone_feeds.push(feed);
        }
    }

    for folder in incoming.folders {
        let target_idx = match merged.folders.iter().position(|f| f.name == folder.name) {
            Some(idx) => idx,
            None => {
                merged.folders.push(Folder {
                    name: folder.name.clone(),
                    feeds: Vec::new(),
                });
                merged.folders.len() - 1
            }
        };
        for feed in folder.feeds {
            if !known_urls.contains(&feed.url) {
                known_urls.insert(feed.url.clone());
                added.push(feed.clone());
                merged.folders[target_idx].feeds.push(feed);
            }
        }
    }

    (merged, added)
}

pub fn sync_inoreader_account(
    _existing: &OpmlFile,
    subscriptions: Vec<crate::network::inoreader::ReaderAPISubscription>,
    tags: Vec<crate::network::inoreader::ReaderAPITag>,
) -> OpmlFile {
    let mut folders = Vec::new();
    let mut standalone_feeds = Vec::new();
    let mut feed_map = std::collections::HashMap::new();

    // 1. Process tags into folders
    for tag in tags {
        // Inoreader folders have IDs like "user/123/label/FolderName"
        if let Some(name) = tag.id.split('/').next_back()
            && !tag.id.contains("/state/")
        {
            folders.push(Folder {
                name: name.to_string(),
                feeds: Vec::new(),
            });
        }
    }

    // 2. Process subscriptions into feeds and assign to folders
    for sub in subscriptions {
        let feed = Feed {
            id: sub.feed_id.clone(),
            url: sub.url.clone(),
            name: Some(sub.title.clone()),
            edited_name: None,
            home_page_url: sub.html_url.clone(),
        };
        feed_map.insert(sub.feed_id.clone(), feed.clone());

        if sub.categories.is_empty() {
            standalone_feeds.push(feed);
        } else {
            for category in sub.categories {
                if let Some(folder_name) = category.id.split('/').next_back()
                    && let Some(folder) = folders.iter_mut().find(|f| f.name == folder_name)
                {
                    folder.feeds.push(feed.clone());
                }
            }
        }
    }

    OpmlFile {
        folders,
        standalone_feeds,
    }
}

/// Hand-rolled OPML writer matching NNW's `OPMLExporter.OPMLString` byte
/// shape. The on-disk save path uses the serde-driven `serialize_opml`
/// because round-trippable structure is what matters there; user-facing
/// exports use this so the file looks identical to NetNewsWire's output
/// (same attribute order, same `description=""` placeholder, same
/// `version="RSS"`, tab indentation).
pub fn serialize_account_opml(title: &str, file: &OpmlFile) -> String {
    let escaped_title = escape_xml(title);
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<!-- OPML generated by viaduct -->\n");
    s.push_str("<opml version=\"1.1\">\n");
    s.push_str("\t<head>\n");
    s.push_str(&format!("\t\t<title>{}</title>\n", escaped_title));
    s.push_str("\t</head>\n");
    s.push_str("<body>\n");

    // NNW `Account.OPMLString`: top-level feeds first (sorted), then folders
    // (sorted). Sorting keeps export output stable across runs.
    let mut feeds_sorted: Vec<&Feed> = file.standalone_feeds.iter().collect();
    feeds_sorted.sort_by_key(|f| feed_sort_key(f));
    for feed in feeds_sorted {
        s.push_str(&feed_opml_string(feed, 1));
    }

    let mut folders_sorted: Vec<&Folder> = file.folders.iter().collect();
    folders_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for folder in folders_sorted {
        s.push_str(&folder_opml_string(folder, 1));
    }

    s.push_str("</body>\n");
    s.push_str("</opml>");
    s
}

fn feed_opml_string(feed: &Feed, indent: usize) -> String {
    // NNW Feed.OPMLString uses editedName ?? name ?? "" — never `nameForDisplay`,
    // because that can stamp "Untitled" onto disk.
    let name = feed
        .edited_name
        .as_deref()
        .or(feed.name.as_deref())
        .unwrap_or("");
    let escaped_name = escape_xml(name);
    let escaped_html = feed
        .home_page_url
        .as_deref()
        .map(escape_xml)
        .unwrap_or_default();
    let escaped_xml = escape_xml(&feed.url);

    format!(
        "{indent}<outline text=\"{name}\" title=\"{name}\" description=\"\" type=\"rss\" version=\"RSS\" htmlUrl=\"{html}\" xmlUrl=\"{xml}\"/>\n",
        indent = "\t".repeat(indent),
        name = escaped_name,
        html = escaped_html,
        xml = escaped_xml,
    )
}

fn folder_opml_string(folder: &Folder, indent: usize) -> String {
    let escaped_name = escape_xml(&folder.name);
    let pad = "\t".repeat(indent);

    if folder.feeds.is_empty() {
        // NNW Folder.OPMLString self-closes when no children exist.
        return format!(
            "{pad}<outline text=\"{name}\" title=\"{name}\"/>\n",
            pad = pad,
            name = escaped_name,
        );
    }

    let mut s = format!(
        "{pad}<outline text=\"{name}\" title=\"{name}\">\n",
        pad = pad,
        name = escaped_name,
    );
    let mut feeds_sorted: Vec<&Feed> = folder.feeds.iter().collect();
    feeds_sorted.sort_by_key(|f| feed_sort_key(f));
    for feed in feeds_sorted {
        s.push_str(&feed_opml_string(feed, indent + 1));
    }
    s.push_str(&pad);
    s.push_str("</outline>\n");
    s
}

fn feed_sort_key(feed: &Feed) -> String {
    feed.edited_name
        .clone()
        .or_else(|| feed.name.clone())
        .unwrap_or_else(|| feed.url.clone())
        .to_lowercase()
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn is_feed(outline: &Outline) -> bool {
    // Older viaduct builds (and the serde default behavior) sometimes wrote
    // `xmlUrl=""` on folder outlines. Treat empty strings as "no URL" so
    // folders aren't misclassified as zero-URL feeds.
    let has_xml = outline
        .xml_url
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    has_xml || outline.type_.as_deref() == Some("rss")
}

fn parse_feed(outline: &Outline) -> Option<Feed> {
    let url = outline.xml_url.clone().filter(|s| !s.is_empty())?;
    Some(Feed {
        id: url.clone(), // ID is the URL initially
        url,
        name: outline.title.clone().or_else(|| outline.text.clone()),
        edited_name: None,
        home_page_url: outline.html_url.clone().filter(|s| !s.is_empty()),
    })
}

fn flatten_feeds(outlines: &[Outline], feeds: &mut Vec<Feed>) {
    for outline in outlines {
        if is_feed(outline) {
            if let Some(feed) = parse_feed(outline) {
                feeds.push(feed);
            }
        } else {
            flatten_feeds(&outline.outlines, feeds);
        }
    }
}

pub fn serialize_opml(opml_file: &OpmlFile) -> Result<String> {
    let mut outlines = Vec::new();

    for folder in &opml_file.folders {
        let folder_outlines: Vec<Outline> = folder
            .feeds
            .iter()
            .map(|f| Outline {
                text: f.name.clone(),
                title: f.name.clone(),
                type_: Some("rss".to_string()),
                xml_url: Some(f.url.clone()),
                html_url: f.home_page_url.clone(),
                outlines: Vec::new(),
            })
            .collect();

        outlines.push(Outline {
            text: Some(folder.name.clone()),
            title: Some(folder.name.clone()),
            type_: None,
            xml_url: None,
            html_url: None,
            outlines: folder_outlines,
        });
    }

    for feed in &opml_file.standalone_feeds {
        outlines.push(Outline {
            text: feed.name.clone(),
            title: feed.name.clone(),
            type_: Some("rss".to_string()),
            xml_url: Some(feed.url.clone()),
            html_url: feed.home_page_url.clone(),
            outlines: Vec::new(),
        });
    }

    let doc = OpmlDocument {
        version: "1.0".to_string(),
        head: Head {
            title: Some("viaduct".to_string()),
        },
        body: Body { outlines },
    };

    let mut buf = String::new();
    let mut ser = quick_xml::se::Serializer::new(&mut buf);
    ser.indent(' ', 4);
    doc.serialize(ser)
        .map_err(|e| ViaductError::Parse(crate::error::ParseError::XmlSe(e.to_string())))?;

    Ok(buf)
}

pub struct OpmlWriter {
    sender: mpsc::Sender<OpmlWriterMsg>,
}

enum OpmlWriterMsg {
    Save(OpmlFile, oneshot::Sender<Result<()>>),
}

impl OpmlWriter {
    pub fn spawn(path: impl AsRef<Path> + Send + 'static) -> Self {
        let (tx, mut rx) = mpsc::channel::<OpmlWriterMsg>(10);
        let path = path.as_ref().to_path_buf();

        tokio::spawn(async move {
            let mut pending_save: Option<OpmlFile> = None;
            let mut save_txs: Vec<oneshot::Sender<Result<()>>> = Vec::new();

            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(OpmlWriterMsg::Save(file, reply_tx)) => {
                                pending_save = Some(file);
                                save_txs.push(reply_tx);
                            }
                            None => break, // Channel closed
                        }
                    }
                    _ = sleep(Duration::from_millis(500)), if pending_save.is_some() => {
                        if let Some(file) = pending_save.take() {
                            let res = Self::write_to_disk(&path, &file).await;
                            // Coalesced save: every queued caller gets the
                            // same flush result. The borrowing dance through
                            // io::Error::other lets us hand each oneshot a
                            // Result<()> without cloning ViaductError (which
                            // wraps non-Clone source errors).
                            for reply_tx in save_txs.drain(..) {
                                let send_res = res.as_ref().map(|_| ()).map_err(|e| {
                                    crate::error::ViaductError::Io(std::io::Error::other(
                                        e.to_string(),
                                    ))
                                });
                                let _ = reply_tx.send(send_res);
                            }
                        }
                    }
                }
            }
        });

        Self { sender: tx }
    }

    pub async fn save(&self, file: OpmlFile) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(OpmlWriterMsg::Save(file, tx))
            .await
            .map_err(|_| ViaductError::Database(crate::error::DatabaseError::WriterGone))?;
        rx.await.unwrap_or_else(|_| {
            Err(ViaductError::Database(
                crate::error::DatabaseError::WriterGone,
            ))
        })
    }

    async fn write_to_disk(path: &Path, file: &OpmlFile) -> std::io::Result<()> {
        let xml = serialize_opml(file).map_err(|e| std::io::Error::other(e.to_string()))?;

        let temp_path = path.with_extension("opml.tmp");
        tokio::fs::write(&temp_path, xml).await?;
        tokio::fs::rename(temp_path, path).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(url: &str, name: &str) -> Feed {
        Feed {
            id: url.to_string(),
            url: url.to_string(),
            name: Some(name.to_string()),
            edited_name: None,
            home_page_url: None,
        }
    }

    #[test]
    fn normalize_drops_nameless_wrapper() {
        // A nameless folder (no `title` attribute) should promote its
        // children to the top level — NNW's `titleFromAttributes == nil`
        // branch in OPMLNormalizer.
        let outlines = vec![Outline {
            text: None,
            title: None,
            type_: None,
            xml_url: None,
            html_url: None,
            outlines: vec![
                Outline {
                    text: None,
                    title: Some("A".into()),
                    type_: Some("rss".into()),
                    xml_url: Some("https://a/feed".into()),
                    html_url: None,
                    outlines: Vec::new(),
                },
                Outline {
                    text: None,
                    title: Some("B".into()),
                    type_: Some("rss".into()),
                    xml_url: Some("https://b/feed".into()),
                    html_url: None,
                    outlines: Vec::new(),
                },
            ],
        }];
        let mut folders = Vec::new();
        let mut standalone = Vec::new();
        let mut seen = std::collections::HashSet::new();
        walk_top_level(&outlines, &mut folders, &mut standalone, &mut seen);
        assert!(folders.is_empty());
        assert_eq!(standalone.len(), 2);
        assert_eq!(standalone[0].url, "https://a/feed");
        assert_eq!(standalone[1].url, "https://b/feed");
    }

    #[test]
    fn normalize_flattens_nested_folders() {
        // A named folder containing a nested named folder should flatten:
        // result is one folder with all descendant feeds, deduped.
        let outlines = vec![Outline {
            text: None,
            title: Some("Tech".into()),
            type_: None,
            xml_url: None,
            html_url: None,
            outlines: vec![
                Outline {
                    text: None,
                    title: Some("X".into()),
                    type_: Some("rss".into()),
                    xml_url: Some("https://x/feed".into()),
                    html_url: None,
                    outlines: Vec::new(),
                },
                Outline {
                    text: None,
                    title: Some("Subgroup".into()),
                    type_: None,
                    xml_url: None,
                    html_url: None,
                    outlines: vec![Outline {
                        text: None,
                        title: Some("Y".into()),
                        type_: Some("rss".into()),
                        xml_url: Some("https://y/feed".into()),
                        html_url: None,
                        outlines: Vec::new(),
                    }],
                },
            ],
        }];
        let mut folders = Vec::new();
        let mut standalone = Vec::new();
        let mut seen = std::collections::HashSet::new();
        walk_top_level(&outlines, &mut folders, &mut standalone, &mut seen);
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Tech");
        let urls: Vec<&str> = folders[0].feeds.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(urls, vec!["https://x/feed", "https://y/feed"]);
        assert!(standalone.is_empty());
    }

    #[test]
    fn normalize_dedups_feeds_within_folder() {
        let outlines = vec![Outline {
            text: None,
            title: Some("News".into()),
            type_: None,
            xml_url: None,
            html_url: None,
            outlines: vec![
                Outline {
                    text: None,
                    title: Some("dup".into()),
                    type_: Some("rss".into()),
                    xml_url: Some("https://x/feed".into()),
                    html_url: None,
                    outlines: Vec::new(),
                },
                Outline {
                    text: None,
                    title: Some("dup".into()),
                    type_: Some("rss".into()),
                    xml_url: Some("https://x/feed".into()),
                    html_url: None,
                    outlines: Vec::new(),
                },
            ],
        }];
        let mut folders = Vec::new();
        let mut standalone = Vec::new();
        let mut seen = std::collections::HashSet::new();
        walk_top_level(&outlines, &mut folders, &mut standalone, &mut seen);
        assert_eq!(folders[0].feeds.len(), 1);
        let _ = standalone;
    }

    #[test]
    fn merge_appends_only_new_feeds() {
        let existing = OpmlFile {
            folders: vec![Folder {
                name: "News".into(),
                feeds: vec![feed("https://a", "A")],
            }],
            standalone_feeds: vec![feed("https://x", "X")],
        };
        let incoming = OpmlFile {
            folders: vec![Folder {
                name: "News".into(),
                feeds: vec![feed("https://a", "A-renamed"), feed("https://b", "B")],
            }],
            standalone_feeds: vec![feed("https://x", "X-renamed"), feed("https://y", "Y")],
        };
        let (merged, added) = merge_opml(&existing, incoming);

        // Top-level: x preserved (with original name "X"), y added.
        assert_eq!(merged.standalone_feeds.len(), 2);
        assert_eq!(merged.standalone_feeds[0].name.as_deref(), Some("X"));
        assert_eq!(merged.standalone_feeds[1].url, "https://y");

        // Folder "News" gets b appended; a is preserved with original name.
        assert_eq!(merged.folders.len(), 1);
        assert_eq!(merged.folders[0].feeds.len(), 2);
        assert_eq!(merged.folders[0].feeds[0].name.as_deref(), Some("A"));
        assert_eq!(merged.folders[0].feeds[1].url, "https://b");

        // `added` reports only the genuinely new feeds.
        let added_urls: Vec<&str> = added.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(added_urls, vec!["https://y", "https://b"]);
    }

    #[test]
    fn merge_creates_missing_folder() {
        let existing = OpmlFile {
            folders: Vec::new(),
            standalone_feeds: Vec::new(),
        };
        let incoming = OpmlFile {
            folders: vec![Folder {
                name: "New".into(),
                feeds: vec![feed("https://a", "A")],
            }],
            standalone_feeds: Vec::new(),
        };
        let (merged, added) = merge_opml(&existing, incoming);
        assert_eq!(merged.folders.len(), 1);
        assert_eq!(merged.folders[0].name, "New");
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn export_matches_nnw_shape() {
        let file = OpmlFile {
            folders: vec![Folder {
                name: "Tech & News".into(),
                feeds: vec![Feed {
                    id: "https://a".into(),
                    url: "https://a".into(),
                    name: Some("A".into()),
                    edited_name: None,
                    home_page_url: Some("https://a.example".into()),
                }],
            }],
            standalone_feeds: vec![Feed {
                id: "https://b".into(),
                url: "https://b".into(),
                name: Some("B".into()),
                edited_name: Some("Bee".into()),
                home_page_url: None,
            }],
        };
        let s = serialize_account_opml("export.opml", &file);
        // Shape checks: header, comment, NNW attribute order, edited_name wins,
        // ampersand escaped in folder title, version="RSS", description="".
        assert!(s.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(s.contains("<!-- OPML generated by viaduct -->"));
        assert!(s.contains("<title>export.opml</title>"));
        assert!(s.contains("Tech &amp; News"));
        assert!(s.contains(
            "<outline text=\"Bee\" title=\"Bee\" description=\"\" type=\"rss\" version=\"RSS\" htmlUrl=\"\" xmlUrl=\"https://b\"/>"
        ));
        assert!(s.ends_with("</opml>"));
    }
}
