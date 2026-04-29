// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::error::{ParseError, Result};
use crate::models::{Attachment, Author, ParsedFeed, ParsedItem};
use crate::parser::date::parse_date;
use md5::{Digest, Md5};
use serde_json::Value;

/// Parse JSON Feed `attachments[]` per the spec
/// (<https://jsonfeed.org/version/1.1#items>): `url` (required), `mime_type`,
/// `title`, `size_in_bytes`, `duration_in_seconds`. Skip entries without
/// a non-empty URL.
fn parse_jf_attachments(item: &Value) -> Vec<Attachment> {
    let Some(arr) = item.get("attachments").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|a| {
            let url = a.get("url").and_then(|v| v.as_str())?.to_string();
            if url.is_empty() {
                return None;
            }
            Some(Attachment {
                url,
                mime_type: a
                    .get("mime_type")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                title: a.get("title").and_then(|v| v.as_str()).map(String::from),
                size_in_bytes: a.get("size_in_bytes").and_then(|v| v.as_i64()),
                duration_in_seconds: a.get("duration_in_seconds").and_then(|v| v.as_i64()),
            })
        })
        .collect()
}

pub fn parse(data: &[u8], feed_url: &str) -> Result<ParsedFeed> {
    let root: Value = serde_json::from_slice(data).map_err(ParseError::Json)?;

    // Check if it's JSON Feed
    if let Some(version) = root.get("version").and_then(|v| v.as_str())
        && version.contains("://jsonfeed.org/version/")
    {
        return parse_json_feed(&root, feed_url);
    }

    // Check if it's RSS in JSON
    if let Some(rss) = root.get("rss").and_then(|v| v.as_object())
        && rss.get("channel").and_then(|v| v.as_object()).is_some()
    {
        return parse_rss_in_json(&root, feed_url);
    }

    Err(ParseError::UnknownFormat.into())
}

fn parse_json_feed(root: &Value, feed_url: &str) -> Result<ParsedFeed> {
    let title = root
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if title.is_none() {
        return Err(ParseError::Malformed("JSON Feed missing title".to_string()).into());
    }

    let home_page_url = root
        .get("home_page_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let parsed_feed_url = root
        .get("feed_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| Some(feed_url.to_string()));

    let mut parsed_items = Vec::new();

    if let Some(items) = root.get("items").and_then(|v| v.as_array()) {
        for item in items {
            let id = if let Some(id_str) = item.get("id").and_then(|v| v.as_str()) {
                id_str.to_string()
            } else if let Some(id_num) = item.get("id").and_then(|v| v.as_i64()) {
                id_num.to_string()
            } else if let Some(id_num) = item.get("id").and_then(|v| v.as_f64()) {
                id_num.to_string()
            } else {
                continue;
            };

            let content_html = item
                .get("content_html")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let content_text = item
                .get("content_text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if content_html.is_none() && content_text.is_none() {
                continue;
            }

            let url = item
                .get("url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let external_url = item
                .get("external_url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let item_title = item
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let summary = item
                .get("summary")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let image_url = item
                .get("image")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let date_published = item
                .get("date_published")
                .and_then(|v| v.as_str())
                .and_then(parse_date);
            let date_modified = item
                .get("date_modified")
                .and_then(|v| v.as_str())
                .and_then(parse_date);

            let mut authors = Vec::new();
            if let Some(authors_array) = item.get("authors").and_then(|v| v.as_array()) {
                for a in authors_array {
                    let name = a
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let url = a.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let avatar_url = a
                        .get("avatar")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if name.is_some() || url.is_some() || avatar_url.is_some() {
                        authors.push(Author {
                            name,
                            url,
                            avatar_url,
                            email: None,
                        });
                    }
                }
            } else if let Some(author) = item.get("author").and_then(|v| v.as_object()) {
                let name = author
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let url = author
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let avatar_url = author
                    .get("avatar")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if name.is_some() || url.is_some() || avatar_url.is_some() {
                    authors.push(Author {
                        name,
                        url,
                        avatar_url,
                        email: None,
                    });
                }
            }

            let attachments = parse_jf_attachments(item);
            parsed_items.push(ParsedItem {
                id,
                title: item_title,
                content_html,
                content_text,
                url,
                external_url,
                summary,
                image_url,
                date_published,
                date_modified,
                authors,
                attachments,
            });
        }
    } else {
        return Err(ParseError::Malformed("JSON Feed items not found".to_string()).into());
    }

    Ok(ParsedFeed {
        title,
        home_page_url,
        feed_url: parsed_feed_url,
        icon_url: None,
        language: None,
        items: parsed_items,
    })
}

fn parse_rss_in_json(root: &Value, feed_url: &str) -> Result<ParsedFeed> {
    let rss = root
        .get("rss")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ParseError::Malformed("RSS channel not found".to_string()))?;
    let channel = rss
        .get("channel")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ParseError::Malformed("RSS channel not found".to_string()))?;

    let title = channel
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let home_page_url = channel
        .get("link")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let items_array = channel
        .get("item")
        .or_else(|| root.get("item"))
        .or_else(|| channel.get("items"))
        .or_else(|| root.get("items"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| ParseError::Malformed("RSS items not found".to_string()))?;

    let mut parsed_items = Vec::new();

    for item in items_array {
        let external_url = item
            .get("link")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let item_title = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut content_html = item
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let mut content_text = None;

        if let Some(ref html) = content_html
            && !html.contains('<')
        {
            content_text = Some(html.clone());
            content_html = None;
        }

        if content_html.is_none() && content_text.is_none() && item_title.is_none() {
            continue;
        }

        let date_published = item
            .get("pubDate")
            .and_then(|v| v.as_str())
            .and_then(parse_date);

        let mut authors = Vec::new();
        if let Some(author_email) = item.get("author").and_then(|v| v.as_str()) {
            authors.push(Author {
                name: None,
                url: None,
                avatar_url: None,
                email: Some(author_email.to_string()),
            });
        }

        let mut unique_id = item
            .get("guid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if unique_id.is_none() {
            let mut s = String::new();
            if let Some(date) = date_published {
                s.push_str(&date.timestamp().to_string());
            }
            if let Some(ref t) = item_title {
                s.push_str(t);
            }
            if let Some(ref link) = external_url {
                s.push_str(link);
            }
            if let Some(author) = authors.first()
                && let Some(ref email) = author.email
            {
                s.push_str(email);
            }
            if s.is_empty() {
                if let Some(ref html) = content_html {
                    s.push_str(html);
                }
                if let Some(ref text) = content_text {
                    s.push_str(text);
                }
            }

            // MD5 to keep the synthetic ID stable across builds (matches NNW).
            let mut hasher = Md5::new();
            hasher.update(s.as_bytes());
            unique_id = Some(format!("{:x}", hasher.finalize()));
        }

        if let Some(id) = unique_id {
            parsed_items.push(ParsedItem {
                id,
                title: item_title,
                content_html,
                content_text,
                url: None,
                external_url,
                summary: None,
                image_url: None,
                date_published,
                date_modified: None,
                authors,
                attachments: Vec::new(),
            });
        }
    }

    Ok(ParsedFeed {
        title,
        home_page_url,
        feed_url: Some(feed_url.to_string()),
        icon_url: None,
        language: None,
        items: parsed_items,
    })
}
