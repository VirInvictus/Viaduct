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

/// Rebuild the OPML tree from the Inoreader subscription + tag lists,
/// merged with the local tree (`existing`). The server is authoritative
/// for which feeds exist and how they are grouped, but four things must
/// survive the reconcile (ports of NNW `ReaderAPIAccountDelegate` fixes
/// from the 2026-07/09 window):
///
/// * an existing feed keeps its `edited_name`, and an empty server
///   title does not blank a name we already have (`9412559ad`);
/// * a subscription whose category names a folder the tag list doesn't
///   know lands at the top level instead of being dropped
///   (`8e0007233`, `2b46ee65c`);
/// * a just-created local folder survives the sync — the Reader API has
///   no create-tag endpoint, so the server only learns a folder when a
///   feed is tagged with it (`f50bcd4ff`). Feeds already inside such a
///   folder stay there while the server doesn't group them; once the
///   server groups them (or groups them elsewhere), the server wins.
pub fn sync_inoreader_account(
    existing: &OpmlFile,
    subscriptions: Vec<crate::network::inoreader::ReaderAPISubscription>,
    tags: Vec<crate::network::inoreader::ReaderAPITag>,
) -> OpmlFile {
    // Server-known folder names, from the tag list. `/state/` tags are
    // read/starred states, not folders.
    let server_folder_names: std::collections::HashSet<String> = tags
        .iter()
        .filter(|t| !t.id.contains("/state/"))
        .filter_map(|t| t.id.split('/').next_back().map(str::to_string))
        .collect();

    // Kept local-only folders, in existing order. A folder deleted on
    // the server while empty is indistinguishable from one that was
    // never synced, and keeping it is the cheaper wrong answer: the
    // user can delete it here, whereas a vanished new folder loses
    // work in progress.
    let mut folders: Vec<Folder> = Vec::new();
    for folder in &existing.folders {
        if !server_folder_names.contains(&folder.name) {
            folders.push(folder.clone());
        }
    }
    let local_only_folder_names: std::collections::HashSet<String> =
        folders.iter().map(|f| f.name.clone()).collect();

    // Server-known folders, in tag order. All of them, even if no feed
    // lands inside — the tag exists server-side.
    for tag in &tags {
        if tag.id.contains("/state/") {
            continue;
        }
        if let Some(name) = tag.id.split('/').next_back()
            && !folders.iter().any(|f| f.name == name)
        {
            folders.push(Folder {
                name: name.to_string(),
                feeds: Vec::new(),
            });
        }
    }

    // Existing feeds by id (fallback url), for name/edited_name carry-over.
    let mut existing_by_id: std::collections::HashMap<&str, &Feed> =
        std::collections::HashMap::new();
    let mut existing_by_url: std::collections::HashMap<&str, &Feed> =
        std::collections::HashMap::new();
    for feed in existing
        .standalone_feeds
        .iter()
        .chain(existing.folders.iter().flat_map(|f| f.feeds.iter()))
    {
        existing_by_id.insert(feed.id.as_str(), feed);
        existing_by_url.insert(feed.url.as_str(), feed);
    }

    let mut standalone_feeds: Vec<Feed> = Vec::new();
    let mut subscribed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for sub in subscriptions {
        let prior = existing_by_id
            .get(sub.feed_id.as_str())
            .or_else(|| existing_by_url.get(sub.url.as_str()))
            .copied();

        // NNW `9412559ad`: only overwrite the name when the server
        // provides a non-empty one, and never touch `edited_name`.
        let name = if sub.title.is_empty() {
            prior.and_then(|f| f.name.clone())
        } else {
            Some(sub.title.clone())
        };
        let feed = Feed {
            id: sub.feed_id.clone(),
            url: sub.url.clone(),
            name,
            edited_name: prior.and_then(|f| f.edited_name.clone()),
            home_page_url: sub.html_url.clone(),
        };
        subscribed_ids.insert(feed.id.clone());

        let category_names: Vec<&str> = sub
            .categories
            .iter()
            .filter_map(|c| c.id.split('/').next_back())
            .collect();

        if category_names.is_empty() {
            // Server doesn't group this feed. If it already lives in a
            // local-only folder (created here, tag not on the server
            // yet), keep it there — replacing the stale copy so a
            // renamed feed doesn't duplicate; otherwise top level.
            let target = folders.iter_mut().find(|f| {
                local_only_folder_names.contains(&f.name)
                    && f.feeds
                        .iter()
                        .any(|existing_feed| existing_feed.id == feed.id)
            });
            match target {
                Some(folder) => {
                    if let Some(slot) = folder.feeds.iter_mut().find(|f| f.id == feed.id) {
                        *slot = feed;
                    }
                }
                None => standalone_feeds.push(feed),
            }
            continue;
        }

        // Server grouping wins. A category naming an unknown folder
        // (tag list missed it) must not drop the feed — top level
        // instead (`8e0007233`).
        let mut placed = false;
        let mut seen_folders = std::collections::HashSet::new();
        for category_name in &category_names {
            if server_folder_names.contains(*category_name)
                && let Some(folder) = folders.iter_mut().find(|f| f.name == *category_name)
                && seen_folders.insert(folder.name.clone())
            {
                folder.feeds.push(feed.clone());
                placed = true;
            }
        }
        // The server grouped this feed, so any stale copy in a kept
        // local-only folder loses — it must not end up in two places.
        for folder in folders.iter_mut() {
            if local_only_folder_names.contains(&folder.name) {
                folder.feeds.retain(|f| f.id != feed.id);
            }
        }
        if !placed {
            standalone_feeds.push(feed);
        }
    }

    // A feed in a kept local-only folder that is no longer in the
    // subscription list was unsubscribed — drop it there too.
    for folder in folders.iter_mut() {
        if local_only_folder_names.contains(&folder.name) {
            folder.feeds.retain(|f| subscribed_ids.contains(&f.id));
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

/// Characters illegal in XML 1.0: most control codes (tab, LF, CR are
/// the legal ones) and the non-characters around the surrogate block.
/// A Rust `char` is always a scalar, so surrogates can't occur here.
/// quick-xml escapes the five predefined entities but writes these raw,
/// and one raw control character in local.opml makes the whole file
/// fail to parse on the next load. Port of NNW `858b65931`
/// (escapingSpecialXMLCharacters dropping illegal scalars).
fn is_legal_xml_character(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r')
        || ('\u{20}'..='\u{D7FF}').contains(&c)
        || ('\u{E000}'..='\u{FFFD}').contains(&c)
        || (c as u32 >= 0x10000)
}

/// `escape_xml` with the illegal-character drop.
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => {
                if is_legal_xml_character(c) {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Illegal-character filter for the serde-driven save path, where
/// quick-xml does the entity escaping itself.
fn xml_safe(s: &str) -> String {
    s.chars().filter(|c| is_legal_xml_character(*c)).collect()
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
                text: f.name.as_ref().map(|n| xml_safe(n)),
                title: f.name.as_ref().map(|n| xml_safe(n)),
                type_: Some("rss".to_string()),
                xml_url: Some(xml_safe(&f.url)),
                html_url: f.home_page_url.as_ref().map(|u| xml_safe(u)),
                outlines: Vec::new(),
            })
            .collect();

        outlines.push(Outline {
            text: Some(xml_safe(&folder.name)),
            title: Some(xml_safe(&folder.name)),
            type_: None,
            xml_url: None,
            html_url: None,
            outlines: folder_outlines,
        });
    }

    for feed in &opml_file.standalone_feeds {
        outlines.push(Outline {
            text: feed.name.as_ref().map(|n| xml_safe(n)),
            title: feed.name.as_ref().map(|n| xml_safe(n)),
            type_: Some("rss".to_string()),
            xml_url: Some(xml_safe(&feed.url)),
            html_url: feed.home_page_url.as_ref().map(|u| xml_safe(u)),
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

    #[test]
    fn escape_xml_and_serialize_drop_illegal_xml_characters() {
        // NNW `858b65931`: control characters other than tab/LF/CR are
        // illegal in XML 1.0. Written raw they poison local.opml — the
        // next load fails to parse and the subscription list is lost.
        let dirty = "a\u{0}b\u{1F}c\td\ne\rf\u{7}g";
        assert_eq!(escape_xml(dirty), "abc\td\ne\rfg");
        // Sanity: entity escaping is unchanged.
        assert_eq!(escape_xml("a&b<c>"), "a&amp;b&lt;c&gt;");

        // The serde save path (what actually writes local.opml) filters
        // too: quick-xml escapes entities but passes control chars raw.
        let file = OpmlFile {
            folders: vec![Folder {
                name: "f\u{2}lder".into(),
                feeds: vec![Feed {
                    id: "https://x".into(),
                    url: "https://x\u{3}/feed".into(),
                    name: Some("n\u{4}me".into()),
                    edited_name: None,
                    home_page_url: None,
                }],
            }],
            standalone_feeds: vec![],
        };
        let xml = serialize_opml(&file).unwrap();
        assert!(!xml.contains('\u{2}') && !xml.contains('\u{3}') && !xml.contains('\u{4}'));
        // The file must round-trip through our own parser instead of
        // failing the next load.
        let reparsed = parse_opml(&xml).expect("poisoned OPML");
        assert_eq!(reparsed.folders[0].name, "flder");
        assert_eq!(reparsed.folders[0].feeds[0].name.as_deref(), Some("nme"));
    }

    // --- sync_inoreader_account ---

    fn sub(
        id: &str,
        title: &str,
        categories: &[&str],
    ) -> crate::network::inoreader::ReaderAPISubscription {
        crate::network::inoreader::ReaderAPISubscription {
            feed_id: id.to_string(),
            title: title.to_string(),
            categories: categories
                .iter()
                .map(|c| crate::network::inoreader::ReaderAPITag {
                    id: format!("user/-/label/{c}"),
                    sortid: None,
                })
                .collect(),
            url: format!("https://{id}/feed"),
            html_url: Some(format!("https://{id}")),
            icon_url: None,
        }
    }

    fn server_tag(name: &str) -> crate::network::inoreader::ReaderAPITag {
        crate::network::inoreader::ReaderAPITag {
            id: format!("user/1234/label/{name}"),
            sortid: None,
        }
    }

    #[test]
    fn sync_preserves_edited_name_and_skips_empty_title() {
        // NNW `9412559ad`: the reconcile must not wipe edited_name, and
        // an empty server title must not blank a name we already have.
        let existing = OpmlFile {
            folders: vec![],
            standalone_feeds: vec![Feed {
                id: "feed/1".into(),
                url: "https://one/feed".into(),
                name: Some("Old Name".into()),
                edited_name: Some("My Name".into()),
                home_page_url: None,
            }],
        };
        let subs = vec![sub("feed/1", "", &[])];
        let out = sync_inoreader_account(&existing, subs, vec![]);
        assert_eq!(out.standalone_feeds.len(), 1);
        assert_eq!(out.standalone_feeds[0].name.as_deref(), Some("Old Name"));
        assert_eq!(
            out.standalone_feeds[0].edited_name.as_deref(),
            Some("My Name")
        );

        // Non-empty server title replaces `name`, edited_name still holds.
        let subs = vec![sub("feed/1", "Server Name", &[])];
        let out = sync_inoreader_account(&existing, subs, vec![]);
        assert_eq!(out.standalone_feeds[0].name.as_deref(), Some("Server Name"));
        assert_eq!(
            out.standalone_feeds[0].edited_name.as_deref(),
            Some("My Name")
        );
    }

    #[test]
    fn sync_feed_with_unknown_folder_goes_top_level_not_dropped() {
        // NNW `8e0007233` / `2b46ee65c`: a subscription categorized into
        // a folder the tag list doesn't know must survive the reconcile.
        let existing = OpmlFile {
            folders: vec![],
            standalone_feeds: vec![],
        };
        let subs = vec![sub("feed/1", "One", &["Ghost Folder"])];
        let out = sync_inoreader_account(&existing, subs, vec![server_tag("Other")]);
        assert!(out.standalone_feeds.iter().any(|f| f.id == "feed/1"));
        assert!(out.folders.iter().all(|f| f.name != "Ghost Folder"));
    }

    #[test]
    fn sync_keeps_local_only_folder_and_its_feed() {
        // NNW `f50bcd4ff`: a folder created locally has no tag on the
        // server until a feed is tagged with it, so the sync must not
        // drop it — and a feed already inside stays there while the
        // server doesn't group it.
        let existing = OpmlFile {
            folders: vec![Folder {
                name: "New Folder".into(),
                feeds: vec![Feed {
                    id: "feed/1".into(),
                    url: "https://one/feed".into(),
                    name: Some("One".into()),
                    edited_name: None,
                    home_page_url: None,
                }],
            }],
            standalone_feeds: vec![],
        };
        let subs = vec![sub("feed/1", "One", &[])];
        let out = sync_inoreader_account(&existing, subs, vec![]);
        assert_eq!(out.folders.len(), 1);
        assert_eq!(out.folders[0].name, "New Folder");
        assert_eq!(out.folders[0].feeds.len(), 1);
        assert_eq!(out.folders[0].feeds[0].id, "feed/1");
        assert!(out.standalone_feeds.is_empty());

        // Once the server groups the feed elsewhere, the server wins:
        // the feed moves and the (now empty) local folder stays.
        let subs = vec![sub("feed/1", "One", &["Server Folder"])];
        let out = sync_inoreader_account(&existing, subs, vec![server_tag("Server Folder")]);
        assert!(
            out.folders
                .iter()
                .any(|f| f.name == "Server Folder" && f.feeds.iter().any(|f| f.id == "feed/1"))
        );
        let local = out.folders.iter().find(|f| f.name == "New Folder").unwrap();
        assert!(local.feeds.is_empty());
    }

    #[test]
    fn sync_drops_unsubscribed_feed_from_local_folder() {
        let existing = OpmlFile {
            folders: vec![Folder {
                name: "Local".into(),
                feeds: vec![Feed {
                    id: "feed/1".into(),
                    url: "https://one/feed".into(),
                    name: Some("One".into()),
                    edited_name: None,
                    home_page_url: None,
                }],
            }],
            standalone_feeds: vec![],
        };
        let subs = vec![sub("feed/2", "Two", &[])];
        let out = sync_inoreader_account(&existing, subs, vec![]);
        let local = out.folders.iter().find(|f| f.name == "Local").unwrap();
        assert!(local.feeds.is_empty());
        assert_eq!(out.standalone_feeds.len(), 1);
    }
}
