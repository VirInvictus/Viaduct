// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! v2.6.24 — in-memory activity ring buffer for the refresh pipeline.
//!
//! Mirrors NetNewsWire's Activity Log surface. Every per-feed terminal
//! state in `refresh_one_feed` (success / 304 / HTTP error / network
//! error / parse error / DB error) and the pre-pipeline skip branches
//! push an `ActivityEvent` here. The GTK side reads a snapshot to
//! render the dialog.
//!
//! Sized to ~500 events. At ~256 bytes/event that's ~125 KB — within
//! the per-cycle delta budget of the v2.6.16 instrumentation arc and
//! well under what we already keep in the ImageCache LRU.

use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

const RING_CAPACITY: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    DisallowedHost,
    CacheControl,
    Throttled,
}

#[derive(Debug, Clone)]
pub enum ActivityKind {
    Skipped(SkipReason),
    NotModified,
    HttpError {
        status: u16,
    },
    NetworkError {
        detail: String,
    },
    ParseError {
        detail: String,
    },
    DbError {
        detail: String,
    },
    Success {
        new: usize,
        updated: usize,
        deleted: usize,
    },
}

#[derive(Debug, Clone)]
pub struct ActivityEvent {
    pub at: DateTime<Utc>,
    pub feed_id: String,
    pub feed_url: String,
    pub feed_name: Option<String>,
    pub kind: ActivityKind,
}

#[derive(Default)]
pub struct ActivityLog {
    events: RwLock<VecDeque<ActivityEvent>>,
}

impl ActivityLog {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: RwLock::new(VecDeque::with_capacity(RING_CAPACITY)),
        })
    }

    pub fn push(&self, ev: ActivityEvent) {
        let mut g = self.events.write().unwrap();
        if g.len() == RING_CAPACITY {
            g.pop_front();
        }
        g.push_back(ev);
    }

    /// Returns events ordered most-recent first.
    pub fn snapshot(&self) -> Vec<ActivityEvent> {
        let g = self.events.read().unwrap();
        g.iter().rev().cloned().collect()
    }

    pub fn clear(&self) {
        self.events.write().unwrap().clear();
    }

    pub fn len(&self) -> usize {
        self.events.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.read().unwrap().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(feed: &str, kind: ActivityKind) -> ActivityEvent {
        ActivityEvent {
            at: Utc::now(),
            feed_id: feed.to_string(),
            feed_url: format!("https://example.com/{feed}"),
            feed_name: None,
            kind,
        }
    }

    #[test]
    fn ring_capacity_evicts_oldest() {
        let log = ActivityLog::new();
        for i in 0..(RING_CAPACITY + 50) {
            log.push(ev(&format!("f{i}"), ActivityKind::NotModified));
        }
        assert_eq!(log.len(), RING_CAPACITY);
        // Newest first; the freshest push had id `f{cap+49}`.
        let snap = log.snapshot();
        assert_eq!(snap[0].feed_id, format!("f{}", RING_CAPACITY + 49));
        // The oldest 50 were evicted, so the tail is `f50`.
        assert_eq!(snap.last().unwrap().feed_id, "f50");
    }

    #[test]
    fn snapshot_is_newest_first() {
        let log = ActivityLog::new();
        log.push(ev("a", ActivityKind::NotModified));
        log.push(ev("b", ActivityKind::NotModified));
        log.push(ev(
            "c",
            ActivityKind::Success {
                new: 1,
                updated: 0,
                deleted: 0,
            },
        ));
        let snap = log.snapshot();
        assert_eq!(snap[0].feed_id, "c");
        assert_eq!(snap[1].feed_id, "b");
        assert_eq!(snap[2].feed_id, "a");
    }

    #[test]
    fn clear_drops_all() {
        let log = ActivityLog::new();
        log.push(ev("x", ActivityKind::NotModified));
        log.push(ev("y", ActivityKind::NotModified));
        assert_eq!(log.len(), 2);
        log.clear();
        assert!(log.is_empty());
    }
}
