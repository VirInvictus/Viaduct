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
//! signals), so each in-memory LRU is **byte-bounded** (`ByteLru`, per-kind
//! ceilings) to keep the cache's share of the 500 MB peak-RSS budget hard-capped
//! regardless of entry-size mix; per-download size caps stop any single body from
//! blowing it. Decode-to-`gdk::Texture` happens on the GTK main thread at the call
//! site — we deliberately store `Vec<u8>` here so the LRU is `Send` and lives on
//! the tokio side.
//!
//! Missing favicons should fall back to `adw::Avatar` with the feed's display name;
//! `color_for(feed_name)` provides a deterministic accent color (port of NNW's
//! `ColorHash`) for callers that want to colorize their fallback widget.

use lru::LruCache;
use md5::{Digest, Md5};
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// v2.8.0: per-download response-size caps. A single body can't exceed
/// these no matter what a server streams. Article `<img>` loads route
/// through here via the `viaduct-img://` scheme handler, so without a cap a
/// pathological image could blow the "supreme" 500 MB peak budget on its
/// own. Generous enough that no real favicon / inline image / thumbnail
/// hits them.
const FAVICON_MAX_BYTES: usize = 1024 * 1024; // 1 MB
const IMAGE_MAX_BYTES: usize = 20 * 1024 * 1024; // 20 MB
const VIDEO_THUMB_MAX_BYTES: usize = 4 * 1024 * 1024; // 4 MB

/// v2.8.0: per-kind ceilings on the *total* bytes held in each in-memory
/// LRU. The stock `lru::LruCache` bounds entry count, not size, so a run of
/// large images could pile up past the cache's share of the budget even
/// with a count cap. `ByteLru` evicts least-recently-used entries until the
/// total is back under these. Combined worst case ~96 MB, comfortably
/// inside the 100–300 MB idle target.
const FAVICON_CACHE_BYTES: usize = 16 * 1024 * 1024; // 16 MB
const IMAGE_CACHE_BYTES: usize = 64 * 1024 * 1024; // 64 MB
const VIDEO_THUMB_CACHE_BYTES: usize = 16 * 1024 * 1024; // 16 MB

/// Byte-bounded LRU over `Vec<u8>` payloads. The stock `lru::LruCache`
/// caps entry *count*; we cap *total bytes* instead so the in-memory image
/// cache has a hard resident-size ceiling regardless of entry-size mix.
/// Backed by an unbounded `LruCache` (count never binds) with manual
/// byte-accounted eviction so the running total stays exact.
struct ByteLru {
    inner: LruCache<String, Vec<u8>>,
    bytes: usize,
    max_bytes: usize,
}

impl ByteLru {
    fn new(max_bytes: usize) -> Self {
        Self {
            inner: LruCache::unbounded(),
            bytes: 0,
            max_bytes,
        }
    }

    fn get(&mut self, key: &str) -> Option<&Vec<u8>> {
        self.inner.get(key)
    }

    fn put(&mut self, key: String, value: Vec<u8>) {
        let added = value.len();
        if let Some(old) = self.inner.put(key, value) {
            self.bytes = self.bytes.saturating_sub(old.len());
        }
        self.bytes += added;
        // Evict LRU until back under the ceiling. Keep at least the entry we
        // just inserted, so a single item larger than the ceiling is still
        // served once rather than evicting into emptiness.
        while self.bytes > self.max_bytes && self.inner.len() > 1 {
            match self.inner.pop_lru() {
                Some((_, evicted)) => self.bytes = self.bytes.saturating_sub(evicted.len()),
                None => break,
            }
        }
    }

    fn clear(&mut self) {
        self.inner.clear();
        self.bytes = 0;
    }
}

#[derive(Clone)]
pub struct ImageCache {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    favicon_dir: PathBuf,
    image_dir: PathBuf,
    video_thumb_dir: PathBuf,
    favicons: ByteLru,
    images: ByteLru,
    video_thumbs: ByteLru,
    client: Client,
}

impl ImageCache {
    pub fn new(favicon_dir: PathBuf, image_dir: PathBuf, video_thumb_dir: PathBuf) -> Self {
        let client = crate::network::http::build_default_client()
            .expect("Failed to build reqwest client for ImageCache");
        Self {
            inner: Arc::new(Mutex::new(Inner {
                favicon_dir,
                image_dir,
                video_thumb_dir,
                favicons: ByteLru::new(FAVICON_CACHE_BYTES),
                images: ByteLru::new(IMAGE_CACHE_BYTES),
                video_thumbs: ByteLru::new(VIDEO_THUMB_CACHE_BYTES),
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
        let bytes = match download(&client, url, kind.max_bytes()).await {
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

impl Kind {
    fn max_bytes(self) -> usize {
        match self {
            Kind::Favicon => FAVICON_MAX_BYTES,
            Kind::Image => IMAGE_MAX_BYTES,
            Kind::VideoThumb => VIDEO_THUMB_MAX_BYTES,
        }
    }
}

impl Inner {
    fn lru_mut(&mut self, kind: Kind) -> &mut ByteLru {
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

async fn download(client: &Client, url: &str, max_bytes: usize) -> Option<Vec<u8>> {
    use reqwest::header;
    let mut resp = match client
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

    // Reject obviously-oversized bodies before reading a byte when the
    // server is honest about Content-Length.
    if let Some(len) = resp.content_length()
        && len as usize > max_bytes
    {
        debug!(%url, len, max_bytes, "image over size cap (content-length); skipping");
        return None;
    }

    // Stream the body, aborting if the running total crosses the cap —
    // Content-Length can be absent or lie, so this is the real guard.
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len() + chunk.len() > max_bytes {
                    debug!(%url, max_bytes, "image over size cap (streamed); skipping");
                    return None;
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                warn!(%url, ?e, "image body read failed");
                return None;
            }
        }
    }
    Some(buf)
}

pub(crate) fn cache_filename(url: &str) -> String {
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

    #[test]
    fn byte_lru_evicts_until_under_ceiling() {
        // Ceiling of 250 bytes; insert five 100-byte entries. After each
        // put the total must stay <= ceiling, and the oldest entries get
        // evicted (LRU order).
        let mut lru = ByteLru::new(250);
        for i in 0..5 {
            lru.put(format!("k{i}"), vec![0u8; 100]);
            assert!(
                lru.bytes <= 250,
                "bytes {} over ceiling after {i}",
                lru.bytes
            );
        }
        // 250-byte ceiling holds at most two 100-byte entries.
        assert_eq!(lru.inner.len(), 2);
        // The two most recent survive; the oldest were evicted.
        assert!(lru.get("k4").is_some());
        assert!(lru.get("k3").is_some());
        assert!(lru.get("k0").is_none());
    }

    #[test]
    fn byte_lru_replacing_a_key_accounts_for_old_bytes() {
        let mut lru = ByteLru::new(10_000);
        lru.put("k".into(), vec![0u8; 100]);
        lru.put("k".into(), vec![0u8; 40]);
        // Replacing the key must subtract the old 100 and add the new 40,
        // not double-count to 140.
        assert_eq!(lru.bytes, 40);
        assert_eq!(lru.inner.len(), 1);
    }

    #[test]
    fn byte_lru_keeps_single_oversized_entry() {
        // An item larger than the whole ceiling is still served once rather
        // than evicting itself into emptiness.
        let mut lru = ByteLru::new(100);
        lru.put("big".into(), vec![0u8; 500]);
        assert_eq!(lru.inner.len(), 1);
        assert!(lru.get("big").is_some());
    }

    #[test]
    fn byte_lru_clear_resets_byte_count() {
        let mut lru = ByteLru::new(10_000);
        lru.put("a".into(), vec![0u8; 100]);
        lru.put("b".into(), vec![0u8; 100]);
        lru.clear();
        assert_eq!(lru.bytes, 0);
        assert_eq!(lru.inner.len(), 0);
    }
}
