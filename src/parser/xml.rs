use crate::error::{ParseError, Result};
use crate::models::{Author, ParsedFeed, ParsedItem};
use crate::parser::date::parse_date_bytes;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::str;

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

    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn parse_rss(data: &[u8], feed_url: &str, is_rdf: bool) -> Result<ParsedFeed> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut title = None;
    let mut home_page_url = None;
    let mut items = Vec::new();

    let mut in_item = false;

    let mut current_item_guid = None;
    let mut current_item_title = None;
    let mut current_item_body = None;
    let mut current_item_link = None;
    let mut current_item_permalink = None;
    let mut current_item_date = None;
    let mut current_item_authors = Vec::new();

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
                                current_item_link = Some(text_str);
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
                                // simplistic check for url
                                if current_item_permalink.is_none() && text_str.starts_with("http")
                                {
                                    current_item_permalink = Some(text_str);
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
                } else {
                    if let Ok(text) = e.unescape() {
                        let text_str = text.to_string();
                        match current_tag.as_slice() {
                            b"title" if title.is_none() => {
                                title = Some(text_str);
                            }
                            b"link" if home_page_url.is_none() => {
                                home_page_url = Some(text_str);
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.local_name();
                let name_ref = name.as_ref();
                current_tag.clear();

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
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => (),
        }
        buf.clear();
    }

    Ok(ParsedFeed {
        title,
        home_page_url,
        feed_url: Some(feed_url.to_string()),
        items,
    })
}

fn parse_atom(data: &[u8], feed_url: &str) -> Result<ParsedFeed> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut title = None;
    let mut home_page_url = None;
    let mut items = Vec::new();

    let mut in_item = false;

    let mut current_item_guid = None;
    let mut current_item_title = None;
    let mut current_item_body = None;
    let mut current_item_link = None;
    let mut current_item_permalink = None;
    let mut current_item_date = None;
    let mut current_item_date_modified = None;
    let mut current_item_authors = Vec::new();

    let mut current_tag = Vec::new();
    let mut _is_xhtml = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.local_name();
                let name_ref = name.as_ref();
                current_tag = name_ref.to_vec();

                if name_ref == b"entry" {
                    in_item = true;
                    current_item_guid = None;
                    current_item_title = None;
                    current_item_body = None;
                    current_item_link = None;
                    current_item_permalink = None;
                    current_item_date = None;
                    current_item_date_modified = None;
                    current_item_authors.clear();
                } else if in_item && (name_ref == b"content" || name_ref == b"summary") {
                    _is_xhtml = false;
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        if attr.key.as_ref() == b"type" && attr.value.as_ref() == b"xhtml" {
                            _is_xhtml = true;
                        }
                    }
                } else if name_ref == b"link" {
                    let mut href = None;
                    let mut rel = None;
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        if attr.key.as_ref() == b"href" {
                            href = str::from_utf8(attr.value.as_ref())
                                .ok()
                                .map(|s| s.to_string());
                        } else if attr.key.as_ref() == b"rel" {
                            rel = str::from_utf8(attr.value.as_ref())
                                .ok()
                                .map(|s| s.to_string());
                        }
                    }
                    if let Some(h) = href {
                        let rel_str = rel.as_deref().unwrap_or("alternate");
                        if in_item {
                            if rel_str == "alternate" && current_item_permalink.is_none() {
                                current_item_permalink = Some(h.clone());
                            } else if rel_str == "related" && current_item_link.is_none() {
                                current_item_link = Some(h.clone());
                            }
                        } else {
                            if rel_str == "alternate" && home_page_url.is_none() {
                                home_page_url = Some(h.clone());
                            }
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = e.local_name();
                if name.as_ref() == b"link" {
                    let mut href = None;
                    let mut rel = None;
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        if attr.key.as_ref() == b"href" {
                            href = str::from_utf8(attr.value.as_ref())
                                .ok()
                                .map(|s| s.to_string());
                        } else if attr.key.as_ref() == b"rel" {
                            rel = str::from_utf8(attr.value.as_ref())
                                .ok()
                                .map(|s| s.to_string());
                        }
                    }
                    if let Some(h) = href {
                        let rel_str = rel.as_deref().unwrap_or("alternate");
                        if in_item {
                            if rel_str == "alternate" && current_item_permalink.is_none() {
                                current_item_permalink = Some(h.clone());
                            } else if rel_str == "related" && current_item_link.is_none() {
                                current_item_link = Some(h.clone());
                            }
                        } else {
                            if rel_str == "alternate" && home_page_url.is_none() {
                                home_page_url = Some(h.clone());
                            }
                        }
                    }
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
                            b"name" => {
                                // Inside author
                                let author = Author {
                                    name: Some(text_str),
                                    url: None,
                                    avatar_url: None,
                                    email: None,
                                };
                                current_item_authors.push(author);
                            }
                            _ => {}
                        }
                    }
                } else {
                    if let Ok(text) = e.unescape() {
                        let text_str = text.to_string();
                        if current_tag.as_slice() == b"title" && title.is_none() {
                            title = Some(text_str);
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.local_name();
                let name_ref = name.as_ref();
                current_tag.clear();

                if name_ref == b"entry" {
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
                        date_modified: current_item_date_modified.take(),
                        authors: std::mem::take(&mut current_item_authors),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => (),
        }
        buf.clear();
    }

    Ok(ParsedFeed {
        title,
        home_page_url,
        feed_url: Some(feed_url.to_string()),
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
