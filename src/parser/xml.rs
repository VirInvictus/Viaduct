// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::error::{ParseError, Result};
use crate::models::{Attachment, Author, ParsedFeed, ParsedItem};
use crate::parser::date::parse_date_bytes;
use md5::{Digest, Md5};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::str;

/// Build an `Attachment` from an RSS `<enclosure>` element's attributes.
/// NNW's `RSSDelegate.handleEnclosure`: requires non-empty `url`, parses
/// optional `length` and `type`. Returns `None` when there's no usable URL.
fn enclosure_from_attrs(e: &quick_xml::events::BytesStart) -> Option<Attachment> {
    let mut url: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut size: Option<i64> = None;
    for attr in e.attributes().filter_map(|a| a.ok()) {
        match attr.key.as_ref() {
            b"url" => {
                url = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
            }
            b"type" => {
                mime_type = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
            }
            b"length" => {
                size = str::from_utf8(attr.value.as_ref())
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .filter(|n| *n > 0);
            }
            _ => {}
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
        duration_in_seconds: None,
    })
}

/// MRSS `<media:content>` / `<media:thumbnail>`. Reads `url`, optional
/// `type`, `fileSize`, `duration`. quick-xml strips the `media:` prefix on
/// `local_name`, so callers must already have confirmed the element name.
fn media_attachment_from_attrs(e: &quick_xml::events::BytesStart) -> Option<Attachment> {
    let mut url: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut size: Option<i64> = None;
    let mut duration: Option<i64> = None;
    for attr in e.attributes().filter_map(|a| a.ok()) {
        match attr.key.as_ref() {
            b"url" => {
                url = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
            }
            b"type" => {
                mime_type = str::from_utf8(attr.value.as_ref()).ok().map(String::from);
            }
            b"fileSize" => {
                size = str::from_utf8(attr.value.as_ref())
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .filter(|n| *n > 0);
            }
            b"duration" => {
                duration = str::from_utf8(attr.value.as_ref())
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .filter(|n| *n > 0);
            }
            _ => {}
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

    let mut current_tag = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.local_name();
                let name_ref = name.as_ref();
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
                    if let Some(att) = enclosure_from_attrs(e) {
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
                        && let Some(att) = media_attachment_from_attrs(e)
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
                let name = e.local_name();
                let name_ref = name.as_ref();
                if in_item && name_ref == b"enclosure" {
                    if let Some(att) = enclosure_from_attrs(e) {
                        current_item_attachments.push(att);
                    }
                } else if in_item
                    && (name_ref == b"content" || name_ref == b"thumbnail")
                    && e.attributes()
                        .filter_map(|a| a.ok())
                        .any(|a| a.key.as_ref() == b"url")
                    && let Some(att) = media_attachment_from_attrs(e)
                {
                    current_item_attachments.push(att);
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_item {
                    if let Ok(text) = e.unescape() {
                        let text_str = text.to_string();
                        match current_tag.as_slice() {
                            b"title" if current_item_title.is_none() => {
                                current_item_title = Some(text_str);
                            }
                            b"link" if current_item_link.is_none() => {
                                current_item_link =
                                    Some(resolve_url(&text_str, home_page_url.as_deref()));
                            }
                            b"description" if current_item_body.is_none() => {
                                current_item_body = Some(text_str);
                            }
                            b"encoded" => {
                                // content:encoded
                                current_item_body = Some(text_str);
                            }
                            b"guid" => {
                                current_item_guid = Some(text_str.clone());
                                if current_guid_is_permalink
                                    && current_item_permalink.is_none()
                                    && guid_looks_like_url(&text_str)
                                {
                                    current_item_permalink =
                                        Some(resolve_url(&text_str, home_page_url.as_deref()));
                                }
                            }
                            b"pubDate" | b"date" if current_item_date.is_none() => {
                                current_item_date = parse_date_bytes(text.as_bytes());
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
                    }
                } else if in_channel_image {
                    if let Ok(text) = e.unescape()
                        && current_tag.as_slice() == b"url"
                        && icon_url.is_none()
                    {
                        icon_url = Some(text.to_string());
                    }
                } else if let Ok(text) = e.unescape() {
                    let text_str = text.to_string();
                    match current_tag.as_slice() {
                        b"title" if title.is_none() => {
                            title = Some(text_str);
                        }
                        b"link" if home_page_url.is_none() => {
                            home_page_url = Some(text_str);
                        }
                        b"language" if language.is_none() => {
                            language = Some(text_str);
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
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

    let resolved_icon = icon_url.map(|u| resolve_url(&u, home_page_url.as_deref().or(Some(feed_url))));
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
    let Some(h) = href else { return };

    if ctx.in_author {
        // <link> inside <author> is non-standard, but if encountered, treat
        // href as the author's URL (matches NNW's generous parsing).
        if let Some(author) = ctx.current_author.as_mut()
            && author.url.is_none()
        {
            author.url = Some(resolve_url(&h, ctx.home_page_url.as_deref()));
        }
        return;
    }

    let resolved = resolve_url(&h, ctx.home_page_url.as_deref());
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
    let mut current_item_link = None;
    let mut current_item_permalink = None;
    let mut current_item_date = None;
    let mut current_item_date_modified = None;
    let mut current_item_authors: Vec<Author> = Vec::new();
    let mut current_item_attachments: Vec<Attachment> = Vec::new();
    let mut current_author: Option<MutableAuthor> = None;

    let mut current_tag: Vec<u8> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.local_name();
                let name_ref = name.as_ref();
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
                    current_item_guid = None;
                    current_item_title = None;
                    current_item_body = None;
                    current_item_link = None;
                    current_item_permalink = None;
                    current_item_date = None;
                    current_item_date_modified = None;
                    current_item_authors.clear();
                    current_item_attachments.clear();
                } else if name_ref == b"source" && in_item {
                    in_source = true;
                } else if name_ref == b"author" {
                    in_author = true;
                    current_author = Some(MutableAuthor::default());
                } else if name_ref == b"link" {
                    let mut ctx = AtomLinkCtx {
                        in_item,
                        in_author,
                        in_source,
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
                let name = e.local_name();
                if name.as_ref() == b"link" {
                    let mut ctx = AtomLinkCtx {
                        in_item,
                        in_author,
                        in_source,
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
                let Ok(text) = e.unescape() else { continue };
                let text_str = text.to_string();

                // Author body: collect into the open MutableAuthor.
                if in_author && let Some(author) = current_author.as_mut() {
                    match current_tag.as_slice() {
                        b"name" => author.name = Some(text_str),
                        b"email" => author.email = Some(text_str),
                        b"uri" => author.url = Some(text_str),
                        _ => {}
                    }
                    continue;
                }

                // <source> wraps a republished entry's original feed metadata —
                // ignore everything inside it so it doesn't pollute our entry.
                if in_source {
                    continue;
                }

                if in_item {
                    match current_tag.as_slice() {
                        b"title" if current_item_title.is_none() => {
                            current_item_title = Some(text_str);
                        }
                        b"content" if current_item_body.is_none() => {
                            current_item_body = Some(text_str);
                        }
                        b"summary" if current_item_body.is_none() => {
                            current_item_body = Some(text_str);
                        }
                        b"id" if current_item_guid.is_none() => {
                            current_item_guid = Some(text_str);
                        }
                        b"published" | b"issued" if current_item_date.is_none() => {
                            current_item_date = parse_date_bytes(text.as_bytes());
                        }
                        b"updated" | b"modified" if current_item_date_modified.is_none() => {
                            current_item_date_modified = parse_date_bytes(text.as_bytes());
                        }
                        _ => {}
                    }
                } else {
                    match current_tag.as_slice() {
                        b"title" if title.is_none() => title = Some(text_str),
                        b"icon" if icon_url.is_none() => icon_url = Some(text_str),
                        b"logo" if logo_url.is_none() => logo_url = Some(text_str),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.local_name();
                let name_ref = name.as_ref();
                current_tag.clear();

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
                        summary: None,
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

    let resolved_icon = icon_url.or(logo_url).map(|u| resolve_url(&u, home_page_url.as_deref().or(Some(feed_url))));
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
}
