// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Async favicon + image cache.
//!
//! Two storage layers:
//!   * **Disk** — `$XDG_CACHE_HOME/viaduct/{favicons,images}/<md5>`. Survives restarts.
//!   * **Memory** — fixed-size LRU keyed by URL, holds the raw bytes.
//!
//! Linux has no reliable low-memory broadcast (NNW relies on iOS / macOS pressure
//! signals), so we cap the in-memory cache at a hard 250 entries to guarantee the
//! 500 MB peak-RSS budget. Decode-to-`gdk::Texture` happens on the GTK main thread
//! at the call site — we deliberately store `Vec<u8>` here so the LRU is `Send` and
//! lives on the tokio side.
//!
//! Missing favicons should fall back to `adw::Avatar` with the feed's display name;
//! `color_for(feed_name)` provides a deterministic accent color (port of NNW's
//! `ColorHash`) for callers that want to colorize their fallback widget.

use lru::LruCache;
use md5::{Digest, Md5};
use reqwest::Client;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Hard cap on each in-memory LRU. Keeps peak RSS bounded.
const MEMORY_CAPACITY: usize = 250;

#[derive(Clone)]
pub struct ImageCache {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    favicon_dir: PathBuf,
    image_dir: PathBuf,
    video_thumb_dir: PathBuf,
    favicons: LruCache<String, Vec<u8>>,
    images: LruCache<String, Vec<u8>>,
    video_thumbs: LruCache<String, Vec<u8>>,
    client: Client,
}

impl ImageCache {
    pub fn new(favicon_dir: PathBuf, image_dir: PathBuf, video_thumb_dir: PathBuf) -> Self {
        let cap = NonZeroUsize::new(MEMORY_CAPACITY).expect("MEMORY_CAPACITY > 0");
        let client = crate::network::http::build_default_client()
            .expect("Failed to build reqwest client for ImageCache");
        Self {
            inner: Arc::new(Mutex::new(Inner {
                favicon_dir,
                image_dir,
                video_thumb_dir,
                favicons: LruCache::new(cap),
                images: LruCache::new(cap),
                video_thumbs: LruCache::new(cap),
                client,
            })),
        }
    }

    /// Borrow the underlying reqwest `Client` for callers that need the same
    /// connection pool (e.g. video oEmbed lookups before the actual thumbnail
    /// fetch).
    pub async fn client(&self) -> Client {
        self.inner.lock().await.client.clone()
    }

    /// Drop every in-memory LRU entry across all three kinds. Disk cache is
    /// untouched — re-loads after this point go disk → texture instead of
    /// memory → texture, but skip the network round-trip. Used by the
    /// Phase 17 run-in-background mode: when the window hides we shed the
    /// cached image bytes so the resident set drops below the budget.
    /// Fire-and-forget — runs the actual clear on the global runtime.
    pub fn clear_memory(&self) {
        let cache = self.clone();
        crate::spawn_on_runtime(async move {
            cache.clear_memory_now().await;
        });
    }

    /// Same as `clear_memory` but await-able, so callers (notably the
    /// `mem_check` harness) can observe RSS after the clear completes
    /// deterministically rather than racing a `sleep`.
    pub async fn clear_memory_now(&self) {
        let mut inner = self.inner.lock().await;
        inner.favicons.clear();
        inner.images.clear();
        inner.video_thumbs.clear();
    }

    /// Fetch a favicon by URL. Memory hit → disk hit → network fetch → cache.
    /// Returns `None` on any failure; callers should fall back to `adw::Avatar`.
    pub async fn favicon(&self, url: &str) -> Option<Vec<u8>> {
        let cache = self.clone();
        let url = url.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = tx.send(cache.fetch_kind(&url, Kind::Favicon).await);
        });
        rx.await.unwrap_or(None)
    }

    /// Fetch an inline article image by URL.
    pub async fn image(&self, url: &str) -> Option<Vec<u8>> {
        let cache = self.clone();
        let url = url.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = tx.send(cache.fetch_kind(&url, Kind::Image).await);
        });
        rx.await.unwrap_or(None)
    }

    /// Fetch a video thumbnail by URL. Stored under the dedicated video-thumb
    /// cache directory so it doesn't blend with article inline images.
    pub async fn video_thumbnail(&self, url: &str) -> Option<Vec<u8>> {
        let cache = self.clone();
        let url = url.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = tx.send(cache.fetch_kind(&url, Kind::VideoThumb).await);
        });
        rx.await.unwrap_or(None)
    }

    async fn fetch_kind(&self, url: &str, kind: Kind) -> Option<Vec<u8>> {
        // 1. In-memory.
        {
            let mut inner = self.inner.lock().await;
            if let Some(bytes) = inner.lru_mut(kind).get(url) {
                debug!(%url, kind = ?kind, bytes = bytes.len(), "image cache: memory hit");
                return Some(bytes.clone());
            }
        }

        // 2. Disk.
        let (disk_path, client) = {
            let inner = self.inner.lock().await;
            (inner.disk_path(kind, url), inner.client.clone())
        };
        if let Ok(bytes) = tokio::fs::read(&disk_path).await {
            debug!(%url, kind = ?kind, bytes = bytes.len(), "image cache: disk hit");
            let mut inner = self.inner.lock().await;
            inner.lru_mut(kind).put(url.to_string(), bytes.clone());
            return Some(bytes);
        }

        // 3. Network.
        debug!(%url, kind = ?kind, "image cache: miss → network");
        let bytes = match download(&client, url).await {
            Some(b) => b,
            None => return None,
        };

        if let Some(parent) = disk_path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            warn!(?e, "failed to create cache dir");
        }
        if let Err(e) = tokio::fs::write(&disk_path, &bytes).await {
            warn!(?e, "failed to persist cache file");
        } else {
            debug!(%url, kind = ?kind, bytes = bytes.len(), path = ?disk_path, "image cache: disk write");
        }

        let mut inner = self.inner.lock().await;
        inner.lru_mut(kind).put(url.to_string(), bytes.clone());
        Some(bytes)
    }
}

#[derive(Copy, Clone, Debug)]
enum Kind {
    Favicon,
    Image,
    VideoThumb,
}

impl Inner {
    fn lru_mut(&mut self, kind: Kind) -> &mut LruCache<String, Vec<u8>> {
        match kind {
            Kind::Favicon => &mut self.favicons,
            Kind::Image => &mut self.images,
            Kind::VideoThumb => &mut self.video_thumbs,
        }
    }

    fn disk_path(&self, kind: Kind, url: &str) -> PathBuf {
        let dir: &Path = match kind {
            Kind::Favicon => &self.favicon_dir,
            Kind::Image => &self.image_dir,
            Kind::VideoThumb => &self.video_thumb_dir,
        };
        dir.join(cache_filename(url))
    }
}

async fn download(client: &Client, url: &str) -> Option<Vec<u8>> {
    use reqwest::header;
    let resp = match client
        .get(url)
        .header(header::ACCEPT, crate::network::http::ACCEPT_IMAGE)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(%url, ?e, "image fetch failed");
            return None;
        }
    };
    if !resp.status().is_success() {
        debug!(%url, status = %resp.status(), "image HTTP non-success");
        return None;
    }
    match resp.bytes().await {
        Ok(b) => Some(b.to_vec()),
        Err(e) => {
            warn!(%url, ?e, "image body read failed");
            None
        }
    }
}

fn cache_filename(url: &str) -> String {
    let mut h = Md5::new();
    h.update(url.as_bytes());
    format!("{:x}", h.finalize())
}

/// Deterministic accent color derived from a feed name or URL — port of NNW's
/// `ColorHash`. Returns a `#rrggbb` hex string suitable for CSS or AdwAvatar
/// custom colors. The mapping is stable across runs.
pub fn color_for(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    let d = h.finalize();
    format!("#{:02x}{:02x}{:02x}", d[0], d[1], d[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_filename_is_md5_hex() {
        let n = cache_filename("https://example.com/favicon.ico");
        assert_eq!(n.len(), 32);
        // Stable across runs.
        assert_eq!(n, cache_filename("https://example.com/favicon.ico"));
    }

    #[test]
    fn color_for_is_deterministic() {
        let a = color_for("Daring Fireball");
        let b = color_for("Daring Fireball");
        assert_eq!(a, b);
        assert!(a.starts_with('#'));
        assert_eq!(a.len(), 7);
    }
}
