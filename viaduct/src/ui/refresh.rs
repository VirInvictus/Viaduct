// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Refresh subsystem, extracted from `window.rs` (v2.8.x). Owns the refresh
//! cycle end to end: the auto-refresh wiring and periodic timer, the manual /
//! forced / per-feed entry points, the bottom progress strip, the completion
//! toast and per-feed desktop notifications, and the `RefreshTally` /
//! `RefreshProgress` value types threaded through a cycle. The methods live in
//! a second `impl ViaductWindow` block so call sites and `actions.rs` stay
//! unchanged; the value types and the `run_refresh_with_tally` engine are free
//! items, unit-tested at the bottom.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::sync::Arc;

use crate::database::accounts::Account;
use crate::ui::window::ViaductWindow;

impl ViaductWindow {
    /// Wire the v1.8.0 sync-on-open + periodic-refresh preferences.
    ///
    /// `refresh-on-startup` (default false): when true, fires one
    /// `act_refresh()` shortly after the OPML load completes. The
    /// 1500 ms delay gives the sidebar / timeline a chance to render
    /// first so the user isn't staring at an empty UI while the
    /// network round-trips.
    ///
    /// `refresh-interval-minutes` (default 0 = disabled, range 0..=1440):
    /// when > 0, installs a `glib::timeout_add_seconds_local` that calls
    /// `act_refresh()` every `interval * 60` seconds. Re-arms when the
    /// user changes the interval in preferences (cancels the old timer
    /// first so we don't pile up handlers).
    pub(crate) fn wire_auto_refresh(&self) {
        let Some(settings) = crate::preferences::settings() else {
            return;
        };

        // --- Startup refresh ---
        if crate::preferences::refresh_on_startup(&settings) {
            let weak = self.downgrade();
            // 1500 ms gives the OPML load + sidebar binding time to
            // finish so the user sees something before the spinner
            // takes over the sync button.
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                if let Some(window) = weak.upgrade() {
                    if window.imp().did_startup_refresh.get() {
                        return;
                    }
                    window.imp().did_startup_refresh.set(true);
                    window.act_refresh_periodic();
                }
            });
        }

        // --- Periodic refresh ---
        self.arm_periodic_refresh(&settings);
        let weak = self.downgrade();
        settings.connect_changed(
            Some(crate::preferences::keys::REFRESH_INTERVAL_MINUTES),
            move |s, _| {
                if let Some(window) = weak.upgrade() {
                    window.arm_periodic_refresh(s);
                }
            },
        );
    }

    /// Cancel any active periodic-refresh timer and start a new one
    /// based on the current `refresh-interval-minutes` setting. A value
    /// of 0 leaves the timer cancelled.
    fn arm_periodic_refresh(&self, settings: &gio::Settings) {
        // Always cancel the previous timer first; otherwise toggling
        // the dropdown a few times piles up handlers and we end up
        // refreshing more often than the user asked for.
        if let Some(prev) = self.imp().periodic_refresh_timeout.borrow_mut().take() {
            prev.remove();
        }
        let minutes = crate::preferences::refresh_interval_minutes(settings);
        if minutes <= 0 {
            return;
        }
        let secs: u32 = (minutes as u32).saturating_mul(60);
        let weak = self.downgrade();
        let source_id = glib::timeout_add_seconds_local(secs, move || {
            if let Some(window) = weak.upgrade() {
                window.act_refresh_periodic();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        self.imp()
            .periodic_refresh_timeout
            .borrow_mut()
            .replace(source_id);
    }
    /// User-initiated refresh (sync button, Ctrl+R, OPML import). Always
    /// `force = true`: bypasses the minimum-time skip throttle and the
    /// content-hash short-circuit so an explicit click always hits the
    /// network and re-parses. Driven from the `win.refresh` action.
    pub(crate) fn act_refresh(&self) {
        self.start_refresh(true);
    }

    /// Timer-initiated refresh (the `refresh-on-startup` and
    /// `refresh-interval-minutes` paths). Always `force = false`:
    /// conditional-GET headers stay in the request, the minimum-time skip
    /// applies, and content-hash matches short-circuit before parse.
    /// Most periodic cycles produce no DB writes — they 304 and exit.
    pub(crate) fn act_refresh_periodic(&self) {
        self.start_refresh(false);
    }

    /// Shared body of `act_refresh` and `act_refresh_periodic`. Drives
    /// `AccountRefresher::refresh_feeds[_forced]` over the full OPML.
    /// Refresher needs a tokio runtime context, so we dispatch on the
    /// global runtime. Tallies `new_articles` across the whole cycle
    /// and routes the count back to the GTK thread for an optional
    /// desktop notification.
    fn start_refresh(&self, force: bool) {
        // v2.6.13: re-entry guard. The periodic-refresh
        // `glib::timeout_add_seconds_local` calls into this directly,
        // bypassing the action-group disable that
        // `set_refresh_in_progress(true)` installs for menu/keyboard
        // entry points. At normal 30-min cadence this is harmless,
        // but at sub-cycle-duration cadence the timer fires another
        // refresh before the previous one completes, stacking N
        // overlapping cycles — each allocates its own per-cycle state,
        // so peak appears as N × per-cycle delta.
        // `refresh_progress_source` is `Some` for the duration of a
        // cycle; checking it tells us whether we're already running.
        if self.imp().refresh_progress_source.borrow().is_some() {
            tracing::debug!("start_refresh: skipping — previous cycle still in flight");
            return;
        }
        let account = self.account();
        let activity_log = Some(self.activity_log());
        let window_weak = self.downgrade();
        let retention_days = current_retention_days();
        let progress = RefreshProgress::new();
        let progress_for_runtime = progress.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<RefreshTally>();
        crate::spawn_on_runtime(async move {
            let opml = match account.load_opml().await {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(?e, "refresh: OPML load failed");
                    let _ = done_tx.send(RefreshTally::default());
                    return;
                }
            };
            let mut feeds: Vec<crate::models::Feed> = Vec::new();
            feeds.extend(opml.standalone_feeds.iter().cloned());
            for folder in &opml.folders {
                feeds.extend(folder.feeds.iter().cloned());
            }
            if feeds.is_empty() {
                let _ = done_tx.send(RefreshTally::default());
                return;
            }
            let paired = pair_feeds_with_settings(&account, feeds).await;
            let tally = run_refresh_with_tally(
                account,
                paired,
                retention_days,
                force,
                Some(progress_for_runtime),
                activity_log,
            )
            .await;
            let _ = done_tx.send(tally);
        });
        self.imp().batch_update.start();
        self.set_refresh_in_progress(true);
        self.show_refresh_progress(progress);
        glib::spawn_future_local(async move {
            let Ok(tally) = done_rx.await else {
                if let Some(window) = window_weak.upgrade() {
                    window.imp().batch_update.end();
                    window.set_refresh_in_progress(false);
                    window.hide_refresh_progress();
                }
                return;
            };
            if let Some(window) = window_weak.upgrade() {
                // v2.6.15: when run-in-background hid the window,
                // skip the toast (no visible target) and the
                // timeline ListStore repopulate (wasted work — the
                // user can't see it, and `main.rs build_ui` already
                // calls `reload_current_timeline` on re-summon).
                // Pre-v2.6.15 we unconditionally repopulated the
                // timeline every cycle while hidden, allocating
                // ~20-30 MB of `ArticleNode` + `Article` structs +
                // spawning a video-thumb fetch per row. The desktop
                // notification + sidebar unread-count refresh stay
                // unconditional: notifications go through the OS
                // regardless of window visibility, and the unread
                // walk is a cheap DB query so badges are accurate
                // the moment the user re-shows the window.
                window.dispatch_refresh_notification(&tally);
                window.refresh_unread_counts();
                if window.is_visible() {
                    window.show_refresh_toast(&tally);
                    // Re-fetch the timeline for the currently-selected sidebar
                    // item so newly-fetched articles appear without the user
                    // having to click around. Without this, the timeline shows
                    // stale (often empty) results until the next sidebar click.
                    window.reload_current_timeline();
                }
                window.imp().batch_update.end();
                window.set_refresh_in_progress(false);
                window.hide_refresh_progress();
            }
        });
    }

    /// Toast feedback so a refresh that produces no visible state change
    /// is at least surfaced. Dismissed automatically by `AdwToast`.
    /// Flip the sync button's icon → spinner. Call at refresh start;
    /// pair with `set_refresh_in_progress(false)` at completion. Also
    /// disables the `win.refresh` action while the cycle runs so a
    /// double-click can't kick off a parallel refresher (which would
    /// double the network load and produce mismatched batch_update
    /// start/end pairs).
    pub(crate) fn set_refresh_in_progress(&self, on: bool) {
        // Sync-button visual state (spinner ↔ icon) lives on SidebarView.
        // The action-disable path stays here because the gio action
        // group does too.
        self.imp().sidebar_view.get().set_refresh_in_progress(on);
        if let Some(action) = self.lookup_action("refresh")
            && let Some(simple) = action.downcast_ref::<gio::SimpleAction>()
        {
            simple.set_enabled(!on);
        }
    }

    /// v2.6.10: reveal the bottom progress strip and start a 250 ms
    /// poll loop that reads the cycle's `RefreshProgress` counters
    /// and updates the bar fraction + label. Call at the same time as
    /// `set_refresh_in_progress(true)`. The poll loop pulses
    /// indeterminately while `total == 0` (the refresher hasn't yet
    /// computed `paired.len()`) and switches to a determinate
    /// `completed / total` fraction once the total is published.
    pub(crate) fn show_refresh_progress(&self, progress: RefreshProgress) {
        let imp = self.imp();
        // If a previous cycle's poll loop is somehow still active
        // (manual + auto refresh racing), drop it so we don't
        // double-update.
        if let Some(source) = imp.refresh_progress_source.borrow_mut().take() {
            source.remove();
        }
        let bar = imp.refresh_progress_bar.get();
        let label = imp.refresh_progress_label.get();
        bar.set_fraction(0.0);
        label.set_text("Refreshing feeds…");
        imp.refresh_progress_revealer.get().set_reveal_child(true);

        let bar_weak = bar.downgrade();
        let label_weak = label.downgrade();
        let source = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            use std::sync::atomic::Ordering;
            let total = progress.total.load(Ordering::Relaxed);
            let completed = progress.completed.load(Ordering::Relaxed);
            let Some(bar) = bar_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(label) = label_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if total == 0 {
                // Pulse-mode: refresher hasn't published the count
                // yet (load_opml + pairing still running). Keeps the
                // user-visible bar moving so they know we're alive.
                bar.pulse();
                label.set_text("Refreshing feeds…");
            } else {
                let fraction = (completed as f64 / total as f64).clamp(0.0, 1.0);
                bar.set_fraction(fraction);
                label.set_text(&format!("Refreshing feeds… {completed} / {total}"));
            }
            glib::ControlFlow::Continue
        });
        imp.refresh_progress_source.borrow_mut().replace(source);
    }

    /// v2.6.10: cancel the poll loop, reset the bar to 0, and slide
    /// the strip out of view. Call at the same time as
    /// `set_refresh_in_progress(false)`.
    pub(crate) fn hide_refresh_progress(&self) {
        let imp = self.imp();
        if let Some(source) = imp.refresh_progress_source.borrow_mut().take() {
            source.remove();
        }
        imp.refresh_progress_revealer.get().set_reveal_child(false);
        imp.refresh_progress_bar.get().set_fraction(0.0);
        imp.refresh_progress_label.get().set_text("");
    }

    fn show_refresh_toast(&self, tally: &RefreshTally) {
        let total = tally.total_new_articles();
        let msg = if tally.feeds_attempted == 0 {
            "No feeds in subscription list.".to_string()
        } else if total == 0 {
            format!(
                "Refreshed {} feed{} — no new articles.",
                tally.feeds_attempted,
                if tally.feeds_attempted == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Refreshed {} feed{} — {} new article{}.",
                tally.feeds_attempted,
                if tally.feeds_attempted == 1 { "" } else { "s" },
                total,
                if total == 1 { "" } else { "s" },
            )
        };
        self.show_toast_public(&msg);
    }

    /// Show desktop notifications for a refresh cycle's new articles,
    /// **per-feed** (v2.4.0). Walks `tally.per_feed_new`; for each feed
    /// with new articles, fetches its `FeedSettings` and fires a
    /// `gio::Notification` titled with the feed's display name **only
    /// when both** the global `notifications-on-refresh` GSetting is on
    /// **and** that feed's per-feed `new_article_notifications_enabled`
    /// flag is set. Silent when either gate is off, when no feeds had
    /// new articles, or when the feed couldn't be resolved.
    fn dispatch_refresh_notification(&self, tally: &RefreshTally) {
        if tally.per_feed_new.is_empty() {
            return;
        }
        let Some(settings) = crate::preferences::settings() else {
            return;
        };
        if !crate::preferences::notifications_enabled(&settings) {
            return;
        }
        let Some(app) = self.application() else {
            return;
        };
        let account = self.account();
        let feed_names = self.imp().sidebar_view.get().feed_names();
        let app = app.clone();
        let entries: Vec<(String, usize)> = tally
            .per_feed_new
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(id, count)| (id.clone(), *count))
            .collect();
        glib::spawn_future_local(async move {
            for (feed_id, count) in entries {
                let s = match account.fetch_feed_settings(feed_id.clone()).await {
                    Ok(Some(s)) => s,
                    _ => continue,
                };
                if !s.new_article_notifications_enabled {
                    continue;
                }
                let display_name = feed_names
                    .borrow()
                    .get(&feed_id)
                    .cloned()
                    .unwrap_or_else(|| s.feed_url.clone());
                let body = if count == 1 {
                    "1 new article".to_string()
                } else {
                    format!("{count} new articles")
                };
                let notif = gio::Notification::new(&display_name);
                notif.set_body(Some(&body));
                notif.set_priority(gio::NotificationPriority::Normal);
                // Per-feed `id` so the notification daemon coalesces
                // repeated refreshes of the same feed instead of
                // stacking N notifications when a user refreshes
                // several times in quick succession.
                let id = format!("viaduct.refresh.{}", feed_id);
                app.send_notification(Some(&id), &notif);
            }
        });
    }
    pub(crate) fn refresh_specific_feeds(&self, feeds: Vec<crate::models::Feed>) {
        // v2.6.13: same re-entry guard as `act_refresh`. Used by the
        // OPML-import + Add Feed paths; if a refresh is already
        // running we skip and let the in-flight cycle pick up the
        // new feeds at next cycle (Account::add_feed has already
        // persisted them to OPML so they'll be in the next pair).
        if self.imp().refresh_progress_source.borrow().is_some() {
            tracing::debug!("refresh_specific_feeds: skipping — previous cycle still in flight");
            return;
        }
        let account = self.account();
        let activity_log = Some(self.activity_log());
        let window_weak = self.downgrade();
        let retention_days = current_retention_days();
        let progress = RefreshProgress::new();
        let progress_for_runtime = progress.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<RefreshTally>();
        self.set_refresh_in_progress(true);
        self.show_refresh_progress(progress);
        crate::spawn_on_runtime(async move {
            let paired = pair_feeds_with_settings(&account, feeds).await;
            // Force=true: post-import re-fetch is also an explicit user
            // intent, not auto-refresh.
            let tally = run_refresh_with_tally(
                account,
                paired,
                retention_days,
                true,
                Some(progress_for_runtime),
                activity_log,
            )
            .await;
            let _ = done_tx.send(tally);
        });
        glib::spawn_future_local(async move {
            let Ok(tally) = done_rx.await else {
                if let Some(window) = window_weak.upgrade() {
                    window.set_refresh_in_progress(false);
                    window.hide_refresh_progress();
                }
                return;
            };
            if let Some(window) = window_weak.upgrade() {
                // v2.6.15: same hidden-window short-circuit as
                // `act_refresh`. Notifications + sidebar unread-count
                // refresh stay unconditional; the timeline ListStore
                // repopulate waits until the user re-summons the
                // window (`main.rs build_ui` calls it on re-show).
                window.dispatch_refresh_notification(&tally);
                window.refresh_unread_counts();
                if window.is_visible() {
                    window.reload_current_timeline();
                }
                window.set_refresh_in_progress(false);
                window.hide_refresh_progress();
            }
        });
    }
}

/// Pair each feed with its persisted FeedSettings (or a blank one if the
/// feed hasn't been seen before). The refresher uses settings for
/// conditional-GET info, content hash, last_check_date, etc.
async fn pair_feeds_with_settings(
    account: &Arc<Account>,
    feeds: Vec<crate::models::Feed>,
) -> Vec<(crate::models::Feed, crate::models::FeedSettings)> {
    let mut paired = Vec::with_capacity(feeds.len());
    for feed in feeds {
        let settings = account
            .fetch_feed_settings(feed.id.clone())
            .await
            .unwrap_or(None)
            .unwrap_or_else(|| crate::models::FeedSettings {
                feed_id: feed.id.clone(),
                feed_url: feed.url.clone(),
                home_page_url: feed.home_page_url.clone(),
                icon_url: None,
                favicon_url: None,
                edited_name: feed.edited_name.clone(),
                content_hash: None,
                last_modified: None,
                etag: None,
                date_created: None,
                max_age: None,
                authors_json: None,
                folder_relationship_json: None,
                last_check_date: None,
                reader_view_always_enabled: false,
                new_article_notifications_enabled: false,
                last_response_code: None,
            });
        paired.push((feed, settings));
    }
    paired
}

/// Run a refresh cycle and return the total `new_articles` count across all
/// `ArticleChanges` batches the refresher emitted. Drops the refresher
/// before awaiting the drain task so all `changes_tx` clones close and the
/// drain returns naturally. `retention_days` is forwarded to `update_feed`
/// for the per-feed prune.
/// Result of a refresh cycle, broken out so the UI can render a toast or
/// a desktop notification with both numbers.
///
/// **v2.4.0**: now tracks new-article counts **per feed** (`per_feed_new`)
/// in addition to the feed-attempt count, so `dispatch_refresh_notification`
/// can fire one `gio::Notification` per feed with `new_article_notifications_enabled`
/// set in `FeedSettings`. The `total_new_articles()` accessor sums the
/// values for the existing toast / global-summary callers.
#[derive(Debug, Default, Clone)]
pub(crate) struct RefreshTally {
    pub feeds_attempted: usize,
    pub per_feed_new: std::collections::HashMap<String, usize>,
}

impl RefreshTally {
    pub fn total_new_articles(&self) -> usize {
        self.per_feed_new.values().sum()
    }
}

async fn run_refresh_with_tally(
    account: Arc<Account>,
    paired: Vec<(crate::models::Feed, crate::models::FeedSettings)>,
    retention_days: i64,
    force: bool,
    progress: Option<RefreshProgress>,
    activity_log: Option<Arc<crate::network::activity::ActivityLog>>,
) -> RefreshTally {
    let feeds_attempted = paired.len();
    // v2.6.10: publish the total to the GTK side BEFORE running so the
    // poll loop can switch from indeterminate-pulse to fraction mode.
    if let Some(p) = progress.as_ref() {
        p.total
            .store(feeds_attempted, std::sync::atomic::Ordering::Relaxed);
    }
    // v2.6.3 + v2.6.16: pre-cycle peak + breakdown so the post-cycle
    // line carries deltas. The breakdown (anon_mb / file_mb /
    // shmem_mb) localises growth — anon_mb climbing means heap
    // (mimalloc / Rust allocations); file_mb means SQLite mmap or
    // similar; shmem_mb means WebKit's shared regions with its
    // WebProcess child.
    let (rss_before, peak_before) = crate::read_memory_mb();
    let breakdown_before = crate::rss_breakdown();
    tracing::info!(
        rss_mb = rss_before,
        peak_mb = peak_before,
        anon_mb = breakdown_before.anon_mb,
        file_mb = breakdown_before.file_mb,
        shmem_mb = breakdown_before.shmem_mb,
        feeds_attempted,
        force,
        "diag: refresh cycle pre"
    );
    let (changes_tx, mut changes_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::models::ArticleChanges>();
    let drain = tokio::spawn(async move {
        let mut per_feed: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        while let Some(changes) = changes_rx.recv().await {
            for article in &changes.new_articles {
                *per_feed.entry(article.feed_id.clone()).or_insert(0) += 1;
            }
        }
        per_feed
    });
    let mut refresher = crate::network::AccountRefresher::new(account, changes_tx, retention_days);
    if let Some(p) = progress.as_ref() {
        refresher = refresher.with_completion_counter(p.completed.clone());
    }
    if let Some(log) = activity_log {
        refresher = refresher.with_activity_log(log);
    }
    if force {
        refresher.refresh_feeds_forced(paired).await;
    } else {
        refresher.refresh_feeds(paired).await;
    }
    drop(refresher);
    let per_feed_new = drain.await.unwrap_or_default();
    // v2.6.14: force mimalloc to return per-cycle transient pages to
    // the OS before we log post-cycle RSS. Without this, the cycle's
    // freed-but-cached allocations sit in mimalloc's internal pools
    // until the default 1 s purge delay expires — even with the
    // `MIMALLOC_PURGE_DELAY=100` startup tweak, the synchronous
    // collect makes the diagnostic log line reflect the true post-
    // cycle floor instead of an inflated transient.
    crate::mimalloc_collect();
    let (rss_after, peak_after) = crate::read_memory_mb();
    let breakdown_after = crate::rss_breakdown();
    tracing::info!(
        rss_mb = rss_after,
        peak_mb = peak_after,
        peak_delta_mb = peak_after.saturating_sub(peak_before),
        anon_mb = breakdown_after.anon_mb,
        anon_delta_mb = (breakdown_after.anon_mb as i64) - (breakdown_before.anon_mb as i64),
        file_mb = breakdown_after.file_mb,
        file_delta_mb = (breakdown_after.file_mb as i64) - (breakdown_before.file_mb as i64),
        shmem_mb = breakdown_after.shmem_mb,
        shmem_delta_mb = (breakdown_after.shmem_mb as i64) - (breakdown_before.shmem_mb as i64),
        feeds_attempted,
        "diag: refresh cycle post"
    );
    RefreshTally {
        feeds_attempted,
        per_feed_new,
    }
}

/// Pair of atomic counters threaded through `run_refresh_with_tally`
/// so the GTK-side progress poll loop can render a determinate bar.
/// `total` starts at 0 (poll loop pulses indeterminately); set to
/// `paired.len()` once the refresher has it. `completed` increments
/// in the per-feed task tail (success / 304 / error all count) and
/// also at the skip branch in the refresher loop.
#[derive(Clone)]
pub(crate) struct RefreshProgress {
    pub completed: Arc<std::sync::atomic::AtomicUsize>,
    pub total: Arc<std::sync::atomic::AtomicUsize>,
}

impl RefreshProgress {
    pub fn new() -> Self {
        Self {
            completed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            total: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

/// Read `retention-days` fresh from GSettings on the GTK thread. Falls back
/// to the schema default (30) when the schema isn't installed (dev env
/// without `glib-compile-schemas`). `gio::Settings` is `!Send`, so this
/// helper must run before we hand control to the tokio runtime.
fn current_retention_days() -> i64 {
    crate::preferences::settings()
        .map(|s| crate::preferences::retention_days(&s))
        .unwrap_or(30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_tally_total_sums_per_feed_new() {
        let mut t = RefreshTally::default();
        assert_eq!(t.total_new_articles(), 0);
        t.feeds_attempted = 3;
        t.per_feed_new.insert("feed-a".to_string(), 2);
        t.per_feed_new.insert("feed-b".to_string(), 5);
        assert_eq!(t.total_new_articles(), 7);
    }

    #[test]
    fn refresh_progress_starts_zero_and_clones_share_atomics() {
        use std::sync::atomic::Ordering;
        let p = RefreshProgress::new();
        assert_eq!(p.total.load(Ordering::Relaxed), 0);
        assert_eq!(p.completed.load(Ordering::Relaxed), 0);
        // Clones share the same atomics (the runtime side increments a clone
        // while the GTK poll loop reads the original).
        let p2 = p.clone();
        p2.completed.fetch_add(4, Ordering::Relaxed);
        p2.total.store(10, Ordering::Relaxed);
        assert_eq!(p.completed.load(Ordering::Relaxed), 4);
        assert_eq!(p.total.load(Ordering::Relaxed), 10);
    }
}
