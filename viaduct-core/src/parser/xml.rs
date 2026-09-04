// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::error::{ParseError, Result};
use crate::models::{Attachment, Author, ParsedFeed, ParsedItem};
use crate::parser::date::parse_date_bytes;
use crate::parser::html_url_resolver::resolving_relative_urls;
use md5::{Digest, Md5};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::str;

/// Build an `Attachment` from an element's attributes. Requires a non-empty
/// `url`; reads optional `type`, an integer size under `size_key` (`length`
/// for RSS `<enclosure>` per NNW `RSSDelegate.handleEnclosure`, `fileSize` for
/// MRSS `<media:content>` / `<media:thumbnail>`), and the optional MRSS
/// `duration` (absent on enclosures, so harmless there). quick-xml strips
/// namespace prefixes on `local_name`, so callers must already have confirmed
/// the element name.
fn attachment_from_attrs(e: &quick_xml::events::BytesStart, size_key: &[u8]) -> Option<Attachment> {
    let mut url: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut size: Option<i64> = None;
    let mut duration: Option<i64> = None;
    for attr in e.attributes().filter_map(|a| a.ok()) {
        let key = attr.key.as_ref();
        if key == b"url" {
            url = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
        } else if key == b"type" {
            mime_type = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
        } else if key == size_key {
            size = str::from_utf8(attr.value.as_ref())
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|n| *n > 0);
        } else if key == b"duration" {
            duration = str::from_utf8(attr.value.as_ref())
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|n| *n > 0);
        }
    }
    let url = url?;
    if url.is_empty() {
        return None;
    }
    Some(Attachment {
        url,
        mime_type,
        title: None,
        size_in_bytes: size,
        duration_in_seconds: duration,
    })
}

#[derive(Debug, Clone)]
pub struct OpmlDocument {
    pub title: Option<String>,
    pub items: Vec<OpmlItem>,
}

#[derive(Debug, Clone)]
pub struct OpmlItem {
    pub title: Option<String>,
    pub text: Option<String>,
    pub xml_url: Option<String>,
    pub html_url: Option<String>,
    pub children: Vec<OpmlItem>,
}

pub fn parse_feed(data: &[u8], feed_url: &str) -> Result<ParsedFeed> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let name_ref = name.as_ref();
                let is_unprefixed_rss = name_ref == b"rss";
                let is_rdf = name_ref == b"RDF" || name_ref == b"rdf:RDF";
                if is_unprefixed_rss || is_rdf {
                    return parse_rss(data, feed_url, is_rdf);
                } else if name_ref == b"feed" {
                    return parse_atom(data, feed_url);
                } else if name_ref == b"opml" {
                    return Err(ParseError::UnknownFormat.into()); // OPML is handled separately
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => (),
        }
        buf.clear();
    }

    Err(ParseError::UnknownFormat.into())
}

fn calculate_id(
    guid: Option<&str>,
    permalink: Option<&str>,
    link: Option<&str>,
    title: Option<&str>,
    body: Option<&str>,
    date_published: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    if let Some(g) = guid
        && !g.is_empty()
    {
        return g.to_string();
    }
    let dp_string = date_published.map(|d| format!("{:.0}", d.timestamp() as f64));
    let mut s = String::new();

    if let (Some(p), Some(d)) = (permalink, &dp_string)
        && !p.is_empty()
    {
        s.push_str(p);
        s.push_str(d);
    }
    if s.is_empty()
        && let (Some(l), Some(d)) = (link, &dp_string)
        && !l.is_empty()
    {
        s.push_str(l);
        s.push_str(d);
    }
    if s.is_empty()
        && let (Some(t), Some(d)) = (title, &dp_string)
        && !t.is_empty()
    {
        s.push_str(t);
        s.push_str(d);
    }
    if s.is_empty()
        && let Some(d) = dp_string
    {
        s.push_str(&d);
    }
    if s.is_empty()
        && let Some(p) = permalink
        && !p.is_empty()
    {
        s.push_str(p);
    }
    if s.is_empty()
        && let Some(l) = link
        && !l.is_empty()
    {
        s.push_str(l);
    }
    if s.is_empty()
        && let Some(t) = title
        && !t.is_empty()
    {
        s.push_str(t);
    }
    if s.is_empty()
        && let Some(b) = body
        && !b.is_empty()
    {
        s.push_str(b);
    }

    // MD5 matches NNW's `s.md5String`. Must be deterministic across builds —
    // a non-stable hash here would orphan article statuses on every restart.
    let mut hasher = Md5::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Resolve a possibly-relative URL against the feed's home page URL.
/// Mirrors NNW `RSSDelegate.resolveURL`. Returns the original string on failure.
fn resolve_url(s: &str, base: Option<&str>) -> String {
    if s.to_ascii_lowercase().starts_with("http") {
        return s.to_string();
    }
    let Some(base_str) = base else {
        return s.to_string();
    };
    let Ok(base_url) = url::Url::parse(base_str) else {
        return s.to_string();
    };
    match base_url.join(s) {
        Ok(joined) => joined.to_string(),
        Err(_) => s.to_string(),
    }
}

/// NNW's heuristic for whether a `<guid>` value can double as a permalink.
fn guid_looks_like_url(s: &str) -> bool {
    if s.contains(' ') {
        return false;
    }
    if !s.contains('/') {
        return false;
    }
    if s.to_ascii_lowercase().starts_with("tag:") {
        return false;
    }
    true
}

/// Dispatches a Text or CDATA payload into the right RSS-item / RSS-channel
/// field. Extracted so the main loop can share one body of logic across
/// `Event::Text` and `Event::CData` arms — without this, CDATA-wrapped
/// `<description>` and `<content:encoded>` (the dominant shape on
/// WordPress and most news sites) silently dropped to `None`.
#[allow(clippy::too_many_arguments)]
fn rss_handle_text_or_cdata(
    text_str: String,
    in_item: bool,
    in_channel_image: bool,
    current_tag: &[u8],
    current_guid_is_permalink: bool,
    current_item_title: &mut Option<String>,
    current_item_link: &mut Option<String>,
    current_item_body: &mut Option<String>,
    current_item_guid: &mut Option<String>,
    current_item_permalink: &mut Option<String>,
    current_item_date: &mut Option<chrono::DateTime<chrono::Utc>>,
    current_item_authors: &mut Vec<Author>,
    icon_url: &mut Option<String>,
    title: &mut Option<String>,
    home_page_url: &mut Option<String>,
    language: &mut Option<String>,
) {
    if in_item {
        match current_tag {
            b"title" if current_item_title.is_none() => {
                *current_item_title = Some(text_str);
            }
            b"link" if current_item_link.is_none() => {
                *current_item_link = Some(resolve_url(&text_str, home_page_url.as_deref()));
            }
            b"description" if current_item_body.is_none() => {
                *current_item_body = Some(text_str);
            }
            b"encoded" => {
                // `<content:encoded>` always wins over `<description>`
                // because it's the full body where description is the
                // summary. NNW prefers it the same way.
                *current_item_body = Some(text_str);
            }
            b"guid" => {
                *current_item_guid = Some(text_str.clone());
                if current_guid_is_permalink
                    && current_item_permalink.is_none()
                    && guid_looks_like_url(&text_str)
                {
                    *current_item_permalink =
                        Some(resolve_url(&text_str, home_page_url.as_deref()));
                }
            }
            b"pubDate" | b"date" if current_item_date.is_none() => {
                *current_item_date = parse_date_bytes(text_str.as_bytes());
            }
            b"creator" | b"author" => {
                let mut author = Author {
                    name: None,
                    url: None,
                    avatar_url: None,
                    email: None,
                };
                if text_str.contains('@') {
                    author.email = Some(text_str);
                } else if text_str.starts_with("http") {
                    author.url = Some(text_str);
                } else {
                    author.name = Some(text_str);
                }
                current_item_authors.push(author);
            }
            _ => {}
        }
    } else if in_channel_image {
        if current_tag == b"url" && icon_url.is_none() {
            *icon_url = Some(text_str);
        }
    } else {
        match current_tag {
            b"title" if title.is_none() => {
                *title = Some(text_str);
            }
            b"link" if home_page_url.is_none() => {
                *home_page_url = Some(text_str);
            }
            b"language" if language.is_none() => {
                *language = Some(text_str);
            }
            _ => {}
        }
    }
}

/// Prefixes whose elements the RSS parser legitimately consumes inside an
/// `<item>`: Dublin Core (`dc:creator` / `dc:date`), `content:encoded`, and
/// MRSS (`media:content` / `media:thumbnail` / `media:group`). Any other
/// prefixed element inside an item is a namespaced extension section
/// (Shopify's `<s:variant>`, Google Merchant's `<g:*>`, etc.) whose subtree
/// we skip so a nested unprefixed element (e.g. `<title>Default Title</title>`)
/// can't be mistaken for the item's own field. Port of NNW #4422; our set is
/// wider than NNW's dc/content/source because NNW's RSS parser never consumes
/// MRSS, but ours does.
fn rss_prefix_is_consumed(prefix: &[u8]) -> bool {
    matches!(prefix, b"dc" | b"content" | b"media")
}

fn parse_rss(data: &[u8], feed_url: &str, is_rdf: bool) -> Result<ParsedFeed> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut title = None;
    let mut home_page_url = None;
    let mut language: Option<String> = None;
    let mut icon_url: Option<String> = None;
    let mut items = Vec::new();

    let mut in_item = false;
    // Inside `<channel><image>...</image></channel>` — capture child `<url>`
    // as the channel icon. Set on `<image>` Start, cleared on `</image>`.
    let mut in_channel_image = false;

    let mut current_item_guid = None;
    let mut current_item_title = None;
    let mut current_item_body = None;
    let mut current_item_link = None;
    let mut current_item_permalink = None;
    let mut current_item_date = None;
    let mut current_item_authors = Vec::new();
    let mut current_item_attachments: Vec<Attachment> = Vec::new();
    // Tracks `<guid isPermaLink="false">` — mirrors NNW's handleGuid attribute check.
    let mut current_guid_is_permalink = true;

    // NNW #4422: depth inside a skipped namespaced section within an item.
    // >0 means we're swallowing a subtree (e.g. `<s:variant>...</s:variant>`).
    let mut namespace_element_depth: u32 = 0;

    let mut current_tag = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                if namespace_element_depth > 0 {
                    namespace_element_depth += 1;
                    current_tag.clear();
                    buf.clear();
                    continue;
                }
                let name = e.local_name();
                let name_ref = name.as_ref();

                // NNW #4422: a namespaced child inside an item (and its whole
                // subtree) is skipped so nested unprefixed elements can't be
                // read as the item's own fields. dc/content/media stay live.
                if in_item
                    && let Some(prefix) = e.name().prefix()
                    && !rss_prefix_is_consumed(prefix.as_ref())
                {
                    namespace_element_depth = 1;
                    current_tag.clear();
                    buf.clear();
                    continue;
                }

                current_tag = name_ref.to_vec();

                if name_ref == b"item" {
                    in_item = true;
                    current_item_guid = None;
                    current_item_title = None;
                    current_item_body = None;
                    current_item_link = None;
                    current_item_permalink = None;
                    current_item_date = None;
                    current_item_authors.clear();
                    current_item_attachments.clear();
                    current_guid_is_permalink = true;

                    if is_rdf {
                        for attr in e.attributes().filter_map(|a| a.ok()) {
                            if attr.key.as_ref() == b"rdf:about"
                                && let Ok(val) = str::from_utf8(attr.value.as_ref())
                            {
                                let val_str = val.to_string();
                                current_item_guid = Some(val_str.clone());
                                current_item_permalink = Some(val_str);
                            }
                        }
                    }
                } else if !in_item && name_ref == b"image" {
                    in_channel_image = true;
                } else if in_item && name_ref == b"enclosure" {
                    if let Some(att) = attachment_from_attrs(e, b"length") {
                        current_item_attachments.push(att);
                    }
                } else if in_item && (name_ref == b"content" || name_ref == b"thumbnail") {
                    // Heuristic: MRSS `<media:content>` / `<media:thumbnail>`
                    // always carry a `url` attribute. `<content:encoded>` does
                    // not, so this distinguishes them despite quick-xml
                    // stripping the namespace prefix from local_name.
                    if e.attributes()
                        .filter_map(|a| a.ok())
                        .any(|a| a.key.as_ref() == b"url")
                        && let Some(att) = attachment_from_attrs(e, b"fileSize")
                    {
                        current_item_attachments.push(att);
                    }
                } else if in_item && name_ref == b"guid" {
                    // RSS 2.0: <guid isPermaLink="false">...</guid> — caller is telling us
                    // this guid is NOT a usable URL. NNW honors it explicitly.
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        if attr.key.as_ref().eq_ignore_ascii_case(b"ispermalink")
                            && let Ok(val) = str::from_utf8(attr.value.as_ref())
                            && val.eq_ignore_ascii_case("false")
                        {
                            current_guid_is_permalink = false;
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                if namespace_element_depth > 0 {
                    buf.clear();
                    continue;
                }
                if in_item
                    && let Some(prefix) = e.name().prefix()
                    && !rss_prefix_is_consumed(prefix.as_ref())
                {
                    buf.clear();
                    continue;
                }
                let name = e.local_name();
                let name_ref = name.as_ref();
                if in_item && name_ref == b"enclosure" {
                    if let Some(att) = attachment_from_attrs(e, b"length") {
                        current_item_attachments.push(att);
                    }
                } else if in_item
                    && (name_ref == b"content" || name_ref == b"thumbnail")
                    && e.attributes()
                        .filter_map(|a| a.ok())
                        .any(|a| a.key.as_ref() == b"url")
                    && let Some(att) = attachment_from_attrs(e, b"fileSize")
                {
                    current_item_attachments.push(att);
                }
            }
            Ok(Event::Text(ref e)) => {
                if namespace_element_depth > 0 {
                    buf.clear();
                    continue;
                }
                let Ok(text) = e.unescape() else { continue };
                let text_str = text.to_string();
                rss_handle_text_or_cdata(
                    text_str,
                    in_item,
                    in_channel_image,
                    &current_tag,
                    current_guid_is_permalink,
                    &mut current_item_title,
                    &mut current_item_link,
                    &mut current_item_body,
                    &mut current_item_guid,
                    &mut current_item_permalink,
                    &mut current_item_date,
                    &mut current_item_authors,
                    &mut icon_url,
                    &mut title,
                    &mut home_page_url,
                    &mut language,
                );
            }
            Ok(Event::CData(ref e)) => {
                if namespace_element_depth > 0 {
                    buf.clear();
                    continue;
                }
                // CDATA carries raw bytes — no entity decoding needed.
                // Sacha Chua's blog and most WordPress sites wrap their
                // <description> and <content:encoded> bodies in CDATA;
                // before this branch existed those bodies were dropped
                // on the floor entirely (empty article panes for the
                // majority of feeds we see in the wild).
                let text_str = String::from_utf8_lossy(e.as_ref()).into_owned();
                rss_handle_text_or_cdata(
                    text_str,
                    in_item,
                    in_channel_image,
                    &current_tag,
                    current_guid_is_permalink,
                    &mut current_item_title,
                    &mut current_item_link,
                    &mut current_item_body,
                    &mut current_item_guid,
                    &mut current_item_permalink,
                    &mut current_item_date,
                    &mut current_item_authors,
                    &mut icon_url,
                    &mut title,
                    &mut home_page_url,
                    &mut language,
                );
            }
            Ok(Event::End(ref e)) => {
                if namespace_element_depth > 0 {
                    namespace_element_depth -= 1;
                    current_tag.clear();
                    buf.clear();
                    continue;
                }
                let name = e.local_name();
                let name_ref = name.as_ref();
                current_tag.clear();

                if name_ref == b"image" && in_channel_image {
                    in_channel_image = false;
                }

                // Stop at the document close tag — NNW's endRSSFound. Defensive
                // against trailing junk in malformed feeds.
                if (name_ref == b"rss") || (is_rdf && name_ref == b"RDF") {
                    break;
                }

                if name_ref == b"item" {
                    in_item = false;
                    let unique_id = calculate_id(
                        current_item_guid.as_deref(),
                        current_item_permalink.as_deref(),
                        current_item_link.as_deref(),
                        current_item_title.as_deref(),
                        current_item_body.as_deref(),
                        current_item_date,
                    );

                    items.push(ParsedItem {
                        id: unique_id,
                        title: current_item_title.take(),
                        content_html: current_item_body.take(),
                        content_text: None,
                        url: current_item_permalink.take(),
                        external_url: current_item_link.take(),
                        summary: None,
                        image_url: None,
                        date_published: current_item_date.take(),
                        date_modified: None,
                        authors: std::mem::take(&mut current_item_authors),
                        attachments: std::mem::take(&mut current_item_attachments),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => (),
        }
        buf.clear();
    }

    let resolved_icon =
        icon_url.map(|u| resolve_url(&u, home_page_url.as_deref().or(Some(feed_url))));
    Ok(ParsedFeed {
        title,
        home_page_url,
        feed_url: Some(feed_url.to_string()),
        icon_url: resolved_icon,
        language,
        items,
    })
}

/// Atom `<link>` `rel` attribute. NNW handles `alternate`, `related`, and
/// `enclosure`. Enclosures need a model addition and are deferred to Phase 11.
#[derive(Debug, Clone, Copy)]
enum AtomLinkRel {
    Alternate,
    Related,
    Enclosure,
    Other,
}

impl AtomLinkRel {
    fn from_str(s: &str) -> Self {
        match s {
            "alternate" => Self::Alternate,
            "related" => Self::Related,
            "enclosure" => Self::Enclosure,
            _ => Self::Other,
        }
    }
}

/// Per-event state threaded through Atom parse callbacks. Bundled so
/// `handle_atom_link_attributes` doesn't carry an argument-list long enough
/// to trip clippy's `too_many_arguments`.
struct AtomLinkCtx<'a> {
    in_item: bool,
    in_author: bool,
    in_source: bool,
    /// The xml:base in effect for this link element, if any.
    xml_base: Option<&'a url::Url>,
    home_page_url: &'a mut Option<String>,
    current_item_permalink: &'a mut Option<String>,
    current_item_link: &'a mut Option<String>,
    current_item_attachments: &'a mut Vec<Attachment>,
    current_author: &'a mut Option<MutableAuthor>,
}

fn handle_atom_link_attributes(e: &quick_xml::events::BytesStart, ctx: &mut AtomLinkCtx<'_>) {
    // Inside <source> (a republished entry's origin feed) we suppress link
    // handling — those links describe the original source, not this entry.
    if ctx.in_source {
        return;
    }
    let mut href = None;
    let mut rel = AtomLinkRel::Alternate;
    let mut mime_type: Option<String> = None;
    let mut size_in_bytes: Option<i64> = None;
    let mut title: Option<String> = None;
    for attr in e.attributes().filter_map(|a| a.ok()) {
        match attr.key.as_ref() {
            b"href" => {
                href = str::from_utf8(attr.value.as_ref())
                    .ok()
                    .map(|s| s.to_string());
            }
            b"rel" => {
                if let Ok(val) = str::from_utf8(attr.value.as_ref()) {
                    rel = AtomLinkRel::from_str(val);
                }
            }
            b"type" => {
                mime_type = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
            }
            b"length" => {
                size_in_bytes = str::from_utf8(attr.value.as_ref())
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .filter(|n| *n > 0);
            }
            b"title" => {
                title = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
            }
            _ => {}
        }
    }
    // An empty href must not resolve to the base or home URL — upstream's
    // #5088 suite pins that (WordPress's wp-atom.php feeds carry them).
    let Some(h) = href.filter(|h| !h.is_empty()) else {
        return;
    };

    if ctx.in_author {
        // <link> inside <author> is non-standard, but if encountered, treat
        // href as the author's URL (matches NNW's generous parsing).
        if let Some(author) = ctx.current_author.as_mut()
            && author.url.is_none()
        {
            author.url = Some(match ctx.xml_base {
                Some(base) => resolved_url_string(h, Some(base)),
                None => resolve_url(&h, ctx.home_page_url.as_deref()),
            });
        }
        return;
    }

    // xml:base applies before the home-page fallback (NNW `1d54877be`).
    let resolved = match ctx.xml_base {
        Some(base) => resolved_url_string(h, Some(base)),
        None => resolve_url(&h, ctx.home_page_url.as_deref()),
    };
    if ctx.in_item {
        match rel {
            AtomLinkRel::Alternate if ctx.current_item_permalink.is_none() => {
                *ctx.current_item_permalink = Some(resolved);
            }
            AtomLinkRel::Related if ctx.current_item_link.is_none() => {
                *ctx.current_item_link = Some(resolved);
            }
            AtomLinkRel::Enclosure if !resolved.is_empty() => {
                ctx.current_item_attachments.push(Attachment {
                    url: resolved,
                    mime_type,
                    title,
                    size_in_bytes,
                    duration_in_seconds: None,
                });
            }
            _ => {}
        }
    } else if matches!(rel, AtomLinkRel::Alternate) && ctx.home_page_url.is_none() {
        *ctx.home_page_url = Some(resolved);
    }
}

#[derive(Default)]
struct MutableAuthor {
    name: Option<String>,
    email: Option<String>,
    url: Option<String>,
}

impl MutableAuthor {
    fn build(self) -> Option<Author> {
        if self.name.is_none() && self.email.is_none() && self.url.is_none() {
            return None;
        }
        Some(Author {
            name: self.name,
            url: self.url,
            avatar_url: None,
            email: self.email,
        })
    }
}

/// Atom `<content type="xhtml">` / `<summary type="xhtml">`: capture the raw
/// inner XML between the tags (including the spec-required `<div xmlns="…">`
/// wrapper) and re-serialize it as a string so it rides the same render
/// pipeline as `type="html"` payloads. Port of NNW's
/// `XMLSAXParser.captureRawInnerContent`. Returns the inner string on the
/// matching `End`, or `None` on EOF/parse error.
fn capture_atom_xhtml_inner<R: std::io::BufRead>(reader: &mut Reader<R>) -> Option<String> {
    let mut out: Vec<u8> = Vec::with_capacity(256);
    let mut writer = quick_xml::Writer::new(&mut out);
    let mut buf = Vec::new();
    let mut depth: i32 = 1;
    // The outer parser uses trim_text(true) so titles / IDs come back clean.
    // For xhtml capture we need raw whitespace preserved or "foo <em>bar</em>"
    // collapses to "foo<em>bar</em>". Flip it off, capture, restore.
    reader.config_mut().trim_text(false);
    let result = loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let _ = writer.write_event(Event::Start(e));
            }
            Ok(Event::End(e)) => {
                depth -= 1;
                if depth == 0 {
                    break Some(String::from_utf8_lossy(&out).into_owned());
                }
                let _ = writer.write_event(Event::End(e));
            }
            Ok(Event::Empty(e)) => {
                let _ = writer.write_event(Event::Empty(e));
            }
            Ok(Event::Text(e)) => {
                let _ = writer.write_event(Event::Text(e));
            }
            Ok(Event::CData(e)) => {
                let _ = writer.write_event(Event::CData(e));
            }
            Ok(Event::Comment(e)) => {
                let _ = writer.write_event(Event::Comment(e));
            }
            Ok(Event::Eof) | Err(_) => break None,
            _ => (),
        }
    };
    reader.config_mut().trim_text(true);
    result
}

/// Returns true when an Atom Start tag carries `type="xhtml"`.
fn atom_type_is_xhtml(e: &quick_xml::events::BytesStart) -> bool {
    for attr in e.attributes().filter_map(|a| a.ok()) {
        if attr.key.as_ref() == b"type"
            && let Ok(val) = str::from_utf8(attr.value.as_ref())
            && val.eq_ignore_ascii_case("xhtml")
        {
            return true;
        }
    }
    false
}

/// Same shape as `rss_handle_text_or_cdata` but for Atom — dispatches
/// a Text or CDATA payload into the right per-author / per-entry /
/// channel field. Atom CDATA is rarer than RSS but real (some Hugo and
/// Jekyll templates produce it).
#[allow(clippy::too_many_arguments)]
fn atom_handle_text_or_cdata(
    text_str: String,
    in_item: bool,
    in_author: bool,
    in_source: bool,
    current_tag: &[u8],
    xml_base: Option<url::Url>,
    content_type: Option<&str>,
    current_author: &mut Option<MutableAuthor>,
    current_item_title: &mut Option<String>,
    current_item_body: &mut Option<String>,
    current_item_summary: &mut Option<String>,
    current_item_guid: &mut Option<String>,
    current_item_date: &mut Option<chrono::DateTime<chrono::Utc>>,
    current_item_date_modified: &mut Option<chrono::DateTime<chrono::Utc>>,
    title: &mut Option<String>,
    icon_url: &mut Option<String>,
    logo_url: &mut Option<String>,
) {
    // Author body: collect into the open MutableAuthor.
    if in_author && let Some(author) = current_author.as_mut() {
        match current_tag {
            b"name" => author.name = Some(text_str),
            b"email" => author.email = Some(text_str),
            // xml:base applies to <uri> like any URL field
            // (NNW `1d54877be`).
            b"uri" => author.url = Some(resolved_url_string(text_str, xml_base.as_ref())),
            _ => {}
        }
        return;
    }
    // <source> wraps a republished entry's original feed metadata —
    // ignore everything inside it so it doesn't pollute our entry.
    if in_source {
        return;
    }
    if in_item {
        match current_tag {
            b"title" if current_item_title.is_none() => {
                *current_item_title = Some(text_str);
            }
            b"content" if current_item_body.is_none() => {
                *current_item_body = Some(resolved_html_body(
                    text_str,
                    content_type,
                    xml_base.as_ref(),
                ));
            }
            // NNW d6eb8df7d: <summary> goes into a SEPARATE summary field;
            // it does NOT promote into body when content is missing. The
            // article-render fallback chain in `window.rs` already prefers
            // content_html → content_text → summary, so summary-only feeds
            // still render correctly without the parser conflating fields.
            b"summary" if current_item_summary.is_none() => {
                *current_item_summary = Some(resolved_html_body(
                    text_str,
                    content_type,
                    xml_base.as_ref(),
                ));
            }
            b"id" if current_item_guid.is_none() => {
                *current_item_guid = Some(text_str);
            }
            b"published" | b"issued" if current_item_date.is_none() => {
                *current_item_date = parse_date_bytes(text_str.as_bytes());
            }
            b"updated" | b"modified" if current_item_date_modified.is_none() => {
                *current_item_date_modified = parse_date_bytes(text_str.as_bytes());
            }
            _ => {}
        }
    } else {
        match current_tag {
            b"title" if title.is_none() => *title = Some(text_str),
            // Feed-level icon / logo resolve against the base in effect
            // at their element (NNW `1d54877be`); the end-of-parse
            // home-page fallback below still applies when no base does.
            b"icon" if icon_url.is_none() => {
                *icon_url = Some(resolved_url_string(text_str, xml_base.as_ref()))
            }
            b"logo" if logo_url.is_none() => {
                *logo_url = Some(resolved_url_string(text_str, xml_base.as_ref()))
            }
            _ => {}
        }
    }
}

/// RFC 4287 / XML Base: the effective base for an element is its
/// `xml:base` resolved against the nearest enclosing base — the
/// document (feed) URL when there is none. A garbage or non-http base
/// (`tag:`, `urn:`) would poison resolution, so it inherits instead.
/// Port of NNW `AtomDelegate.pushBaseURL` (`1d54877be`).
fn effective_base_url(
    inherited: Option<url::Url>,
    attrs: &quick_xml::events::BytesStart,
    document_base: Option<&url::Url>,
) -> Option<url::Url> {
    let xml_base = attrs.attributes().filter_map(|a| a.ok()).find_map(|a| {
        if a.key.as_ref() == b"xml:base" {
            a.unescape_value().ok().map(|v| v.to_string())
        } else {
            None
        }
    });
    let Some(xml_base) = xml_base.filter(|v| !v.is_empty()) else {
        return inherited;
    };
    let outer = inherited.as_ref().or(document_base)?;
    match outer.join(&xml_base) {
        Ok(resolved) if matches!(resolved.scheme(), "http" | "https") => Some(resolved),
        _ => inherited,
    }
}

/// Resolves `s` against an `xml:base` when one is in effect and the
/// value isn't already absolute. Port of NNW `resolvedURLString`: an
/// absolute (http/https) value passes through, then the base applies,
/// and the original string survives any failure so the pre-existing
/// home-page/feed fallbacks downstream stay intact.
fn resolved_url_string(s: String, xml_base: Option<&url::Url>) -> String {
    if s.to_ascii_lowercase().starts_with("http") {
        return s;
    }
    if let Some(base) = xml_base
        && let Ok(resolved) = base.join(&s)
    {
        return resolved.to_string();
    }
    s
}

/// Rewrites relative URLs in an escaped-HTML body when an `xml:base` is
/// in effect. RFC 4287 allows a MIME type as well as the "html"
/// keyword; `type="text"` (and the absent type, which means text) is
/// never rewritten. Port of NNW `resolvedHTMLBody`.
fn resolved_html_body(
    html: String,
    content_type: Option<&str>,
    xml_base: Option<&url::Url>,
) -> String {
    let Some(base) = xml_base else {
        return html;
    };
    match content_type {
        Some(t) if t == "html" || t == "text/html" => resolving_relative_urls(&html, base),
        _ => html,
    }
}

fn parse_atom(data: &[u8], feed_url: &str) -> Result<ParsedFeed> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut title = None;
    let mut home_page_url = None;
    let mut language: Option<String> = None;
    let mut icon_url: Option<String> = None;
    let mut logo_url: Option<String> = None;
    let mut items = Vec::new();
    let mut root_author: Option<Author> = None;

    let mut in_item = false;
    let mut in_author = false;
    let mut in_source = false;

    let mut current_item_guid = None;
    let mut current_item_title = None;
    let mut current_item_body = None;
    let mut current_item_summary = None;
    let mut current_item_link = None;
    let mut current_item_permalink = None;
    let mut current_item_date = None;
    let mut current_item_date_modified = None;
    let mut current_item_authors: Vec<Author> = Vec::new();
    let mut current_item_attachments: Vec<Attachment> = Vec::new();
    let mut current_author: Option<MutableAuthor> = None;

    // NNW #4422: depth inside a skipped namespaced section within an entry.
    // Atom's own elements are all unprefixed, so any prefixed child of an
    // entry is an extension we swallow whole — no exceptions, unlike RSS.
    let mut namespace_element_depth: u32 = 0;

    let mut current_tag: Vec<u8> = Vec::new();

    // Atom xml:base (NNW `1d54877be`, #5088): the effective base per open
    // element, pushed at every Start and popped at every End so the two
    // stay exactly in balance. `None` entries mean "inherit only".
    let document_base: Option<url::Url> = url::Url::parse(feed_url).ok();
    let mut base_stack: Vec<Option<url::Url>> = Vec::new();
    // The `type` attribute of the open content/summary element, for the
    // escaped-HTML resolution gate (html / text/html only).
    let mut current_content_type: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                // Every element pushes one entry (the inherited base when
                // it carries no usable xml:base), so the End arm can pop
                // unconditionally. Skipped namespaced-subtree elements
                // push their inherited base too: nothing inside them is
                // consumed, but the balance is what matters.
                let inherited = base_stack.last().cloned().flatten();
                base_stack.push(effective_base_url(inherited, e, document_base.as_ref()));

                if namespace_element_depth > 0 {
                    namespace_element_depth += 1;
                    current_tag.clear();
                    buf.clear();
                    continue;
                }
                let name = e.local_name();
                let name_ref = name.as_ref();

                // NNW #4422: skip any namespaced child (and its subtree) inside
                // an entry so a nested unprefixed element can't masquerade as
                // one of the entry's own fields.
                if in_item && e.name().prefix().is_some() {
                    namespace_element_depth = 1;
                    current_tag.clear();
                    buf.clear();
                    continue;
                }

                current_tag = name_ref.to_vec();

                if name_ref == b"feed" && language.is_none() {
                    // Atom feed-root xml:lang. quick-xml's `local_name`
                    // already strips namespace prefixes; the attribute is
                    // raw `xml:lang`.
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        if attr.key.as_ref() == b"xml:lang"
                            && let Ok(val) = str::from_utf8(attr.value.as_ref())
                        {
                            language = Some(val.to_string());
                        }
                    }
                }
                if name_ref == b"entry" {
                    in_item = true;
                    in_source = false;
                    current_content_type = None;
                    current_item_guid = None;
                    current_item_title = None;
                    current_item_body = None;
                    current_item_summary = None;
                    current_item_link = None;
                    current_item_permalink = None;
                    current_item_date = None;
                    current_item_date_modified = None;
                    current_item_authors.clear();
                    current_item_attachments.clear();
                } else if name_ref == b"source" && in_item {
                    in_source = true;
                } else if in_item
                    && !in_source
                    && (name_ref == b"content" || name_ref == b"summary")
                {
                    // The element's own `type` attribute gates how the text
                    // is interpreted, recorded for the Text/CData handlers
                    // (html / text/html bodies resolve relative URLs; text
                    // and xhtml take different paths).
                    current_content_type = e.attributes().filter_map(|a| a.ok()).find_map(|a| {
                        if a.key.as_ref() == b"type" {
                            a.unescape_value().ok().map(|v| v.to_string())
                        } else {
                            None
                        }
                    });

                    if atom_type_is_xhtml(e) {
                        // type="xhtml" payloads contain inline XML (per RFC 4287,
                        // wrapped in a single <div xmlns="...">). Capture the raw
                        // inner XML and feed it to the renderer as HTML. Without
                        // this branch quick-xml hands us only the bare text nodes,
                        // which loses every tag. Per NNW d6eb8df7d: content goes
                        // to body, summary goes to summary — they don't share a
                        // slot anymore.
                        //
                        // The element's own base is on the stack (pushed above),
                        // matching upstream: "the content element is still on the
                        // stacks here". An xml:base on an element inside the
                        // captured content is invisible (pass-through emits no
                        // events) and unsupported, same as upstream. The capture
                        // consumes the element's End event, so pop its base entry
                        // here instead of at the End arm.
                        let inner = capture_atom_xhtml_inner(&mut reader);
                        let own_base = base_stack.pop().flatten();
                        current_content_type = None;
                        if let Some(inner) = inner {
                            let inner = match own_base.as_ref() {
                                Some(base) => resolving_relative_urls(&inner, base),
                                None => inner,
                            };
                            if name_ref == b"content" && current_item_body.is_none() {
                                current_item_body = Some(inner);
                            } else if name_ref == b"summary" && current_item_summary.is_none() {
                                current_item_summary = Some(inner);
                            }
                        }
                        current_tag.clear();
                    }
                } else if name_ref == b"author" {
                    in_author = true;
                    current_author = Some(MutableAuthor::default());
                } else if name_ref == b"link" {
                    // The element's own base is already on the stack (pushed
                    // at arm entry) — reading the stack is the whole job
                    // here. Re-applying effective_base_url would join a
                    // relative xml:base onto itself.
                    let own_base = base_stack.last().cloned().flatten();
                    let mut ctx = AtomLinkCtx {
                        in_item,
                        in_author,
                        in_source,
                        xml_base: own_base.as_ref(),
                        home_page_url: &mut home_page_url,
                        current_item_permalink: &mut current_item_permalink,
                        current_item_link: &mut current_item_link,
                        current_item_attachments: &mut current_item_attachments,
                        current_author: &mut current_author,
                    };
                    handle_atom_link_attributes(e, &mut ctx);
                }
            }
            Ok(Event::Empty(ref e)) => {
                if namespace_element_depth > 0 {
                    buf.clear();
                    continue;
                }
                if in_item && e.name().prefix().is_some() {
                    buf.clear();
                    continue;
                }
                let name = e.local_name();
                if name.as_ref() == b"link" {
                    // Empty elements never push the stack (self-contained),
                    // but their OWN xml:base applies to their attributes.
                    let own_base = effective_base_url(
                        base_stack.last().cloned().flatten(),
                        e,
                        document_base.as_ref(),
                    );
                    let mut ctx = AtomLinkCtx {
                        in_item,
                        in_author,
                        in_source,
                        xml_base: own_base.as_ref(),
                        home_page_url: &mut home_page_url,
                        current_item_permalink: &mut current_item_permalink,
                        current_item_link: &mut current_item_link,
                        current_item_attachments: &mut current_item_attachments,
                        current_author: &mut current_author,
                    };
                    handle_atom_link_attributes(e, &mut ctx);
                }
            }
            Ok(Event::Text(ref e)) => {
                if namespace_element_depth > 0 {
                    buf.clear();
                    continue;
                }
                let Ok(text) = e.unescape() else { continue };
                let text_str = text.to_string();
                atom_handle_text_or_cdata(
                    text_str,
                    in_item,
                    in_author,
                    in_source,
                    &current_tag,
                    base_stack.last().cloned().flatten(),
                    current_content_type.as_deref(),
                    &mut current_author,
                    &mut current_item_title,
                    &mut current_item_body,
                    &mut current_item_summary,
                    &mut current_item_guid,
                    &mut current_item_date,
                    &mut current_item_date_modified,
                    &mut title,
                    &mut icon_url,
                    &mut logo_url,
                );
            }
            Ok(Event::CData(ref e)) => {
                if namespace_element_depth > 0 {
                    buf.clear();
                    continue;
                }
                // Same CDATA fix as parse_rss: Atom feeds with
                // `<title><![CDATA[…]]></title>` or
                // `<content type="html"><![CDATA[…]]></content>` were
                // dropping their content entirely. Less common than RSS
                // but still encountered (e.g. some Hugo + Jekyll
                // templates).
                let text_str = String::from_utf8_lossy(e.as_ref()).into_owned();
                atom_handle_text_or_cdata(
                    text_str,
                    in_item,
                    in_author,
                    in_source,
                    &current_tag,
                    base_stack.last().cloned().flatten(),
                    current_content_type.as_deref(),
                    &mut current_author,
                    &mut current_item_title,
                    &mut current_item_body,
                    &mut current_item_summary,
                    &mut current_item_guid,
                    &mut current_item_date,
                    &mut current_item_date_modified,
                    &mut title,
                    &mut icon_url,
                    &mut logo_url,
                );
            }
            Ok(Event::End(ref e)) => {
                // Every Start pushed one base entry; pop it before any
                // branch so the skip / feed-break paths stay balanced.
                base_stack.pop();
                if namespace_element_depth > 0 {
                    namespace_element_depth -= 1;
                    current_tag.clear();
                    buf.clear();
                    continue;
                }
                let name = e.local_name();
                let name_ref = name.as_ref();
                current_tag.clear();

                if name_ref == b"content" || name_ref == b"summary" {
                    current_content_type = None;
                }

                // NNW endFeedFound — stop scanning at </feed>.
                if name_ref == b"feed" {
                    break;
                }

                if name_ref == b"author" {
                    in_author = false;
                    if let Some(built) = current_author.take().and_then(MutableAuthor::build) {
                        if in_item {
                            current_item_authors.push(built);
                        } else if root_author.is_none() {
                            root_author = Some(built);
                        }
                    }
                    continue;
                }

                if name_ref == b"source" {
                    in_source = false;
                    continue;
                }

                if name_ref == b"entry" {
                    in_item = false;
                    in_source = false;
                    let unique_id = calculate_id(
                        current_item_guid.as_deref(),
                        current_item_permalink.as_deref(),
                        current_item_link.as_deref(),
                        current_item_title.as_deref(),
                        current_item_body.as_deref(),
                        current_item_date,
                    );

                    items.push(ParsedItem {
                        id: unique_id,
                        title: current_item_title.take(),
                        content_html: current_item_body.take(),
                        content_text: None,
                        url: current_item_permalink.take(),
                        external_url: current_item_link.take(),
                        summary: current_item_summary.take(),
                        image_url: None,
                        date_published: current_item_date.take(),
                        date_modified: current_item_date_modified.take(),
                        authors: std::mem::take(&mut current_item_authors),
                        attachments: std::mem::take(&mut current_item_attachments),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => (),
        }
        buf.clear();
    }

    // NNW's rootAuthor propagation: feed-level <author> applies to any entry
    // that didn't declare its own.
    if let Some(author) = root_author {
        for item in items.iter_mut() {
            if item.authors.is_empty() {
                item.authors.push(author.clone());
            }
        }
    }

    let resolved_icon = icon_url
        .or(logo_url)
        .map(|u| resolve_url(&u, home_page_url.as_deref().or(Some(feed_url))));
    Ok(ParsedFeed {
        title,
        home_page_url,
        feed_url: Some(feed_url.to_string()),
        // NNW prefers <icon> over <logo> for the channel icon.
        icon_url: resolved_icon,
        language,
        items,
    })
}

pub fn parse_opml(data: &[u8]) -> Result<OpmlDocument> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut title = None;
    let mut stack: Vec<OpmlItem> = Vec::new();
    let mut root_items = Vec::new();
    let mut collecting_title = false;

    // simplistic check for opml
    let limit = std::cmp::min(data.len(), 4096);
    let mut found_opml = false;
    let mut i = 0;
    while i + 4 < limit {
        if data[i] == b'<' && data[i + 1..=i + 4].eq_ignore_ascii_case(b"opml") {
            found_opml = true;
            break;
        }
        i += 1;
    }
    if !found_opml {
        return Err(ParseError::UnknownFormat.into());
    }

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                if name.as_ref() == b"title" {
                    collecting_title = stack.is_empty();
                } else if name.as_ref() == b"outline" {
                    let mut item = OpmlItem {
                        title: None,
                        text: None,
                        xml_url: None,
                        html_url: None,
                        children: Vec::new(),
                    };
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        let key = attr.key.as_ref();
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        if key == b"title" {
                            item.title = Some(val);
                        } else if key == b"text" {
                            item.text = Some(val);
                        } else if key == b"xmlUrl" {
                            item.xml_url = Some(val);
                        } else if key == b"htmlUrl" {
                            item.html_url = Some(val);
                        }
                    }
                    stack.push(item);
                }
            }
            Ok(Event::Empty(ref e)) if e.name().as_ref() == b"outline" => {
                let mut item = OpmlItem {
                    title: None,
                    text: None,
                    xml_url: None,
                    html_url: None,
                    children: Vec::new(),
                };
                for attr in e.attributes().filter_map(|a| a.ok()) {
                    let key = attr.key.as_ref();
                    let val = String::from_utf8_lossy(&attr.value).to_string();
                    if key == b"title" {
                        item.title = Some(val);
                    } else if key == b"text" {
                        item.text = Some(val);
                    } else if key == b"xmlUrl" {
                        item.xml_url = Some(val);
                    } else if key == b"htmlUrl" {
                        item.html_url = Some(val);
                    }
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(item);
                } else {
                    root_items.push(item);
                }
            }
            Ok(Event::Text(ref e)) => {
                if collecting_title && let Ok(t) = e.unescape() {
                    title = Some(t.to_string());
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                if name.as_ref() == b"title" {
                    collecting_title = false;
                } else if name.as_ref() == b"outline"
                    && let Some(item) = stack.pop()
                {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(item);
                    } else {
                        root_items.push(item);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => (),
        }
        buf.clear();
    }

    Ok(OpmlDocument {
        title,
        items: root_items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atom_author_name_email_uri_captured() {
        // Regression: the previous parse_atom fired on any <name> text and
        // produced a bogus Author. Verify we only emit an Author when an
        // <author> wrapper is present, and that email + uri propagate.
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Site</title>
  <entry>
    <id>1</id>
    <title>Post</title>
    <author><name>Jane</name><email>jane@example.com</email><uri>https://jane.example</uri></author>
    <content>hi</content>
    <updated>2020-01-01T00:00:00Z</updated>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(feed.items.len(), 1);
        let authors = &feed.items[0].authors;
        assert_eq!(authors.len(), 1);
        assert_eq!(authors[0].name.as_deref(), Some("Jane"));
        assert_eq!(authors[0].email.as_deref(), Some("jane@example.com"));
        assert_eq!(authors[0].url.as_deref(), Some("https://jane.example"));
    }

    #[test]
    fn atom_root_author_propagates_to_authorless_items() {
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Site</title>
  <author><name>Feed Author</name></author>
  <entry>
    <id>1</id>
    <title>Post 1</title>
  </entry>
  <entry>
    <id>2</id>
    <title>Post 2</title>
    <author><name>Per-Entry</name></author>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(feed.items.len(), 2);
        assert_eq!(
            feed.items[0].authors[0].name.as_deref(),
            Some("Feed Author")
        );
        assert_eq!(feed.items[1].authors[0].name.as_deref(), Some("Per-Entry"));
    }

    #[test]
    fn atom_source_block_does_not_pollute_entry() {
        // <source> carries the ORIGINAL feed's metadata when an entry has been
        // republished. Its <title>/<id>/<link> must not overwrite ours.
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Planet</title>
  <entry>
    <id>entry-42</id>
    <title>Real Title</title>
    <updated>2024-01-01T00:00:00Z</updated>
    <source>
      <id>original-source-id</id>
      <title>Wrong Title</title>
      <link href="https://wrong.example/" rel="alternate"/>
    </source>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(feed.items.len(), 1);
        assert_eq!(feed.items[0].title.as_deref(), Some("Real Title"));
    }

    #[test]
    fn rss_cdata_description_captured_as_body() {
        // Sacha Chua's blog and most WordPress / Jekyll feeds wrap
        // <description> in CDATA. Before the parser learned to
        // handle Event::CData these bodies were silently dropped.
        let xml = br#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Site</title>
  <item>
    <title>Post</title>
    <link>https://example.com/post</link>
    <description><![CDATA[<p>Hello <a href="https://example.com">world</a>.</p>]]></description>
    <pubDate>Mon, 27 Apr 2026 11:29:30 GMT</pubDate>
  </item>
</channel></rss>"#;
        let feed = parse_rss(xml, "https://example.com/feed", false).unwrap();
        assert_eq!(feed.items.len(), 1);
        let body = feed.items[0].content_html.as_deref().unwrap_or("");
        assert!(body.contains("<p>Hello "), "missing CDATA body: {body}");
        assert!(body.contains("<a href"), "missing CDATA link: {body}");
    }

    #[test]
    fn rss_cdata_content_encoded_overrides_description() {
        // <content:encoded> is the full body; <description> is the
        // summary. NNW prefers content:encoded and so do we — confirm
        // it works through CDATA too.
        let xml = br#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel>
  <title>Site</title>
  <item>
    <title>Post</title>
    <description><![CDATA[<p>Short summary.</p>]]></description>
    <content:encoded><![CDATA[<p>Full article body with <strong>everything</strong>.</p>]]></content:encoded>
  </item>
</channel></rss>"#;
        let feed = parse_rss(xml, "https://example.com/feed", false).unwrap();
        let body = feed.items[0].content_html.as_deref().unwrap_or("");
        assert!(body.contains("Full article body"), "body lost: {body}");
        assert!(!body.contains("Short summary"), "summary leaked: {body}");
    }

    #[test]
    fn atom_cdata_content_captured_as_body() {
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Site</title>
  <entry>
    <id>1</id>
    <title>Post</title>
    <updated>2024-01-01T00:00:00Z</updated>
    <content type="html"><![CDATA[<p>Atom CDATA body.</p>]]></content>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        let body = feed.items[0].content_html.as_deref().unwrap_or("");
        assert!(body.contains("Atom CDATA body"), "body lost: {body}");
    }

    #[test]
    fn atom_xhtml_content_captures_inline_html() {
        // Atom RFC 4287 says type="xhtml" payloads contain a single <div>
        // wrapper in the XHTML namespace. Without raw-inner capture we would
        // see only the bare text nodes ("Hello bar") and lose every tag.
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Site</title>
  <entry>
    <id>1</id>
    <title>Post</title>
    <updated>2024-01-01T00:00:00Z</updated>
    <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><p>Hello <em>bar</em>.</p><p>Second.</p></div></content>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(feed.items.len(), 1);
        let body = feed.items[0].content_html.as_deref().unwrap_or("");
        assert!(body.contains("<p>Hello "), "missing <p> tag: {body}");
        assert!(body.contains("<em>bar</em>"), "missing <em>: {body}");
        assert!(
            body.contains("<p>Second."),
            "missing second paragraph: {body}"
        );
    }

    #[test]
    fn atom_xhtml_summary_lands_in_summary_field_not_body() {
        // Per NNW d6eb8df7d (issue: <summary> being conflated with <content>):
        // the summary, even when expressed as type="xhtml", goes into the
        // ParsedItem.summary field. It does NOT promote into content_html
        // even when content is absent — the article-render fallback chain
        // in window.rs prefers content_html → content_text → summary, so
        // summary-only feeds still render correctly downstream.
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Site</title>
  <entry>
    <id>2</id>
    <title>Post</title>
    <updated>2024-01-01T00:00:00Z</updated>
    <summary type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><strong>Inline</strong> summary.</div></summary>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        let item = &feed.items[0];
        assert!(item.content_html.is_none(), "content_html shouldn't be set");
        let summary = item.summary.as_deref().unwrap_or("");
        assert!(
            summary.contains("<strong>Inline</strong>"),
            "lost xhtml summary: {summary}"
        );
    }

    #[test]
    fn atom_summary_and_content_kept_separate() {
        // Both elements present — each lands in its own field. Port of
        // NNW's `summaryAndContentKeptSeparate` test (d6eb8df7d).
        let xml = br#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>urn:t</id>
  <entry>
    <id>urn:t:1</id>
    <title>both</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <summary>The summary.</summary>
    <content>The content.</content>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "http://example.com/").unwrap();
        let item = &feed.items[0];
        assert_eq!(item.content_html.as_deref(), Some("The content."));
        assert_eq!(item.summary.as_deref(), Some("The summary."));
    }

    #[test]
    fn rss_enclosure_and_media_content_become_attachments() {
        let xml = br#"<?xml version="1.0"?>
<rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/">
  <channel>
    <title>Pod</title>
    <item>
      <title>Episode 1</title>
      <enclosure url="https://example.com/ep1.mp3" length="12345" type="audio/mpeg"/>
      <media:content url="https://example.com/ep1.mp4" type="video/mp4" fileSize="98765" duration="600"/>
      <media:thumbnail url="https://example.com/thumb.jpg"/>
    </item>
  </channel>
</rss>"#;
        let feed = parse_rss(xml, "https://example.com/feed", false).unwrap();
        assert_eq!(feed.items.len(), 1);
        let atts = &feed.items[0].attachments;
        assert_eq!(atts.len(), 3);
        assert_eq!(atts[0].url, "https://example.com/ep1.mp3");
        assert_eq!(atts[0].mime_type.as_deref(), Some("audio/mpeg"));
        assert_eq!(atts[0].size_in_bytes, Some(12345));
        assert_eq!(atts[1].url, "https://example.com/ep1.mp4");
        assert_eq!(atts[1].duration_in_seconds, Some(600));
        assert_eq!(atts[2].url, "https://example.com/thumb.jpg");
    }

    #[test]
    fn atom_enclosure_link_becomes_attachment() {
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Pod</title>
  <entry>
    <id>1</id>
    <title>Episode</title>
    <link rel="enclosure" type="audio/mpeg" length="555" href="https://example.com/ep.mp3"/>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        let atts = &feed.items[0].attachments;
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].url, "https://example.com/ep.mp3");
        assert_eq!(atts[0].mime_type.as_deref(), Some("audio/mpeg"));
        assert_eq!(atts[0].size_in_bytes, Some(555));
    }

    #[test]
    fn rss_channel_image_and_language_captured() {
        let xml = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Site</title>
    <language>en-us</language>
    <image>
      <url>https://example.com/logo.png</url>
      <title>Site Logo</title>
    </image>
    <item><title>Hi</title></item>
  </channel>
</rss>"#;
        let feed = parse_rss(xml, "https://example.com/feed", false).unwrap();
        assert_eq!(
            feed.icon_url.as_deref(),
            Some("https://example.com/logo.png")
        );
        assert_eq!(feed.language.as_deref(), Some("en-us"));
    }

    #[test]
    fn atom_icon_logo_and_xml_lang_captured() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xml:lang="ja">
  <title>サイト</title>
  <icon>https://example.com/icon.png</icon>
  <logo>https://example.com/logo.png</logo>
  <entry><id>1</id><title>Post</title></entry>
</feed>"#
            .as_bytes();
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        // <icon> wins over <logo>.
        assert_eq!(
            feed.icon_url.as_deref(),
            Some("https://example.com/icon.png")
        );
        assert_eq!(feed.language.as_deref(), Some("ja"));
    }

    #[test]
    fn atom_logo_used_when_icon_missing() {
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Site</title>
  <logo>https://example.com/logo-only.png</logo>
  <entry><id>1</id><title>Post</title></entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(
            feed.icon_url.as_deref(),
            Some("https://example.com/logo-only.png")
        );
    }

    #[test]
    fn atom_link_href_is_resolved_against_home_page() {
        // <link href="/posts/1"> inside an entry should resolve against the
        // feed's own home page URL when that URL is available.
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Site</title>
  <link href="https://example.com/" rel="alternate"/>
  <entry>
    <id>1</id>
    <title>Post</title>
    <link href="/posts/1" rel="alternate"/>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(
            feed.items[0].url.as_deref(),
            Some("https://example.com/posts/1")
        );
    }

    #[test]
    fn rss_namespaced_section_does_not_overwrite_item_fields() {
        // NNW #4422: an unprefixed <title>Default Title</title> nested inside a
        // namespaced <s:variant> must not overwrite the item's real title,
        // while the dc:/content: elements we DO consume keep parsing.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:content="http://purl.org/rss/1.0/modules/content/"
     xmlns:s="http://jadedpixel.com/-/spec/shopify">
  <channel>
    <title>Test Shop</title>
    <link>https://example.com</link>
    <description>Test feed for #4422.</description>
    <item>
      <title>Real Product Title</title>
      <link>https://example.com/products/1</link>
      <guid>https://example.com/products/1</guid>
      <dc:creator>Jane Maker</dc:creator>
      <content:encoded><![CDATA[<p>Body HTML</p>]]></content:encoded>
      <s:type>Periodical</s:type>
      <s:variant>
        <id>https://example.com/products/1</id>
        <title>Default Title</title>
        <s:price currency="USD">9.99</s:price>
        <s:sku/>
      </s:variant>
    </item>
  </channel>
</rss>"#;
        let feed = parse_rss(xml, "https://example.com/feed", false).unwrap();
        assert_eq!(feed.items.len(), 1);
        let item = &feed.items[0];
        assert_eq!(item.title.as_deref(), Some("Real Product Title"));
        assert_eq!(item.content_html.as_deref(), Some("<p>Body HTML</p>"));
        assert_eq!(item.authors.len(), 1);
        assert_eq!(item.authors[0].name.as_deref(), Some("Jane Maker"));
    }

    #[test]
    fn rss_namespaced_section_before_real_fields_is_ignored() {
        // The harder case the is_none guards can't catch on their own: the
        // namespaced section precedes the real elements and carries a nested
        // <author>. Without the subtree skip the bogus title would latch and
        // the spam author would be added.
        let xml = br#"<?xml version="1.0"?>
<rss version="2.0" xmlns:s="http://example.com/s">
  <channel>
    <title>Shop</title>
    <item>
      <s:variant>
        <title>Default Title</title>
        <author>bot@spam.example</author>
        <link>https://example.com/wrong</link>
      </s:variant>
      <title>Real Title</title>
      <link>https://example.com/right</link>
      <guid>g1</guid>
    </item>
  </channel>
</rss>"#;
        let feed = parse_rss(xml, "https://example.com/feed", false).unwrap();
        let item = &feed.items[0];
        assert_eq!(item.title.as_deref(), Some("Real Title"));
        assert!(item.authors.is_empty());
        assert_eq!(
            item.external_url.as_deref(),
            Some("https://example.com/right")
        );
    }

    #[test]
    fn rss_media_content_survives_namespace_skip() {
        // Our divergence from NNW: we consume MRSS, so media:* must NOT be
        // swallowed by the namespace skip even when a sibling <s:variant> is.
        let xml = br#"<?xml version="1.0"?>
<rss version="2.0"
     xmlns:media="http://search.yahoo.com/mrss/"
     xmlns:s="http://example.com/s">
  <channel>
    <title>Shop</title>
    <item>
      <title>Real Title</title>
      <guid>g1</guid>
      <media:content url="https://example.com/video.mp4" type="video/mp4"/>
      <media:group>
        <media:thumbnail url="https://example.com/thumb.jpg"/>
      </media:group>
      <s:variant><title>Default Title</title></s:variant>
    </item>
  </channel>
</rss>"#;
        let feed = parse_rss(xml, "https://example.com/feed", false).unwrap();
        let item = &feed.items[0];
        assert_eq!(item.title.as_deref(), Some("Real Title"));
        assert_eq!(item.attachments.len(), 2);
        let urls: Vec<&str> = item.attachments.iter().map(|a| a.url.as_str()).collect();
        assert!(urls.contains(&"https://example.com/video.mp4"));
        assert!(urls.contains(&"https://example.com/thumb.jpg"));
    }

    #[test]
    fn atom_namespaced_section_does_not_overwrite_entry_fields() {
        // NNW #4422, Atom side: a nested unprefixed <title> inside a namespaced
        // <s:variant> placed before the real title must not win.
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:s="http://example.com/s">
  <title>Site</title>
  <entry>
    <s:variant><title>Default Title</title></s:variant>
    <id>entry-1</id>
    <title>Real Entry Title</title>
    <summary>Real summary.</summary>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(feed.items.len(), 1);
        let item = &feed.items[0];
        assert_eq!(item.title.as_deref(), Some("Real Entry Title"));
        assert_eq!(item.summary.as_deref(), Some("Real summary."));
        assert!(
            !feed
                .items
                .iter()
                .any(|i| i.title.as_deref() == Some("Default Title"))
        );
    }

    #[test]
    fn atom_media_in_entry_does_not_corrupt_title() {
        // Atom consumes no prefixed elements, so a <media:title> inside a
        // <media:group> must not leak into the entry title (it did before).
        let xml = br#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:media="http://search.yahoo.com/mrss/">
  <title>Site</title>
  <entry>
    <media:group><media:title>Bogus Media Title</media:title></media:group>
    <id>entry-1</id>
    <title>Real Entry Title</title>
  </entry>
</feed>"#;
        let feed = parse_atom(xml, "https://example.com/feed").unwrap();
        assert_eq!(feed.items[0].title.as_deref(), Some("Real Entry Title"));
    }

    #[test]
    fn html_named_entities_decode_rather_than_dropping_the_text() {
        // Regression: unescape() returns Err on any entity outside the five
        // predefined XML ones, and the callers treat Err as "skip this text",
        // so before the quick-xml escape-html feature every one of these
        // titles parsed to None instead of merely keeping the raw entity.
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Chan</title>
  <item><guid>g1</guid><title>Tom &mdash; Jerry</title></item>
  <item><guid>g2</guid><title>Mary&rsquo;s &ldquo;cat&rdquo;</title></item>
  <item><guid>g3</guid><title>Tom &amp; Jerry &#8212; live</title></item>
</channel></rss>"#;
        let feed = parse_rss(xml.as_bytes(), "https://example.com/feed", false).unwrap();
        let titles: Vec<_> = feed
            .items
            .iter()
            .filter_map(|i| i.title.as_deref())
            .collect();
        assert_eq!(
            titles,
            vec!["Tom — Jerry", "Mary’s “cat”", "Tom & Jerry — live"]
        );
    }

    #[test]
    fn zero_width_entities_decode() {
        // NNW 3a948b755: &zwj;/&zwnj; carry emoji ZWJ sequences and Indic /
        // Arabic / Persian shaping, so an undecoded one is a rendering bug.
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Chan</title>
  <item><guid>g1</guid><title>a&zwj;b</title><description>c&zwnj;d</description></item>
</channel></rss>"#;
        let feed = parse_rss(xml.as_bytes(), "https://example.com/feed", false).unwrap();
        assert_eq!(feed.items[0].title.as_deref(), Some("a\u{200D}b"));
        assert_eq!(feed.items[0].content_html.as_deref(), Some("c\u{200C}d"));
    }

    // MARK: - Atom xml:base (NNW `1d54877be`, #5088)

    /// Pappacoda-shape: a feed-level xml:base resolves the feed icon, the
    /// entry's alternate link, and the URLs inside xhtml content (src and
    /// srcset), while the <id> stays verbatim and the xhtml wrapper div
    /// survives.
    #[test]
    fn xml_base_resolves_xhtml_content_and_links() {
        let xml = r##"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://andrea.pappacoda.it/blog/">
  <title>pappacoda</title><id>tag:andrea.pappacoda.it,2023</id>
  <icon>../assets/debian-button.gif</icon>
  <logo>/andrea_pappacoda.jpg</logo>
  <entry>
    <id>tag:andrea.pappacoda.it,2023-06-09:c_su_windows</id>
    <title>C su Windows</title>
    <updated>2023-06-09T00:00:00Z</updated>
    <link rel="alternate" href="c_su_windows/"/>
    <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml">
      <img src="c_su_windows/c_su_windows_visual_studio_componenti.png"
           srcset="c_su_windows/c_su_windows_visual_studio_componenti_light.png 1x"/>
    </div></content>
  </entry>
</feed>"##;
        let feed =
            parse_atom(xml.as_bytes(), "https://andrea.pappacoda.it/blog/feed.atom").unwrap();

        // Root-relative icon resolves against the feed-level base; the
        // logo (root-relative with a slash) resolves to the site root.
        assert_eq!(
            feed.icon_url.as_deref(),
            Some("https://andrea.pappacoda.it/assets/debian-button.gif")
        );

        let item = feed.items.first().expect("entry");
        assert_eq!(
            item.url.as_deref(),
            Some("https://andrea.pappacoda.it/blog/c_su_windows/")
        );
        // <id> is never resolved, even though it looks nothing like the base.
        assert_eq!(item.id, "tag:andrea.pappacoda.it,2023-06-09:c_su_windows");

        let html = item.content_html.as_deref().expect("xhtml content");
        assert!(html.contains(r#"src="https://andrea.pappacoda.it/blog/c_su_windows/c_su_windows_visual_studio_componenti.png""#));
        assert!(html.contains(r#"srcset="https://andrea.pappacoda.it/blog/c_su_windows/c_su_windows_visual_studio_componenti_light.png 1x""#));
        // The xhtml wrapper still comes through verbatim.
        assert!(html.contains(r#"xmlns="http://www.w3.org/1999/xhtml""#));
    }

    /// A feed-level base and a relative content-level base compose, a
    /// relative entry link resolves against xml:base rather than the
    /// homepage fallback, and an xml:base on the link element itself
    /// wins (upstream nestedRelativeXMLBase, 1:1).
    #[test]
    fn nested_relative_xml_base_composes() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://example.com/blog/">
  <title>t</title><id>urn:t</id>
  <link rel="alternate" href="https://homepage.example.org/"/>
  <entry>
    <id>urn:t:1</id>
    <title>nested base</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <link rel="alternate" href="post1/"/>
    <link rel="related" xml:base="https://elsewhere.example.net/dir/" href="related.html"/>
    <content type="html" xml:base="post1/">&lt;a href="pic.png"&gt;pic&lt;/a&gt;</content>
  </entry>
</feed>"#;
        let feed = parse_atom(xml.as_bytes(), "https://example.com/blog/feed.atom").unwrap();
        let item = feed.items.first().expect("entry");

        assert_eq!(item.url.as_deref(), Some("https://example.com/blog/post1/"));
        assert_eq!(
            item.external_url.as_deref(),
            Some("https://elsewhere.example.net/dir/related.html")
        );
        let html = item.content_html.as_deref().expect("content");
        assert!(html.contains(r#"href="https://example.com/blog/post1/pic.png""#));
    }

    /// RFC 4287 allows type="text/html" as well as type="html"; a
    /// relative xml:base with no enclosing base resolves against the
    /// document (feed) URL; a type="text" body is never rewritten; and a
    /// garbage (non-http) xml:base inherits instead of poisoning.
    #[test]
    fn content_type_gates_and_base_fallbacks() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>urn:t</id>
  <entry>
    <id>urn:t:1</id>
    <title>mime type html</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <content type="text/html" xml:base="https://example.com/post/">&lt;img src="pic.png"&gt;</content>
  </entry>
  <entry>
    <id>/2024/04/23/qemu-9-0-0</id>
    <title>release</title>
    <updated>2024-04-23T23:02:00Z</updated>
    <content type="html" xml:base="/2024/04/23/qemu-9-0-0/">&lt;a href="changelog.html"&gt;changelog&lt;/a&gt;</content>
  </entry>
  <entry>
    <id>urn:t:3</id>
    <title>text content</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <content type="text" xml:base="https://example.com/post/">See src="pic.png" for details.</content>
  </entry>
  <entry>
    <id>urn:t:4</id>
    <title>garbage base inherits</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <link rel="alternate" xml:base="urn:whatever" href="elsewhere/"/>
    <content type="html">&lt;img src="raw.png"&gt;</content>
  </entry>
</feed>"#;
        let feed = parse_atom(xml.as_bytes(), "https://www.qemu.org/feed.xml").unwrap();

        // type="text/html" resolves.
        let html = feed.items[0].content_html.as_deref().unwrap();
        assert!(html.contains(r#"src="https://example.com/post/pic.png""#));

        // A relative-looking <id> stays verbatim; the relative content-level
        // xml:base composed against the feed URL resolves the body.
        assert_eq!(feed.items[1].id, "/2024/04/23/qemu-9-0-0");
        let html = feed.items[1].content_html.as_deref().unwrap();
        assert!(
            html.contains(r#"href="https://www.qemu.org/2024/04/23/qemu-9-0-0/changelog.html""#)
        );

        // type="text" is never rewritten.
        let html = feed.items[2].content_html.as_deref().unwrap();
        assert!(html.contains(r#"src="pic.png""#));

        // The urn: xml:base is not usable, so the entry inherits: the link
        // falls back to the home-page-less path and stays relative, and the
        // body resolves against nothing.
        let item = &feed.items[3];
        assert_eq!(item.url.as_deref(), Some("elsewhere/"));
        let html = item.content_html.as_deref().unwrap();
        assert!(html.contains(r#"src="raw.png""#));
    }

    /// Fragment-only hrefs in bodies stay fragment-only (the footnote
    /// links depend on it), and an empty link href must not resolve to
    /// the base or home URL (upstream's OneFootTsunami assertions).
    #[test]
    fn fragment_and_empty_hrefs_survive_xml_base() {
        let xml = r##"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://example.com/blog/">
  <title>t</title><id>urn:t</id>
  <entry>
    <id>urn:t:1</id>
    <title>footnotes</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <link rel="alternate" href=""/>
    <content type="html">&lt;a href="#fn1"&gt;1&lt;/a&gt; &lt;img src="pic.png"&gt;</content>
  </entry>
</feed>"##;
        let feed = parse_atom(xml.as_bytes(), "https://example.com/blog/feed.atom").unwrap();
        let item = feed.items.first().expect("entry");

        // Empty href: the link is skipped entirely rather than resolving
        // to the base.
        assert_eq!(item.url, None);

        let html = item.content_html.as_deref().unwrap();
        assert!(
            html.contains(r##"href="#fn1""##),
            "fragment-only hrefs must stay fragment-only"
        );
        assert!(html.contains(r#"src="https://example.com/blog/pic.png""#));
    }

    /// A summary with its own xml:base resolves at parse time. Ours keeps
    /// the summary in its own field (the render fallback chain prefers
    /// content, then summary), so this asserts on `summary`.
    #[test]
    fn summary_with_xml_base_resolves() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>t</title><id>urn:t</id>
  <entry>
    <id>urn:t:1</id>
    <title>summary only</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <summary type="html" xml:base="https://example.com/post/">&lt;img src="pic.png"&gt;</summary>
  </entry>
</feed>"#;
        let feed = parse_atom(xml.as_bytes(), "https://example.com/feed.atom").unwrap();
        let item = feed.items.first().expect("entry");
        let html = item.summary.as_deref().expect("summary");
        assert!(html.contains(r#"src="https://example.com/post/pic.png""#));
    }

    /// A relative author <uri> resolves against the base in effect.
    #[test]
    fn relative_author_uri_resolves_against_xml_base() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://example.com/blog/">
  <title>t</title><id>urn:t</id>
  <entry>
    <id>urn:t:1</id>
    <title>relative author uri</title>
    <updated>2020-05-01T00:00:00Z</updated>
    <author><name>Alice</name><uri>about/</uri></author>
  </entry>
</feed>"#;
        let feed = parse_atom(xml.as_bytes(), "https://example.com/blog/feed.atom").unwrap();
        let item = feed.items.first().expect("entry");
        let author = item.authors.first().expect("author");
        assert_eq!(
            author.url.as_deref(),
            Some("https://example.com/blog/about/")
        );
    }
}
