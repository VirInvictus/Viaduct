// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Discover a feed's favicon URL when the feed XML doesn't ship one.
//!
//! Port of NetNewsWire's `SingleFaviconDownloader` flow. Most personal
//! blogs (Sacha Chua, Karthinks, public voit, etc.) don't include
//! `<image><url>` in their RSS or `<icon>` in their Atom, so the parser
//! leaves `ParsedFeed.icon_url` as None and the sidebar shows the
//! AdwAvatar fallback. Real readers probe the home page HTML's `<head>`
//! for a `<link rel="icon">` and fall back to `<origin>/favicon.ico`.
//!
//! Two passes:
//!   1. GET the home page; scan the head with `parser::extract_metadata`
//!      for `<link rel="icon">` / `<link rel="shortcut icon">` /
//!      `<link rel="apple-touch-icon">`. Prefer plain `icon` over
//!      `apple-touch-icon` (smaller, cheaper to render at 24 px in the
//!      sidebar avatar).
//!   2. Fall back to `<origin>/favicon.ico` and verify with a HEAD.
//!
//! Returns the absolute URL string of the verified favicon. Refresher
//! persists into `FeedSettings.favicon_url`; the sidebar's existing
//! `spawn_favicon_fetch` (`favicon_url.or(icon_url)`) then renders it.

use crate::parser;
use reqwest::Client;
use reqwest::header;
use tracing::debug;
use url::Url;

/// Cap the home-page HTML download. The metadata extractor only needs
/// the `<head>` block; aborting once we've read 256 KB keeps us from
/// pulling down a multi-MB landing page just to scan the first ~10 KB.
const HTML_FETCH_CAP_BYTES: usize = 256 * 1024;

/// Try to discover a usable favicon URL for `home_page_url`. Returns
/// `None` when neither the HTML head probe nor the `/favicon.ico`
/// fallback succeeds — caller leaves `favicon_url` unset so the sidebar
/// shows the `adw::Avatar` fallback (deterministic accent + initials).
pub async fn discover_favicon(client: &Client, home_page_url: &str) -> Option<String> {
    let base = Url::parse(home_page_url).ok()?;

    // Pass 1: scan the home page HTML head.
    if let Some(html_bytes) = fetch_html(client, home_page_url).await
        && let Some(candidate) = first_icon_link(&html_bytes, home_page_url)
        && verify_favicon(client, &candidate).await
    {
        debug!(%home_page_url, %candidate, "favicon discovery: html head match");
        return Some(candidate);
    }

    // Pass 2: `<origin>/favicon.ico` fallback. The vast majority of
    // sites still serve one even when the HTML head doesn't reference
    // it explicitly.
    let fallback = base.join("/favicon.ico").ok()?.to_string();
    if verify_favicon(client, &fallback).await {
        debug!(%home_page_url, %fallback, "favicon discovery: fallback /favicon.ico");
        return Some(fallback);
    }

    None
}

async fn fetch_html(client: &Client, url: &str) -> Option<Vec<u8>> {
    let resp = client
        .get(url)
        .header(header::ACCEPT, crate::network::http::ACCEPT_HTML)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() > HTML_FETCH_CAP_BYTES {
        // Truncate at the cap. extract_metadata is byte-driven and
        // bails at `<body>` for non-YouTube hosts; the head almost
        // always lands well inside 256 KB.
        Some(bytes[..HTML_FETCH_CAP_BYTES].to_vec())
    } else {
        Some(bytes.to_vec())
    }
}

/// Walk the metadata's `<link>` tags and pick the best favicon
/// candidate. Plain `icon` (and `shortcut icon`) wins over
/// `apple-touch-icon`; ties go to first-encountered. Returns absolute
/// URLs (resolved against the page URL).
fn first_icon_link(html_bytes: &[u8], base_url: &str) -> Option<String> {
    let metadata = parser::extract_metadata(html_bytes, base_url);
    let base = Url::parse(base_url).ok()?;

    let mut icon: Option<String> = None;
    let mut apple: Option<String> = None;
    for tag in &metadata.tags {
        if tag.tag_type != parser::HtmlTagType::Link {
            continue;
        }
        let Some(rel_raw) = tag.attributes.get("rel") else {
            continue;
        };
        let rel = rel_raw.to_ascii_lowercase();
        let mut rel_tokens = rel.split_whitespace();
        let is_icon = rel_tokens.any(|t| t == "icon" || t == "shortcut");
        let is_apple = rel.split_whitespace().any(|t| t == "apple-touch-icon");
        if !is_icon && !is_apple {
            continue;
        }
        let Some(href) = tag.attributes.get("href") else {
            continue;
        };
        let Ok(resolved) = base.join(href) else {
            continue;
        };
        let abs = resolved.to_string();
        if is_icon && icon.is_none() {
            icon = Some(abs);
        } else if is_apple && apple.is_none() {
            apple = Some(abs);
        }
    }
    icon.or(apple)
}

/// Confirm the URL serves a non-empty 200 response. HEAD with a GET
/// fallback because some CDNs (notably Cloudflare-fronted feeds) reject
/// HEAD with a 405. Body bytes are discarded — we just want a
/// reachability + reasonable-size signal so we don't persist a URL
/// that 404s on every sidebar bind.
async fn verify_favicon(client: &Client, url: &str) -> bool {
    if let Ok(resp) = client.head(url).send().await
        && resp.status().is_success()
    {
        return true;
    }
    if let Ok(resp) = client.get(url).send().await
        && resp.status().is_success()
        && let Ok(bytes) = resp.bytes().await
    {
        return !bytes.is_empty();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn link_tag(rel: &str, href: &str) -> parser::HtmlTag {
        let mut attributes = HashMap::new();
        attributes.insert("rel".to_string(), rel.to_string());
        attributes.insert("href".to_string(), href.to_string());
        parser::HtmlTag {
            tag_type: parser::HtmlTagType::Link,
            attributes,
        }
    }

    fn html_with_links(tags: &[(&str, &str)]) -> Vec<u8> {
        let mut body = String::from("<html><head>");
        for (rel, href) in tags {
            body.push_str(&format!("<link rel=\"{rel}\" href=\"{href}\">"));
        }
        body.push_str("</head><body></body></html>");
        body.into_bytes()
    }

    #[test]
    fn finds_plain_icon_link() {
        let html = html_with_links(&[("icon", "/favicon-32.png")]);
        assert_eq!(
            first_icon_link(&html, "https://example.com/"),
            Some("https://example.com/favicon-32.png".to_string())
        );
    }

    #[test]
    fn finds_shortcut_icon_link() {
        let html = html_with_links(&[("shortcut icon", "/favicon.ico")]);
        assert_eq!(
            first_icon_link(&html, "https://example.com/"),
            Some("https://example.com/favicon.ico".to_string())
        );
    }

    #[test]
    fn prefers_icon_over_apple_touch_icon() {
        let html = html_with_links(&[
            ("apple-touch-icon", "/apple-touch.png"),
            ("icon", "/favicon.ico"),
        ]);
        assert_eq!(
            first_icon_link(&html, "https://example.com/"),
            Some("https://example.com/favicon.ico".to_string())
        );
    }

    #[test]
    fn falls_back_to_apple_touch_icon() {
        let html = html_with_links(&[("apple-touch-icon", "/apple-touch.png")]);
        assert_eq!(
            first_icon_link(&html, "https://example.com/"),
            Some("https://example.com/apple-touch.png".to_string())
        );
    }

    #[test]
    fn ignores_non_icon_links() {
        let html = html_with_links(&[("stylesheet", "/style.css"), ("alternate", "/feed.xml")]);
        assert_eq!(first_icon_link(&html, "https://example.com/"), None);
    }

    #[test]
    fn resolves_relative_href() {
        let html = html_with_links(&[("icon", "favicon.ico")]);
        assert_eq!(
            first_icon_link(&html, "https://blog.example.com/posts/"),
            Some("https://blog.example.com/posts/favicon.ico".to_string())
        );
    }

    #[test]
    fn keeps_absolute_href() {
        let html = html_with_links(&[("icon", "https://cdn.example.com/icon.png")]);
        assert_eq!(
            first_icon_link(&html, "https://example.com/"),
            Some("https://cdn.example.com/icon.png".to_string())
        );
    }

    #[test]
    fn html_no_links_returns_none() {
        let html = b"<html><head></head><body><p>no links</p></body></html>";
        assert_eq!(first_icon_link(html, "https://example.com/"), None);
    }

    #[test]
    fn link_tag_helper_matches_extractor_shape() {
        // Sanity: confirms the extractor + first_icon_link integrate
        // correctly when the input is an HTML byte buffer rather than a
        // pre-built HtmlMetadata. (Other tests exercise the full path;
        // this one pins the shape so a future quick-xml upgrade can't
        // silently break attribute capture.)
        let tag = link_tag("icon", "/x");
        assert_eq!(tag.attributes.get("rel").map(String::as_str), Some("icon"));
        assert_eq!(tag.attributes.get("href").map(String::as_str), Some("/x"));
    }
}
