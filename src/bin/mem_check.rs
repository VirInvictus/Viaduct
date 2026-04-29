// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Phase 7 memory checkpoint harness.
//!
//! Runs `LocalAccount::update_feed` through the real single-writer worker
//! against a synthetic 500-feed × 10-article corpus, then warms the favicon
//! and image cache against an in-process HTTP fixture (500 favicons + 50
//! images), then reads `VmHWM` (peak resident set, kB) from
//! `/proc/self/status` and reports pass/fail against the roadmap's 500 MB
//! peak / 100–300 MB idle targets.
//!
//! Two checkpoints are reported:
//!
//! - **post-DB peak** — exercises DB + parser + serde end-to-end.
//! - **post-image-warmup peak** — adds 500 favicons (1 KB) + 50 images
//!   (50 KB) routed through the real `ImageCache`, hitting LRU eviction
//!   (cap is 250/kind so 500 favicons exercise the eviction path).
//!
//! Usage:
//!
//! ```sh
//! cargo run --release --bin mem_check
//! ```
//!
//! Run in release mode — debug builds carry enough instrumentation that the
//! reported peak will be misleading.

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use viaduct::database::accounts::Account;
use viaduct::database::{self};
use viaduct::models::{Author, ParsedItem};
use viaduct::network::cache::ImageCache;

const FEED_COUNT: usize = 500;
const ARTICLES_PER_FEED: usize = 10;

const FAVICONS_TO_WARM: usize = 500;
const IMAGES_TO_WARM: usize = 50;
const SYNTH_FAVICON_BYTES: usize = 1024;
const SYNTH_IMAGE_BYTES: usize = 50 * 1024;

const PEAK_BUDGET_MB: u64 = 500;
const IDLE_TARGET_LOW_MB: u64 = 100;
const IDLE_TARGET_HIGH_MB: u64 = 300;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The library's ImageCache routes through `viaduct::spawn_on_runtime`,
    // which expects the global runtime to be installed. Match `main.rs` and
    // build the runtime explicitly so the cache can use it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handle = rt.handle().clone();
    viaduct::init_runtime(rt);
    handle.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    // Route the DBs into a tempdir instead of touching the user's XDG state.
    // Best-effort cleanup on exit; not critical if it lingers on crash.
    let tmp = make_tempdir()?;
    redirect_xdg(&tmp);
    viaduct::paths::ensure_dirs()?;

    let (db_tx, db_rx) = mpsc::channel(256);
    database::spawn_db_worker(db_rx)?;

    let (sync_tx, sync_rx) = mpsc::channel(256);
    database::spawn_sync_worker(sync_rx)?;

    let account = Arc::new(Account::new(db_tx, sync_tx).await?);

    let baseline = read_vm_hwm_mb().unwrap_or(0);
    println!(
        "== viaduct memory checkpoint ==\nfeeds: {}, articles/feed: {}, total: {}",
        FEED_COUNT,
        ARTICLES_PER_FEED,
        FEED_COUNT * ARTICLES_PER_FEED
    );
    println!("baseline peak RSS (pre-load): {} MB", baseline);

    let start = std::time::Instant::now();
    for feed_ix in 0..FEED_COUNT {
        let feed_id = format!("https://synthetic-{}.example/feed.xml", feed_ix);
        let items: Vec<ParsedItem> = (0..ARTICLES_PER_FEED)
            .map(|art_ix| synth_item(feed_ix, art_ix))
            .collect();
        account
            .update_feed(
                feed_id,
                items,
                true,
                viaduct::database::articles::DEFAULT_RETENTION_DAYS,
            )
            .await?;
    }
    let db_elapsed = start.elapsed();

    let post_db_peak = read_vm_hwm_mb().unwrap_or(0);
    let post_db_rss = read_vm_rss_mb().unwrap_or(0);

    println!("-- post-DB checkpoint --");
    println!("insert time: {:?}", db_elapsed);
    println!("peak RSS (VmHWM): {} MB", post_db_peak);
    println!("current RSS (VmRSS): {} MB", post_db_rss);

    // ---------- Image cache warmup ----------
    let warm_start = std::time::Instant::now();
    let port = spawn_synth_image_server().await?;
    let cache = ImageCache::new(
        viaduct::paths::favicon_cache_dir()?,
        viaduct::paths::image_cache_dir()?,
    );
    let mut handles = Vec::with_capacity(FAVICONS_TO_WARM + IMAGES_TO_WARM);
    for i in 0..FAVICONS_TO_WARM {
        let cache = cache.clone();
        handles.push(tokio::spawn(async move {
            cache
                .favicon(&format!("http://127.0.0.1:{}/fav-{}", port, i))
                .await
        }));
    }
    for i in 0..IMAGES_TO_WARM {
        let cache = cache.clone();
        handles.push(tokio::spawn(async move {
            cache
                .image(&format!("http://127.0.0.1:{}/img-{}", port, i))
                .await
        }));
    }
    let mut hits = 0usize;
    for h in handles {
        if let Ok(Some(_)) = h.await {
            hits += 1;
        }
    }
    let warm_elapsed = warm_start.elapsed();

    let post_warm_peak = read_vm_hwm_mb().unwrap_or(0);
    let post_warm_rss = read_vm_rss_mb().unwrap_or(0);

    println!("-- post-image-warmup checkpoint --");
    println!(
        "warmup time: {:?} ({} of {} hits)",
        warm_elapsed,
        hits,
        FAVICONS_TO_WARM + IMAGES_TO_WARM,
    );
    println!("peak RSS (VmHWM): {} MB", post_warm_peak);
    println!("current RSS (VmRSS): {} MB", post_warm_rss);

    println!("== results ==");
    let mut failed = false;
    if post_warm_peak > PEAK_BUDGET_MB {
        eprintln!(
            "FAIL: peak RSS {} MB exceeds hard budget of {} MB",
            post_warm_peak, PEAK_BUDGET_MB
        );
        failed = true;
    } else {
        println!(
            "PASS: peak RSS {} MB under {} MB budget",
            post_warm_peak, PEAK_BUDGET_MB
        );
    }
    if post_warm_rss < IDLE_TARGET_LOW_MB {
        println!(
            "NOTE: current RSS {} MB is below the {} MB lower idle target — synthetic corpus is smaller than a real-world subscription list.",
            post_warm_rss, IDLE_TARGET_LOW_MB
        );
    } else if post_warm_rss > IDLE_TARGET_HIGH_MB {
        eprintln!(
            "FAIL: current RSS {} MB exceeds idle target of {} MB",
            post_warm_rss, IDLE_TARGET_HIGH_MB
        );
        failed = true;
    } else {
        println!(
            "PASS: current RSS {} MB within idle band ({}–{} MB)",
            post_warm_rss, IDLE_TARGET_LOW_MB, IDLE_TARGET_HIGH_MB
        );
    }
    if hits < FAVICONS_TO_WARM + IMAGES_TO_WARM {
        eprintln!(
            "WARN: {} of {} cache fetches missed (synth server lossy?)",
            (FAVICONS_TO_WARM + IMAGES_TO_WARM) - hits,
            FAVICONS_TO_WARM + IMAGES_TO_WARM,
        );
    }

    // Cleanup (best-effort).
    let _ = std::fs::remove_dir_all(&tmp);

    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// Spawns a tiny in-process HTTP/1.1 server on an ephemeral port serving
/// canned PNG-shaped bytes per path-prefix (`/fav-*` → 1 KB, `/img-*` → 50 KB).
/// Returns the bound port. Survives until the process exits.
async fn spawn_synth_image_server() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            tokio::spawn(async move {
                let mut req = [0u8; 4096];
                let n = sock.read(&mut req).await.unwrap_or(0);
                let is_favicon = n > 8 && req[..n.min(64)].windows(8).any(|w| w == b"GET /fav");
                let body = if is_favicon {
                    vec![0xABu8; SYNTH_FAVICON_BYTES]
                } else {
                    vec![0xCDu8; SYNTH_IMAGE_BYTES]
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Ok(port)
}

fn make_tempdir() -> std::io::Result<PathBuf> {
    let pid = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!("viaduct-memcheck-{}-{}", pid, ts));
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

fn synth_item(feed_ix: usize, art_ix: usize) -> ParsedItem {
    // ~2 KB of body text per article. Gives a realistic footprint without
    // artificially ballooning DB size.
    let body = format!(
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Article {}/{} for synthetic feed {}. {}",
        art_ix,
        ARTICLES_PER_FEED,
        feed_ix,
        "word ".repeat(300),
    );
    ParsedItem {
        id: format!("guid-{}-{}", feed_ix, art_ix),
        title: Some(format!("Synthetic title {} / {}", feed_ix, art_ix)),
        content_html: Some(body.clone()),
        content_text: Some(body),
        url: Some(format!(
            "https://synthetic-{}.example/posts/{}",
            feed_ix, art_ix
        )),
        external_url: None,
        summary: None,
        image_url: None,
        date_published: Some(Utc::now() - chrono::Duration::hours(art_ix as i64)),
        date_modified: None,
        authors: vec![Author {
            name: Some(format!("Author {}", feed_ix % 50)),
            url: None,
            avatar_url: None,
            email: None,
        }],
        attachments: Vec::new(),
    }
}

fn redirect_xdg(tmp: &Path) {
    let data = tmp.join("data");
    let cache = tmp.join("cache");
    // SAFETY: single-threaded at this point; no one else is reading env.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", data);
        std::env::set_var("XDG_CACHE_HOME", cache);
    }
}

fn read_vm_hwm_mb() -> Option<u64> {
    read_proc_status_kb("VmHWM").map(|kb| kb / 1024)
}

fn read_vm_rss_mb() -> Option<u64> {
    read_proc_status_kb("VmRSS").map(|kb| kb / 1024)
}

fn read_proc_status_kb(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field)
            && let Some(rest) = rest.strip_prefix(':')
        {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if let Some(kb_str) = parts.first()
                && let Ok(kb) = kb_str.parse::<u64>()
            {
                return Some(kb);
            }
        }
    }
    None
}
