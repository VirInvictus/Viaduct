// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Phase 7 memory checkpoint harness.
//!
//! Runs `LocalAccount::update_feed` through the real single-writer worker
//! against a synthetic 500-feed × 10-article corpus, then reads `VmHWM`
//! (peak resident set, kB) from `/proc/self/status` and reports pass/fail
//! against the roadmap's 500 MB peak / 100–300 MB idle targets.
//!
//! This exercises the DB + parser + serde path end-to-end. It does **not**
//! warm the favicon / image cache — that would require a live network or a
//! synthetic stub, both out of scope for port-first. The idle target is
//! therefore a lower bound; real-world idle after image-cache warm will be
//! higher but must still land inside 100–300 MB.
//!
//! Usage:
//!   cargo run --release --bin mem_check
//!
//! Run in release mode — debug builds carry enough instrumentation that the
//! reported peak will be misleading.

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use viaduct::database::accounts::Account;
use viaduct::database::{self};
use viaduct::models::{Author, ParsedItem};

const FEED_COUNT: usize = 500;
const ARTICLES_PER_FEED: usize = 10;

const PEAK_BUDGET_MB: u64 = 500;
const IDLE_TARGET_LOW_MB: u64 = 100;
const IDLE_TARGET_HIGH_MB: u64 = 300;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let elapsed = start.elapsed();

    let peak = read_vm_hwm_mb().unwrap_or(0);
    let rss = read_vm_rss_mb().unwrap_or(0);

    println!("== results ==");
    println!("insert time: {:?}", elapsed);
    println!("peak RSS (VmHWM): {} MB", peak);
    println!("current RSS (VmRSS): {} MB", rss);

    let mut failed = false;
    if peak > PEAK_BUDGET_MB {
        eprintln!(
            "FAIL: peak RSS {} MB exceeds hard budget of {} MB",
            peak, PEAK_BUDGET_MB
        );
        failed = true;
    } else {
        println!("PASS: peak RSS under {} MB budget", PEAK_BUDGET_MB);
    }
    if rss < IDLE_TARGET_LOW_MB {
        println!(
            "NOTE: current RSS {} MB is below the {} MB lower idle target — expected because the image/favicon cache hasn't been warmed.",
            rss, IDLE_TARGET_LOW_MB
        );
    } else if rss > IDLE_TARGET_HIGH_MB {
        eprintln!(
            "FAIL: current RSS {} MB exceeds idle target of {} MB",
            rss, IDLE_TARGET_HIGH_MB
        );
        failed = true;
    } else {
        println!(
            "PASS: current RSS {} MB within idle band ({}–{} MB)",
            rss, IDLE_TARGET_LOW_MB, IDLE_TARGET_HIGH_MB
        );
    }

    // Cleanup (best-effort).
    let _ = std::fs::remove_dir_all(&tmp);

    if failed {
        std::process::exit(1);
    }
    Ok(())
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
