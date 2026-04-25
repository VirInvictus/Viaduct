// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Local Reader View extractor.
//!
//! Port of NNW's `ArticleExtractor` (`.netnewswire/Shared/Article Extractor/`),
//! with one deliberate deviation: NNW calls a hosted Mercury endpoint
//! (`extract.feedbin.com/parser`). We run the Mozilla Readability port
//! locally via the `readability` crate so we don't depend on an external
//! service. The state machine matches NNW:
//! `Ready → Processing → (Complete | Failed | Cancelled)`.
//!
//! HTML sources, in order of preference:
//!   1. The article's own `content_html` when it looks long enough to be
//!      full-text (heuristic: >1 KB and contains a `<p`). Avoids a round-
//!      trip for feeds that already carry the complete article.
//!   2. An HTTP fetch of the article URL. Plain reqwest — we don't use the
//!      feed `Fetcher` because that layer has conditional-GET / coalescing
//!      semantics tuned for feed polling, not one-off page loads.
//!
//! Memory gate: the input HTML is capped at `INPUT_SIZE_CAP` before
//! extraction. Readability allocates multiple DOM representations; feeding
//! it a 20 MB tracker-blob page blows the 500 MB ceiling. If we need to
//! raise this cap, re-run `mem_check` first.

use reqwest::Client;
use std::io::Cursor;
use std::time::Duration;
use thiserror::Error;

/// Runtime cap on input HTML size. Tuned for the peak-RSS budget — anything
/// larger is usually an adtech-heavy page that wasn't going to extract well
/// anyway.
pub const INPUT_SIZE_CAP: usize = 5 * 1024 * 1024;

/// Rough threshold for "this feed body looks like it already has the full
/// article in it". Avoids a network round-trip when the feed publisher has
/// been generous with `<content:encoded>`.
const FULL_TEXT_MIN: usize = 1024;

/// Matches `ArticleExtractorState` in NNW. The UI button reflects this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderState {
    Ready,
    Processing,
    Complete,
    Failed,
    #[allow(dead_code)]
    Cancelled,
}

#[derive(Debug, Error)]
pub enum ReaderError {
    #[error("input HTML exceeds {INPUT_SIZE_CAP}-byte cap")]
    TooLarge,
    #[error("article URL is not a valid absolute URL")]
    InvalidUrl,
    #[error("network fetch failed: {0}")]
    Fetch(String),
    #[error("readability extraction failed: {0}")]
    Extraction(String),
    #[error("background task failed: {0}")]
    Join(String),
}

/// Run the extractor for `article_url`.
///
/// If `existing_html` already looks like full-text, run readability against
/// it directly. Otherwise fetch the URL and extract from that. Returns the
/// extracted body as HTML suitable for `article::render_html`.
pub async fn extract(
    article_url: &str,
    existing_html: Option<&str>,
) -> Result<String, ReaderError> {
    let parsed_url = url::Url::parse(article_url).map_err(|_| ReaderError::InvalidUrl)?;

    let html = match existing_html {
        Some(h) if h.len() >= FULL_TEXT_MIN && h.contains("<p") => h.to_string(),
        _ => fetch_article_html(article_url).await?,
    };

    if html.len() > INPUT_SIZE_CAP {
        return Err(ReaderError::TooLarge);
    }

    // `readability::extractor::extract` is CPU-bound (html5ever parse +
    // scoring tree walk). `spawn_blocking` keeps tokio worker threads free.
    let task = tokio::task::spawn_blocking(move || {
        let mut reader = Cursor::new(html.into_bytes());
        readability::extractor::extract(&mut reader, &parsed_url)
    });

    match task.await {
        Ok(Ok(product)) => Ok(product.content),
        Ok(Err(e)) => Err(ReaderError::Extraction(e.to_string())),
        Err(e) => Err(ReaderError::Join(e.to_string())),
    }
}

async fn fetch_article_html(url: &str) -> Result<String, ReaderError> {
    let client = Client::builder()
        .user_agent("Viaduct/1.0 (Reader View)")
        .use_rustls_tls()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| ReaderError::Fetch(e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ReaderError::Fetch(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(ReaderError::Fetch(format!("HTTP {}", resp.status())));
    }
    resp.text()
        .await
        .map_err(|e| ReaderError::Fetch(e.to_string()))
}
