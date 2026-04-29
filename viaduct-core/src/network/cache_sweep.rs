// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Disk cache sweepers (v2.6.5).
//!
//! The `ImageCache` happily writes favicon / image / video-thumbnail
//! bytes to `$XDG_CACHE_HOME/viaduct/{favicons,images,video-thumbs}/`
//! but never deletes anything. After a year of normal use the favicon
//! dir is full of icons for feeds the user unsubscribed from long ago,
//! the image dir holds inline-`<img>` payloads for articles that
//! retention pruned out, and video-thumbs/ keeps thumbs for now-deleted
//! articles. Two sweep strategies, picked per cache kind:
//!
//! * **Targeted (favicons)** — caller passes the live set of URLs
//!   currently referenced by `feed_settings`. We hash to the same
//!   md5-hex filename that `cache::cache_filename` produces, walk the
//!   dir, and delete files whose name isn't in the set. Exact, fast,
//!   no false positives.
//!
//! * **Age-based (images, video-thumbs)** — there's no clean "live
//!   set" for either: it would mean parsing every article's HTML for
//!   `<img>` URLs (images) or running `detect_video` over every
//!   article body (video-thumbs). Instead we rely on the fact that
//!   article retention is bounded (`retention_days`, default 30): any
//!   cached file whose mtime is older than the retention window is
//!   for an article that's been pruned out, so it's by definition a
//!   ghost. Caller picks the threshold.
//!
//! Both sweeps are best-effort: per-file failures log and continue.
//! Caller logs a single summary line after the chain.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::warn;

/// Walk `dir` and delete any file whose filename isn't in
/// `live_filenames`. Returns the number of files deleted. Used for the
/// favicon sweep where we know the exact set of live md5-hex names from
/// the `feed_settings` table.
pub fn sweep_targeted(dir: &Path, live_filenames: &HashSet<String>) -> usize {
    if !dir.exists() {
        return 0;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(?dir, ?e, "cache sweep: read_dir failed");
            return 0;
        }
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Skip any subdirectories. The cache layout is intentionally
        // flat (`<dir>/<md5>`); a nested directory is either user-
        // created clutter or a broken filesystem state, and either way
        // not our problem.
        if entry.file_type().map(|t| !t.is_file()).unwrap_or(true) {
            continue;
        }
        if live_filenames.contains(name) {
            continue;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(?path, ?e, "cache sweep: remove_file failed");
            continue;
        }
        removed += 1;
    }
    removed
}

/// Walk `dir` and delete any file whose mtime is older than
/// `max_age_days`. Returns the number of files deleted. Used for image +
/// video-thumbnail sweeps where computing a live set would mean parsing
/// every article body — age-based wins because article retention is
/// itself age-based, so any cached file that survived past retention is
/// by definition for a pruned article.
pub fn sweep_by_age(dir: &Path, max_age_days: u64) -> usize {
    if !dir.exists() {
        return 0;
    }
    let cutoff = match SystemTime::now().checked_sub(Duration::from_secs(max_age_days * 86400)) {
        Some(t) => t,
        None => return 0, // arithmetic overflow on absurd inputs; bail
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(?dir, ?e, "cache sweep: read_dir failed");
            return 0;
        }
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|t| !t.is_file()).unwrap_or(true) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        // mtime gates the decision. atime would be ideal (cache hit
        // = touched) but most filesystems mount with `noatime` or
        // `relatime` so reads don't reliably bump it. mtime gets
        // rewritten on every cache write, which is the operation that
        // matters for "still in active use".
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified > cutoff {
            continue;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(?path, ?e, "cache sweep: remove_file failed");
            continue;
        }
        removed += 1;
    }
    removed
}

/// Hash every URL in `urls` to the same md5-hex filename
/// `cache::cache_filename` uses, returning the live-set the targeted
/// sweep expects. Empty / blank URLs are dropped.
pub fn live_filenames_for(urls: &[String]) -> HashSet<String> {
    urls.iter()
        .filter(|u| !u.trim().is_empty())
        .map(|u| crate::network::cache::cache_filename(u))
        .collect()
}

/// Wipe every file in `dir` regardless of age or live-set membership.
/// Used by the v2.6.5 `win.debug-clear-caches` action. Returns the
/// number of files deleted; subdirectories are skipped (the cache
/// layout is flat by design).
pub fn wipe_dir(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(?dir, ?e, "cache wipe: read_dir failed");
            return 0;
        }
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|t| !t.is_file()).unwrap_or(true) {
            continue;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(?path, ?e, "cache wipe: remove_file failed");
            continue;
        }
        removed += 1;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Roll a unique temp dir without pulling the `tempfile` crate
    /// into core's dep tree (matches the pattern in `bin/mem_check.rs`).
    /// Cleanup happens via `std::fs::remove_dir_all` in a `Drop`.
    struct ScopedDir(PathBuf);
    impl ScopedDir {
        fn new(label: &str) -> Self {
            let pid = std::process::id();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("viaduct-cache-sweep-{label}-{pid}-{ts}-{counter}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScopedDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn live_filenames_round_trips_through_cache_filename() {
        let urls = vec![
            "https://example.com/favicon.ico".to_string(),
            "https://other.example/icon.png".to_string(),
        ];
        let set = live_filenames_for(&urls);
        assert_eq!(set.len(), 2);
        for url in &urls {
            assert!(set.contains(&crate::network::cache::cache_filename(url)));
        }
    }

    #[test]
    fn live_filenames_skips_blank() {
        let urls = vec!["".to_string(), "   ".to_string(), "https://x".to_string()];
        let set = live_filenames_for(&urls);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn sweep_targeted_keeps_live_drops_orphans() {
        let dir = ScopedDir::new("targeted");
        let live_url = "https://example.com/favicon.ico";
        let orphan_url = "https://orphan.test/icon.ico";
        let live_name = crate::network::cache::cache_filename(live_url);
        let orphan_name = crate::network::cache::cache_filename(orphan_url);
        touch(&dir.path().join(&live_name), b"icon");
        touch(&dir.path().join(&orphan_name), b"icon");

        let live: HashSet<String> = std::iter::once(live_name.clone()).collect();
        let removed = sweep_targeted(dir.path(), &live);
        assert_eq!(removed, 1);
        assert!(dir.path().join(&live_name).exists());
        assert!(!dir.path().join(&orphan_name).exists());
    }

    #[test]
    fn sweep_targeted_handles_missing_dir() {
        let missing = std::path::PathBuf::from("/tmp/viaduct-test-no-such-dir-2026-04-29");
        let removed = sweep_targeted(&missing, &HashSet::new());
        assert_eq!(removed, 0);
    }

    #[test]
    fn sweep_by_age_drops_old_keeps_fresh() {
        let dir = ScopedDir::new("age");
        let old = dir.path().join("aaa");
        let fresh = dir.path().join("bbb");
        touch(&old, b"old");
        touch(&fresh, b"fresh");

        // Backdate the old file 90 days. Fresh keeps current mtime.
        let ninety_days_ago = SystemTime::now() - Duration::from_secs(90 * 86400);
        let f = fs::File::options().write(true).open(&old).unwrap();
        let times = fs::FileTimes::new()
            .set_modified(ninety_days_ago)
            .set_accessed(ninety_days_ago);
        f.set_times(times).unwrap();

        let removed = sweep_by_age(dir.path(), 60);
        assert_eq!(removed, 1);
        assert!(!old.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn sweep_by_age_handles_missing_dir() {
        let missing = std::path::PathBuf::from("/tmp/viaduct-test-no-such-dir-2026-04-29-b");
        let removed = sweep_by_age(&missing, 30);
        assert_eq!(removed, 0);
    }

    #[test]
    fn wipe_dir_removes_every_file() {
        let dir = ScopedDir::new("wipe");
        touch(&dir.path().join("a"), b"x");
        touch(&dir.path().join("b"), b"y");
        touch(&dir.path().join("c"), b"z");

        let removed = wipe_dir(dir.path());
        assert_eq!(removed, 3);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn wipe_dir_handles_missing_dir() {
        let missing = std::path::PathBuf::from("/tmp/viaduct-test-no-such-dir-2026-04-29-c");
        let removed = wipe_dir(&missing);
        assert_eq!(removed, 0);
    }
}
