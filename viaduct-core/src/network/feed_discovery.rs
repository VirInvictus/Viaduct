// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Resolve a user-supplied URL to a real feed URL + title.
//!
//! Port of NetNewsWire's `FeedFinder`. Two-pass:
//!   1. Treat the input URL as a feed and try to parse it directly. If
//!      the bytes parse as RSS / RDF / Atom / JSON Feed, we're done.
//!   2. Otherwise treat it as a website. Use `parser::html::extract_metadata`
//!      to scan the `<head>` for `<link rel="alternate" type="application/{rss,atom}+xml">`
//!      tags. Pick the first match, recurse into it as a feed.
//!
//! This is what NNW's "Add Feed" dialog uses to let users paste either
//! `https://daringfireball.net/feeds/main` (a real feed URL) or
//! `https://daringfireball.net` (the website's home page) — both end up
//! pointing at the same feed.

use crate::error::{NetworkError, Result, ViaductError};
use crate::models::ParsedFeed;
use crate::parser;
use reqwest::Client;
use url::Url;

/// Result of a successful discovery — the canonical feed URL and the
/// metadata we got from parsing it. The caller uses these to populate
/// the OPML entry and fire an immediate refresh.
#[derive(Debug, Clone)]
pub struct DiscoveredFeed {
    pub feed_url: String,
    pub title: Option<String>,
    pub home_page_url: Option<String>,
}

impl DiscoveredFeed {
    fn from_parsed(feed_url: String, parsed: ParsedFeed) -> Self {
        Self {
            feed_url,
            title: parsed.title,
            home_page_url: parsed.home_page_url,
        }
    }
}

/// Try to resolve `url` to a feed. First attempts to parse the URL's
/// content directly as a feed; if that fails, treats it as HTML and
/// looks for a `<link rel="alternate" type="application/rss+xml">` (or
/// the Atom equivalent) and recurses into the discovered feed URL.
///
/// Returns the canonical feed URL + title + home-page URL on success.
/// Returns `Err(NetworkError::NoFeedFound)` when neither path yields a
/// parseable feed.
pub async fn discover_feed(client: &Client, url: &str) -> Result<DiscoveredFeed> {
    // Avoid runaway recursion if a redirect chain or rel=alternate loop
    // sends us in circles. NNW caps similar loops at 2; matching that.
    discover_feed_with_depth(client, url, 0).await
}

#[allow(clippy::manual_async_fn)]
fn discover_feed_with_depth<'a>(
    client: &'a Client,
    url: &'a str,
    depth: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<DiscoveredFeed>> + 'a + Send>> {
    // Hand-rolled `Pin<Box<...>>` rather than `async fn` so the function
    // can be self-recursive without `async-recursion`. The recursion
    // depth is bounded by `MAX_DEPTH` so the stack stays modest.
    Box::pin(async move {
        const MAX_DEPTH: u8 = 2;
        if depth > MAX_DEPTH {
            return Err(ViaductError::Network(NetworkError::NoFeedFound));
        }

        let canonical = canonicalize_input(url)?;
        let bytes = fetch_bytes(client, &canonical).await?;

        // Pass 1: try parsing as a feed. parse() dispatches on the magic
        // bytes / opening element so RSS / RDF / Atom / JSON Feed all
        // work without us pre-classifying.
        if let Ok(parsed) = parser::parse(&bytes, &canonical) {
            return Ok(DiscoveredFeed::from_parsed(canonical, parsed));
        }

        // Pass 2: treat as HTML, scan for rel=alternate.
        let metadata = parser::extract_metadata(&bytes, &canonical);
        let Some(alternate_url) = first_feed_link(&metadata, &canonical) else {
            return Err(ViaductError::Network(NetworkError::NoFeedFound));
        };

        discover_feed_with_depth(client, &alternate_url, depth + 1).await
    })
}

/// Normalize the user-supplied URL — strip whitespace, default to
/// `https://` if no scheme. Bare hostnames like "daringfireball.net"
/// should resolve.
fn canonicalize_input(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ViaductError::Network(NetworkError::NoFeedFound));
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Ok(trimmed.to_string());
    }
    Ok(format!("https://{trimmed}"))
}

async fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ViaductError::Network(NetworkError::Reqwest(e)))?;
    if !resp.status().is_success() {
        return Err(ViaductError::Network(NetworkError::NoFeedFound));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ViaductError::Network(NetworkError::Reqwest(e)))?;
    Ok(bytes.to_vec())
}

/// Walk the metadata's `<link>` tags and return the first one whose
/// `rel` includes `alternate` and whose `type` is `application/rss+xml`,
/// `application/atom+xml`, or `application/feed+json`. Resolves the
/// link's `href` against the page URL so we get an absolute target.
fn first_feed_link(metadata: &parser::HtmlMetadata, base_url: &str) -> Option<String> {
    let base = Url::parse(base_url).ok()?;
    for tag in &metadata.tags {
        if tag.tag_type != parser::HtmlTagType::Link {
            continue;
        }
        let rel = tag.attributes.get("rel")?.to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "alternate") {
            continue;
        }
        let kind = tag.attributes.get("type")?.to_ascii_lowercase();
        let is_feed = kind == "application/rss+xml"
            || kind == "application/atom+xml"
            || kind == "application/feed+json";
        if !is_feed {
            continue;
        }
        let Some(href) = tag.attributes.get("href") else {
            continue;
        };
        return base.join(href).ok().map(|u| u.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn link_tag(rel: &str, kind: &str, href: &str) -> parser::HtmlTag {
        let mut attributes = HashMap::new();
        attributes.insert("rel".to_string(), rel.to_string());
        attributes.insert("type".to_string(), kind.to_string());
        attributes.insert("href".to_string(), href.to_string());
        parser::HtmlTag {
            tag_type: parser::HtmlTagType::Link,
            attributes,
        }
    }

    #[test]
    fn finds_first_rss_alternate() {
        let metadata = parser::HtmlMetadata {
            url_string: "https://example.com/".to_string(),
            tags: vec![
                link_tag("stylesheet", "text/css", "/style.css"),
                link_tag("alternate", "application/rss+xml", "/feed.xml"),
            ],
        };
        assert_eq!(
            first_feed_link(&metadata, "https://example.com/"),
            Some("https://example.com/feed.xml".to_string())
        );
    }

    #[test]
    fn finds_atom_alternate() {
        let metadata = parser::HtmlMetadata {
            url_string: "https://example.com/".to_string(),
            tags: vec![link_tag(
                "alternate",
                "application/atom+xml",
                "https://feeds.example.com/atom",
            )],
        };
        assert_eq!(
            first_feed_link(&metadata, "https://example.com/"),
            Some("https://feeds.example.com/atom".to_string())
        );
    }

    #[test]
    fn finds_json_feed_alternate() {
        let metadata = parser::HtmlMetadata {
            url_string: "https://example.com/".to_string(),
            tags: vec![link_tag("alternate", "application/feed+json", "/feed.json")],
        };
        assert_eq!(
            first_feed_link(&metadata, "https://example.com/"),
            Some("https://example.com/feed.json".to_string())
        );
    }

    #[test]
    fn ignores_non_feed_alternates() {
        let metadata = parser::HtmlMetadata {
            url_string: "https://example.com/".to_string(),
            tags: vec![
                link_tag("alternate", "text/html", "/mobile/"),
                link_tag("icon", "image/png", "/favicon.png"),
            ],
        };
        assert_eq!(first_feed_link(&metadata, "https://example.com/"), None);
    }

    #[test]
    fn resolves_relative_href_against_page() {
        let metadata = parser::HtmlMetadata {
            url_string: "https://example.com/blog/".to_string(),
            tags: vec![link_tag("alternate", "application/rss+xml", "feed.xml")],
        };
        assert_eq!(
            first_feed_link(&metadata, "https://example.com/blog/"),
            Some("https://example.com/blog/feed.xml".to_string())
        );
    }

    #[test]
    fn canonicalize_adds_https_when_missing() {
        assert_eq!(
            canonicalize_input("daringfireball.net").unwrap(),
            "https://daringfireball.net"
        );
        assert_eq!(
            canonicalize_input("https://daringfireball.net").unwrap(),
            "https://daringfireball.net"
        );
        assert_eq!(
            canonicalize_input("http://example.com").unwrap(),
            "http://example.com"
        );
    }

    #[test]
    fn canonicalize_trims_whitespace() {
        assert_eq!(
            canonicalize_input("  https://example.com  ").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn canonicalize_rejects_empty() {
        assert!(canonicalize_input("").is_err());
        assert!(canonicalize_input("   ").is_err());
    }
}
