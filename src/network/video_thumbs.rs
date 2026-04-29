// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Video thumbnail extraction.
//!
//! Detects YouTube / Vimeo URLs in article metadata or HTML body and resolves
//! them to a thumbnail image URL. The resolved URL flows through `ImageCache`
//! (`Kind::VideoThumb`) for the same memory + disk caching the favicon and
//! inline-image paths use.
//!
//! NetNewsWire's analog (`Modules/Articles/Sources/Articles/.../articleImage`)
//! stops at the article-image hint embedded in the feed itself; it does NOT
//! inspect feed HTML for video embeds. We extend the convention because so
//! many tech / video feeds ship items where the only useful preview lives at
//! a YouTube watch URL.
//!
//! Detection priority:
//!   1. `article.external_url` — the canonical "this article IS a video" case
//!      (YouTube channel feeds, Vimeo profile feeds).
//!   2. `article.url` — same but for clients that swap the two fields.
//!   3. `article.content_html` — first matching `<a href>` or bare URL.
//!
//! Vimeo lookups hit the public oEmbed endpoint (`/api/oembed.json`) since the
//! thumbnail URL is not deterministic from the video ID. YouTube thumbnails
//! resolve via the deterministic `i.ytimg.com` path.

use crate::models::Article;

/// Identified video source. Carries enough state to resolve a thumbnail URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSource {
    YouTube { id: String },
    Vimeo { id: String },
}

impl VideoSource {
    /// Try to identify a video source from a URL. Returns `None` for URLs that
    /// don't match a recognized provider pattern.
    pub fn from_url(url: &str) -> Option<Self> {
        if let Some(id) = parse_youtube_id(url) {
            return Some(VideoSource::YouTube { id });
        }
        if let Some(id) = parse_vimeo_id(url) {
            return Some(VideoSource::Vimeo { id });
        }
        None
    }
}

/// Detect a primary video source for an article. Walks `external_url`, `url`,
/// then scans `content_html` / `content_text` / `summary` for the first
/// recognizable provider URL. Returns `None` when nothing matches.
pub fn detect_video(article: &Article) -> Option<VideoSource> {
    if let Some(url) = article.external_url.as_deref()
        && let Some(src) = VideoSource::from_url(url)
    {
        return Some(src);
    }
    if let Some(url) = article.url.as_deref()
        && let Some(src) = VideoSource::from_url(url)
    {
        return Some(src);
    }
    for body in [
        article.content_html.as_deref(),
        article.content_text.as_deref(),
        article.summary.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(src) = scan_html_for_video(body) {
            return Some(src);
        }
    }
    None
}

/// Deterministic YouTube thumbnail URL. Uses `hqdefault.jpg` (480×360) which
/// always exists for any valid video ID — `maxresdefault` exists only when
/// the uploader provided a high-res custom thumbnail.
pub fn youtube_thumbnail_url(id: &str) -> String {
    format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
}

/// Resolve a thumbnail URL for a `VideoSource`. YouTube returns synchronously;
/// Vimeo hits the public oEmbed endpoint (no auth required).
pub async fn thumbnail_url(client: &reqwest::Client, source: &VideoSource) -> Option<String> {
    match source {
        VideoSource::YouTube { id } => Some(youtube_thumbnail_url(id)),
        VideoSource::Vimeo { id } => fetch_vimeo_thumbnail_url(client, id).await,
    }
}

async fn fetch_vimeo_thumbnail_url(client: &reqwest::Client, id: &str) -> Option<String> {
    let oembed = format!("https://vimeo.com/api/oembed.json?url=https://vimeo.com/{id}");
    let resp = client.get(&oembed).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("thumbnail_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn scan_html_for_video(html: &str) -> Option<VideoSource> {
    for token in extract_url_candidates(html) {
        if let Some(src) = VideoSource::from_url(token) {
            return Some(src);
        }
    }
    None
}

/// Pull every `http(s)://...` substring out of an HTML blob — we don't need a
/// full HTML parser for this, just the URL tokens. Stops at the first
/// whitespace, quote, or `<` after the scheme.
fn extract_url_candidates(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &html[i..];
        let scheme_off = rest.find("http").unwrap_or(usize::MAX);
        if scheme_off == usize::MAX {
            break;
        }
        let start = i + scheme_off;
        let after_scheme = &html[start..];
        if !(after_scheme.starts_with("http://") || after_scheme.starts_with("https://")) {
            i = start + 4;
            continue;
        }
        let end_rel = after_scheme
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
            .unwrap_or(after_scheme.len());
        out.push(&after_scheme[..end_rel]);
        i = start + end_rel;
        if end_rel == 0 {
            i += 1;
        }
    }
    out
}

fn parse_youtube_id(url: &str) -> Option<String> {
    // Matches: youtube.com/watch?v=ID, youtu.be/ID, youtube.com/embed/ID,
    // youtube-nocookie.com/embed/ID, youtube.com/shorts/ID, youtube.com/v/ID.
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    if host == "youtu.be" {
        let id = parsed.path().trim_start_matches('/');
        return sanitize_id(id);
    }
    if host == "youtube.com" || host == "youtube-nocookie.com" {
        let path = parsed.path();
        if path == "/watch" {
            let id = parsed.query_pairs().find(|(k, _)| k == "v")?.1.into_owned();
            return sanitize_id(&id);
        }
        for prefix in ["/embed/", "/shorts/", "/v/", "/live/"] {
            if let Some(rest) = path.strip_prefix(prefix) {
                let id = rest.split('/').next().unwrap_or("");
                return sanitize_id(id);
            }
        }
    }
    None
}

fn parse_vimeo_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host != "vimeo.com" && host != "player.vimeo.com" {
        return None;
    }
    let path = parsed.path().trim_start_matches('/');
    let first = path.split('/').next().unwrap_or("");
    let candidate = if first == "video" {
        path.split('/').nth(1).unwrap_or("")
    } else {
        first
    };
    if candidate.chars().all(|c| c.is_ascii_digit()) && !candidate.is_empty() {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn sanitize_id(raw: &str) -> Option<String> {
    let id: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if id.is_empty() { None } else { Some(id) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(id: &str) -> Option<VideoSource> {
        Some(VideoSource::YouTube { id: id.to_string() })
    }

    fn vm(id: &str) -> Option<VideoSource> {
        Some(VideoSource::Vimeo { id: id.to_string() })
    }

    #[test]
    fn detects_youtube_watch_url() {
        assert_eq!(
            VideoSource::from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            yt("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn detects_youtu_be_short_url() {
        assert_eq!(
            VideoSource::from_url("https://youtu.be/dQw4w9WgXcQ"),
            yt("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn detects_youtube_embed_url() {
        assert_eq!(
            VideoSource::from_url("https://www.youtube.com/embed/dQw4w9WgXcQ"),
            yt("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn detects_youtube_shorts_url() {
        assert_eq!(
            VideoSource::from_url("https://youtube.com/shorts/dQw4w9WgXcQ?feature=share"),
            yt("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn detects_youtube_nocookie_embed() {
        assert_eq!(
            VideoSource::from_url("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"),
            yt("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn detects_vimeo_video() {
        assert_eq!(
            VideoSource::from_url("https://vimeo.com/123456789"),
            vm("123456789")
        );
    }

    #[test]
    fn detects_vimeo_player() {
        assert_eq!(
            VideoSource::from_url("https://player.vimeo.com/video/123456789"),
            vm("123456789")
        );
    }

    #[test]
    fn rejects_non_video_urls() {
        assert_eq!(
            VideoSource::from_url("https://example.com/article/123"),
            None
        );
        assert_eq!(VideoSource::from_url("not a url"), None);
        assert_eq!(VideoSource::from_url("https://vimeo.com/categories"), None);
    }

    #[test]
    fn youtube_thumb_url_is_deterministic() {
        assert_eq!(
            youtube_thumbnail_url("dQw4w9WgXcQ"),
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
        );
    }

    #[test]
    fn extract_urls_from_html_finds_anchors() {
        let html =
            r#"<p>Check this <a href="https://www.youtube.com/watch?v=abc123XYZ_-">video</a>!</p>"#;
        let urls = extract_url_candidates(html);
        assert!(urls.iter().any(|u| u.contains("youtube.com/watch?v=")));
    }

    #[test]
    fn detect_video_finds_youtube_in_external_url() {
        let mut a = Article {
            article_id: "x".into(),
            feed_id: "f".into(),
            title: None,
            content_html: None,
            content_text: None,
            url: None,
            external_url: Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".into()),
            summary: None,
            image_url: None,
            date_published: None,
            date_modified: None,
            authors: vec![],
            attachments: vec![],
        };
        assert_eq!(detect_video(&a), yt("dQw4w9WgXcQ"));

        a.external_url = None;
        a.content_html =
            Some(r#"<p>Watch on <a href="https://youtu.be/abc123XYZ-_">YouTube</a></p>"#.into());
        assert_eq!(detect_video(&a), yt("abc123XYZ-_"));
    }

    #[test]
    fn detect_video_returns_none_when_no_match() {
        let a = Article {
            article_id: "x".into(),
            feed_id: "f".into(),
            title: Some("Plain post".into()),
            content_html: Some("<p>Just text, no videos.</p>".into()),
            content_text: None,
            url: Some("https://example.com/post/123".into()),
            external_url: Some("https://example.com/post/123".into()),
            summary: None,
            image_url: None,
            date_published: None,
            date_modified: None,
            authors: vec![],
            attachments: vec![],
        };
        assert_eq!(detect_video(&a), None);
    }
}
