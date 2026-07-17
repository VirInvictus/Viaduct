// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use std::sync::Arc;

use crate::database::accounts::Account;
use crate::network::ImageCache;
use crate::paths::{favicon_cache_dir, image_cache_dir, video_thumb_cache_dir};
use crate::ui::sidebar::{SidebarItem, selected_sidebar_item};
use crate::ui::timeline::ArticleNode;

pub(crate) mod imp {
    use super::*;
    use std::cell::OnceCell;
    use std::cell::RefCell;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "window.ui")]
    pub struct ViaductWindow {
        #[template_child]
        pub outer_split_view: TemplateChild<gtk::Paned>,
        #[template_child]
        pub inner_split_view: TemplateChild<gtk::Paned>,
        #[template_child]
        pub sidebar_view: TemplateChild<crate::ui::sidebar_view::SidebarView>,
        #[template_child]
        pub timeline_view: TemplateChild<crate::ui::timeline_view::TimelineView>,
        #[template_child]
        pub article_pane: TemplateChild<crate::ui::article_pane_view::ArticlePaneView>,
        // Phase 20c: the `adw::ToastOverlay` / `adw::Toast` replacement, a
        // `GtkOverlay` with an auto-hiding `GtkRevealer` at the bottom.
        // Newest toast wins: `show_toast` cancels the pending hide before
        // arming a new one (`toast_timeout`), so a burst doesn't dismiss
        // the last message early.
        #[template_child]
        pub toast_overlay: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub toast_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub toast_label: TemplateChild<gtk::Label>,
        pub toast_timeout: RefCell<Option<glib::SourceId>>,
        // v2.6.10 refresh-progress strip (Revealer + label + bar at
        // window bottom). Hidden until a refresh cycle starts; the
        // poll loop in `show_refresh_progress` reads
        // `refresh_progress_completed` (incremented by each per-feed
        // task in the refresher) and updates fraction + label at 250ms.
        #[template_child]
        pub refresh_progress_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub refresh_progress_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub refresh_progress_bar: TemplateChild<gtk::ProgressBar>,

        pub account: OnceCell<Arc<Account>>,
        pub image_cache: OnceCell<Arc<ImageCache>>,
        /// v2.6.24 process-wide activity log shared with every
        /// AccountRefresher constructed by the window. Read-only
        /// snapshot consumed by the Activity dialog.
        pub activity_log: OnceCell<Arc<crate::network::activity::ActivityLog>>,
        pub batch_update: crate::ui::batch::BatchUpdate,
        /// Right-click context-menu state for the timeline (v1.7.1). The
        /// sidebar's equivalent right_clicked_feed/folder cells live on
        /// `SidebarView` (v2.0.0-pre3). Window-level action bodies read
        /// through `sidebar_view.take_right_clicked_*()` accessors.
        pub right_clicked_article: RefCell<Option<crate::models::Article>>,
        /// v2.7.0 — right-clicked custom Smart Feed used by the
        /// `win.delete-smart-feed` action body. Populated by the
        /// sidebar's right-click gesture handler before the popover
        /// shows; `take()` on use so the value can't bleed into a
        /// later activation.
        pub right_clicked_smart_feed: RefCell<Option<crate::smart_feeds::SmartFeed>>,
        /// Timeline right-click popover. Window-owned because the action
        /// bodies it triggers (`win.toggle-read`, `win.toggle-star`,
        /// `win.open-in-browser`, `win.open-enclosure`, `win.copy-url`)
        /// are window methods that operate on the timeline selection.
        /// `set_pointing_to` + `popup()` reuses the same widget per click.
        pub timeline_popover: OnceCell<gtk::PopoverMenu>,
        /// Periodic-refresh `glib::timeout` source ID (v1.8.0). Replaced
        /// when the user changes `refresh-interval-minutes` in
        /// preferences — the previous timeout is removed first so we
        /// don't end up with overlapping cycles after a few flips.
        pub periodic_refresh_timeout: RefCell<Option<glib::SourceId>>,
        /// v2.6.3 background-mode RSS ticker. Armed by
        /// `hide_for_background`, cancelled by `unhide_from_background`.
        /// Climbing values across overnight runs = real leak; a stable
        /// oscillation = benign heap caching.
        pub hidden_state_ticker: RefCell<Option<glib::SourceId>>,
        /// v2.6.10 refresh-progress poll loop. Set by
        /// `show_refresh_progress` (which captures the per-cycle
        /// counter into the closure and stores the SourceId here),
        /// removed by `hide_refresh_progress` at completion.
        pub refresh_progress_source: RefCell<Option<glib::SourceId>>,
        /// Whether `refresh_on_startup_if_enabled` has run. Belt-and-
        /// braces guard against the GTK Application activating twice
        /// (single-instance handoff or similar) double-refreshing.
        pub did_startup_refresh: std::cell::Cell<bool>,
        /// Monotonically-increasing counter for the sidebar selection
        /// → timeline-fetch pipeline (v1.9.0). Each click on the
        /// sidebar bumps this counter, captures the new value, and
        /// passes it to the spawned fetch task. When the task returns,
        /// it compares its captured value to the current counter — if
        /// they don't match, the user clicked again while it was
        /// running, so the result is dropped instead of overwriting
        /// the timeline. NNW's `FetchRequestQueue` analog.
        pub selection_fetch_generation: std::cell::Cell<u64>,
        /// Phase 20c: once the user presses F9, they own the sidebar's
        /// visibility and width-driven auto-collapse stops touching it.
        pub sidebar_manual_override: std::cell::Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ViaductWindow {
        const NAME: &'static str = "ViaductWindow";
        type Type = super::ViaductWindow;
        type ParentType = gtk::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            // The window.ui template references `WebKitWebView` (used
            // inside ArticlePaneView's own template) plus the
            // `ViaductArticlePaneView` and `ViaductTimelineView` custom
            // widgets by class name. Every GType must be registered
            // before the GTK builder resolves the template, otherwise
            // template loading fails.
            webkit6::WebView::ensure_type();
            crate::ui::article_pane_view::ArticlePaneView::ensure_type();
            crate::ui::timeline_view::TimelineView::ensure_type();
            crate::ui::sidebar_view::SidebarView::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ViaductWindow {}
    impl WidgetImpl for ViaductWindow {
        // Phase 20c: the `AdwBreakpoint` replacement. `AdwNavigationSplitView`
        // collapsed into a navigation stack, which `GtkPaned` has no analog
        // for, so we do the safe, non-stranding part: auto-hide the sidebar
        // below a threshold (it has an F9 toggle to bring back), leaving the
        // timeline + article side by side. The medium-width inner collapse is
        // a deferred refinement (it would need a timeline toggle or a nav
        // stack). `size_allocate` is where a plain window learns its width.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let should_show = width >= SIDEBAR_AUTOHIDE_WIDTH;
            if !self.sidebar_manual_override.get()
                && self.sidebar_view.get().get_visible() != should_show
            {
                // Via the wrapper so the focus-move-out-of-sidebar guard runs.
                self.obj().set_sidebar_visible(should_show);
            }
        }
    }
    impl WindowImpl for ViaductWindow {}
    impl ApplicationWindowImpl for ViaductWindow {}
}

/// Below this window width the sidebar auto-hides (an F9 toggle brings it
/// back). Mirrors the old outer `AdwBreakpoint` at 600sp.
const SIDEBAR_AUTOHIDE_WIDTH: i32 = 600;

glib::wrapper! {
    pub struct ViaductWindow(ObjectSubclass<imp::ViaductWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl ViaductWindow {
    pub fn new(app: &gtk::Application, account: Arc<Account>) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        window.imp().account.set(account).ok();
        // Build the image cache rooted at our XDG cache subdirs. Errors here
        // shouldn't be possible — `paths::ensure_dirs()` ran during startup —
        // but if they are, fall back to placeholder paths under /tmp so the
        // window still opens (favicons silently fail).
        let favicons = favicon_cache_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
        let images = image_cache_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
        let video_thumbs =
            video_thumb_cache_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
        window
            .imp()
            .image_cache
            .set(Arc::new(ImageCache::new(favicons, images, video_thumbs)))
            .ok();
        window
            .imp()
            .activity_log
            .set(crate::network::activity::ActivityLog::new())
            .ok();
        // Phase 18 (v2.0.0-pre1): the article-pane WebView lives inside
        // the ViaductArticlePaneView custom widget now. Bootstrap it with
        // the shared ImageCache so the `viaduct-img://` scheme resolves;
        // this also wires the reader-button toggle, the play-video click
        // handler, and the WebKit lockdown profile.
        window
            .imp()
            .article_pane
            .get()
            .bootstrap(window.image_cache());
        window.wire_models();
        crate::ui::actions::install(&window, app);

        // Clean shutdown for the v1.7.1 right-click popovers. The
        // popovers are parented to the list views via `set_parent` in
        // `wire_context_menus`; without explicit unparenting, GTK emits
        // a "Finalizing GtkListView, but it still has children left:
        // GtkPopoverMenu …" warning during teardown. Non-fatal but
        // ugly in the logs. close_request fires before the widget
        // tree starts unwinding, so unparenting here keeps cleanup
        // ordered.
        let weak_for_close = window.downgrade();
        window.connect_close_request(move |_| {
            let Some(window) = weak_for_close.upgrade() else {
                return glib::Propagation::Proceed;
            };

            // Phase 17 background mode: when the user has enabled
            // run-in-background, hide the window instead of quitting so
            // the periodic-refresh timer keeps firing. The portal grant
            // (org.freedesktop.portal.Background) was requested when
            // they flipped the GSetting on; without it, on Flatpak the
            // host will eventually still terminate the process.
            let run_in_background = crate::preferences::settings()
                .map(|s| s.boolean("run-in-background"))
                .unwrap_or(false);
            if run_in_background {
                window.hide_for_background();
                return glib::Propagation::Stop;
            }

            let imp = window.imp();
            // Sidebar popovers live on SidebarView (v2.0.0-pre3); the
            // timeline popover stays on the window because its menu
            // items hit window-level actions.
            imp.sidebar_view.get().unparent_popovers();
            if let Some(popover) = imp.timeline_popover.get() {
                popover.unparent();
            }
            glib::Propagation::Proceed
        });

        if crate::is_debug_mode() {
            let debug_section = gio::Menu::new();
            debug_section.append(Some("Crash (Panic)"), Some("win.debug-crash"));
            debug_section.append(Some("Wipe Disk Caches"), Some("win.debug-clear-caches"));
            debug_section.append(Some("Memory snapshot"), Some("win.debug-memory-snapshot"));
            window
                .imp()
                .sidebar_view
                .get()
                .primary_menu()
                .append_submenu(Some("Debug"), &debug_section);
        }

        window
    }

    pub(crate) fn account(&self) -> Arc<Account> {
        self.imp()
            .account
            .get()
            .cloned()
            .expect("ViaductWindow constructed without Account")
    }

    pub(crate) fn activity_log(&self) -> Arc<crate::network::activity::ActivityLog> {
        self.imp()
            .activity_log
            .get()
            .cloned()
            .expect("ViaductWindow constructed without ActivityLog")
    }

    pub(crate) fn image_cache(&self) -> Arc<ImageCache> {
        self.imp()
            .image_cache
            .get()
            .cloned()
            .expect("ViaductWindow constructed without ImageCache")
    }

    /// Toast surface for adjacent UI modules (Add Feed dialog and the
    /// future context-menu actions). Same wrapper `show_toast` uses
    /// internally; this is just a `pub` re-export so the dialog can
    /// surface success / failure feedback through the same channel.
    pub fn show_toast_public(&self, message: &str) {
        self.show_toast(message);
    }

    /// Re-export for the Add Feed dialog: kicks off a one-shot refresh
    /// of just the supplied feeds (the freshly-added one). Same body as
    /// the OPML-import path uses.
    pub fn refresh_specific_feeds_public(&self, feeds: Vec<crate::models::Feed>) {
        self.refresh_specific_feeds(feeds);
    }

    /// Hide the window for the run-in-background mode (Phase 17). The
    /// process keeps running so the periodic-refresh `glib::timeout`
    /// keeps firing; reopening via the dock icon routes through
    /// `Application::activate` which re-presents this same window.
    /// Sheds resident memory by dropping the ImageCache LRU, idling the
    /// article-pane WebView, and compacting the timeline `ListStore`.
    /// `reload_current_timeline` repopulates from the still-selected
    /// sidebar item on re-show, so the user lands back where they left
    /// off (and with any articles fetched while the window was hidden).
    fn hide_for_background(&self) {
        let imp = self.imp();

        if let Some(cache) = imp.image_cache.get() {
            cache.clear_memory();
        }

        // Article pane: idle the WebProcess (`about:blank`), drop the
        // article body + display state, hide the play-video button.
        imp.article_pane.get().idle_for_background();

        // Compact the timeline list store. TimelineView's
        // `connect_items_changed` flips the stack to the empty page
        // automatically, so no explicit stack call is needed.
        imp.timeline_view.get().clear();

        self.set_visible(false);

        // v2.6.3 diagnostics: snapshot post-clear RSS and arm the
        // hidden-state ticker. Verifies v1.10.0's "≤ 100 MB hidden"
        // target and detects the run-in-background memory growth the
        // user reported (~450 MB peak after a long session).
        let (rss_mb, peak_mb) = crate::read_memory_mb();
        tracing::info!(rss_mb, peak_mb, "diag: hide_for_background post-clear");
        self.arm_hidden_state_ticker();
    }

    /// Inverse of `hide_for_background`. Called from `main.rs build_ui`
    /// when GApplication re-activates an existing window (dock-icon
    /// click, D-Bus activation, tray Show). Cancels the periodic
    /// hidden-state ticker and logs the re-show RSS so the delta from
    /// the last hide line shows the re-summon cost.
    pub fn unhide_from_background(&self) {
        if let Some(source) = self.imp().hidden_state_ticker.borrow_mut().take() {
            source.remove();
        }
        let (rss_mb, peak_mb) = crate::read_memory_mb();
        tracing::info!(rss_mb, peak_mb, "diag: reload_current_timeline re-show");
    }

    /// Arm a 5-minute `glib::timeout_add_seconds_local` that logs
    /// VmRSS while the window is hidden. No-op when one is already
    /// armed (defensive: `hide_for_background` running twice without
    /// a re-show shouldn't double-arm).
    fn arm_hidden_state_ticker(&self) {
        const TICK_SECS: u32 = 300;
        let imp = self.imp();
        if imp.hidden_state_ticker.borrow().is_some() {
            return;
        }
        let weak = self.downgrade();
        let source = glib::timeout_add_seconds_local(TICK_SECS, move || {
            // Stop firing once the window is gone.
            let Some(_window) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let (rss_mb, peak_mb) = crate::read_memory_mb();
            tracing::info!(rss_mb, peak_mb, "diag: background tick");
            glib::ControlFlow::Continue
        });
        imp.hidden_state_ticker.borrow_mut().replace(source);
    }

    /// Read the current OPML's folder names so the Add Feed dialog can
    /// populate its folder dropdown. Thin pass-through to the
    /// `SidebarView` accessor.
    pub fn list_folder_names_public(&self) -> Vec<String> {
        self.imp().sidebar_view.get().list_folder_names()
    }

    fn wire_models(&self) {
        let imp = self.imp();

        // Phase 18 (v2.0.0-pre1): the WebKit lockdown, link interceptor,
        // viaduct-img:// / viaduct-font:// scheme handlers, and hover URL
        // overlay all moved into `ArticlePaneView::bootstrap`, called from
        // `ViaductWindow::new` as soon as the ImageCache is available.

        // Re-render the article pane when:
        //   * the user changes the article-theme GSetting, or
        //   * the libadwaita color scheme flips (so "auto" mode swaps
        //     Sepia ↔ Tiqoe Dark live).
        // No-op when no article is selected — `refresh_render` clears the
        // pane in that case.
        let pane_for_theme = imp.article_pane.get().downgrade();
        if let Some(settings) = crate::preferences::settings() {
            settings.connect_changed(
                Some(crate::preferences::keys::ARTICLE_THEME),
                move |_, _| {
                    if let Some(pane) = pane_for_theme.upgrade() {
                        pane.refresh_render();
                    }
                },
            );
        }
        // Phase 20b: our own portal-backed resolution rather than
        // `adw::StyleManager::connect_dark_notify`. The listener is held
        // weakly against the pane and pruned when it dies, which is the leak
        // the pilot's migration exposed: closures registered on the
        // StyleManager singleton were never disconnected.
        let pane = imp.article_pane.get();
        let pane_for_dark = pane.downgrade();
        crate::theme::connect_dark_changed(&pane, move |_dark| {
            if let Some(pane) = pane_for_dark.upgrade() {
                pane.refresh_render();
            }
        });

        // Phase 18 (v2.0.0-pre3): SidebarView owns the sidebar list view,
        // delegate / controller / data source, popovers, and the
        // feed_names resolver. `bootstrap` builds the tree, sets up the
        // row factory, wires the right-click context menus.
        imp.sidebar_view
            .get()
            .bootstrap(self.account(), self.image_cache());
        let feed_names = imp.sidebar_view.get().feed_names();

        // Phase 18 (v2.0.0-pre2): TimelineView owns the timeline list
        // view + store + selection + search bar + scope toggle + the
        // FTS5 debounce pipeline. `bootstrap` creates the store, sets up
        // the row factory, wires the search-button / search-bar bind
        // and the FTS handler.
        imp.timeline_view.get().bootstrap(
            self.account(),
            self.image_cache(),
            feed_names,
            &imp.sidebar_view.get().search_btn(),
        );

        self.install_timeline_capture_shortcuts();

        // Article pane starts in the empty state. The pane's own template
        // defaults to its empty page; calling `clear()` here is
        // belt-and-braces to make sure leftover state from a prior window
        // doesn't leak through (relevant once we have multi-window
        // support; today it's a one-line no-op).
        imp.article_pane.get().clear();

        // Initial OPML load — populate the sidebar. `Account::load_opml`
        // calls `tokio::fs`, which requires a tokio runtime context — and
        // `glib::spawn_future_local` runs on the GLib main loop, NOT on tokio.
        // Hop through `spawn_on_runtime` for the read, deliver the parsed
        // OpmlFile back through a oneshot, and apply it on the GTK thread
        // via `sidebar_view.apply_opml`.
        let account = self.account();
        let window_weak_for_load = self.downgrade();
        let (load_tx, load_rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = load_tx.send(account.load_opml().await);
        });
        glib::spawn_future_local(async move {
            match load_rx.await {
                Ok(Ok(opml)) => {
                    if let Some(window) = window_weak_for_load.upgrade() {
                        // v2.6.23: first-launch welcome dialog. Fires
                        // when the OPML is empty (no standalone feeds,
                        // no folder feeds) AND the welcome-shown
                        // GSetting is false. Modal over the main
                        // window so the user gets a clear next action
                        // instead of an empty sidebar.
                        let opml_empty = opml.standalone_feeds.is_empty()
                            && opml.folders.iter().all(|f| f.feeds.is_empty());
                        window.imp().sidebar_view.get().apply_opml(opml);
                        window.refresh_unread_counts();
                        window.reload_custom_smart_feeds();
                        if opml_empty && crate::ui::welcome_dialog::should_present() {
                            crate::ui::welcome_dialog::present(&window);
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(?e, "failed to load OPML at startup");
                    // v2.6.5: surface a toast so the user knows feeds
                    // didn't load. Pre-v2.6.5 the sidebar just sat
                    // empty and the user had no signal whether the
                    // OPML was malformed, missing, or just genuinely
                    // empty (fresh install, no feeds yet).
                    if let Some(window) = window_weak_for_load.upgrade() {
                        window.show_toast("Couldn't load local.opml — see log for details.");
                    }
                }
                Err(_) => tracing::warn!("OPML load task aborted"),
            }
        });

        self.wire_sidebar_selection();

        self.wire_timeline_selection();

        // Mark-all-read button (lives in the sidebar header bar) — fires
        // the same action as Ctrl+K so click and keyboard share a code
        // path.
        let window_weak_for_mark = self.downgrade();
        imp.sidebar_view
            .get()
            .mark_all_read_btn()
            .connect_clicked(move |_| {
                if let Some(window) = window_weak_for_mark.upgrade() {
                    window.act_mark_all_read();
                }
            });

        // Search wiring (v1.8.0 + Phase 18) lives entirely inside
        // TimelineView::bootstrap now — including the bidirectional
        // search-button bind, the FTS5 debounce, and the scope-toggle
        // re-trigger. Nothing to do at the window level.

        self.wire_context_menus();
        self.wire_auto_refresh();
    }

    /// Sidebar selection to timeline fetch (split out of wire_models, v2.8.x).
    fn wire_sidebar_selection(&self) {
        let sidebar_selection = self.imp().sidebar_view.get().selection();
        // Sidebar selection → timeline fetch.
        let account_for_sidebar = self.account();
        let window_weak_for_sidebar = self.downgrade();
        sidebar_selection.connect_selection_changed(move |sel, _pos, _n| {
            let Some(item) = selected_sidebar_item(sel) else {
                return;
            };
            // Track which feed (if any) is selected so the search scope toggle
            // knows what "this feed" means.
            if let Some(window) = window_weak_for_sidebar.upgrade() {
                let feed_id = if let SidebarItem::Feed(ref feed) = item {
                    Some(feed.id.clone())
                } else {
                    None
                };
                window
                    .imp()
                    .timeline_view
                    .get()
                    .set_selected_feed_id(feed_id);
                // (Phase 20c: the `GtkPaned` shell has no navigation stack to
                // push, so the old collapsed-mode `set_show_content` is gone;
                // the timeline pane is always visible beside the sidebar.)
            }
            // v1.9.0: cancel-stale-fetch + tracing instrumentation.
            //
            // Bump the generation counter; capture this click's value;
            // if a second click happens before the fetch completes, the
            // captured value won't match the current counter at apply
            // time and we drop the result instead of overwriting the
            // timeline with stale data. Also lets us count cancelled
            // fetches in the logs so users can tell us when their
            // selection isn't sticking.
            let account = account_for_sidebar.clone();
            let window_weak_for_fetch = window_weak_for_sidebar.clone();
            let item_label = sidebar_item_label(&item);
            let generation = if let Some(window) = window_weak_for_fetch.upgrade() {
                let imp = window.imp();
                let g = imp.selection_fetch_generation.get().wrapping_add(1);
                imp.selection_fetch_generation.set(g);
                g
            } else {
                return;
            };
            glib::spawn_future_local(async move {
                let click_at = std::time::Instant::now();
                let sort = current_timeline_sort();
                let result: crate::error::Result<Vec<_>> = match item {
                    SidebarItem::Feed(feed) => account.fetch_articles_by_feed(feed.id, sort).await,
                    SidebarItem::SmartFeed(name) => match name.as_str() {
                        "Today" => account.fetch_today_articles(sort).await,
                        "All Unread" => account.fetch_unread_articles(sort).await,
                        "Starred" => account.fetch_starred_articles(sort).await,
                        _ => Ok(Vec::new()),
                    },
                    SidebarItem::Folder(folder) => {
                        fetch_folder_articles(&account, &folder, sort).await
                    }
                    SidebarItem::CustomSmartFeed(sf) => {
                        account.fetch_smart_feed_articles(sf.rules, sort).await
                    }
                    SidebarItem::SmartFeedGroup | SidebarItem::CustomSmartFeedsGroup => {
                        Ok(Vec::new())
                    }
                };
                let fetch_ms = click_at.elapsed().as_millis();

                // Cancel-stale guard: if the user clicked again while
                // we were in the DB, drop the result.
                if let Some(window) = window_weak_for_fetch.upgrade() {
                    let current = window.imp().selection_fetch_generation.get();
                    if current != generation {
                        tracing::info!(
                            target: "viaduct::perf",
                            item = %item_label,
                            generation,
                            current,
                            fetch_ms = fetch_ms as u64,
                            "selection fetch dropped — newer click in flight"
                        );
                        return;
                    }
                } else {
                    return;
                }

                match result {
                    Ok(articles) => {
                        let count = articles.len();
                        let Some(window) = window_weak_for_fetch.upgrade() else {
                            return;
                        };
                        let timeline = window.imp().timeline_view.get();

                        let populate_at = std::time::Instant::now();
                        timeline.populate(articles);
                        let populate_ms = populate_at.elapsed().as_millis();

                        let status_at = std::time::Instant::now();
                        timeline.refresh_statuses(account.clone());
                        let status_ms = status_at.elapsed().as_millis();

                        let total_ms = click_at.elapsed().as_millis() as u64;
                        let line = "selection navigation";
                        if total_ms >= 500 {
                            tracing::warn!(
                                target: "viaduct::perf",
                                item = %item_label,
                                articles = count,
                                fetch_ms = fetch_ms as u64,
                                populate_ms = populate_ms as u64,
                                status_ms = status_ms as u64,
                                total_ms,
                                "{line} (slow)"
                            );
                        } else {
                            tracing::info!(
                                target: "viaduct::perf",
                                item = %item_label,
                                articles = count,
                                fetch_ms = fetch_ms as u64,
                                populate_ms = populate_ms as u64,
                                status_ms = status_ms as u64,
                                total_ms,
                                "{line}"
                            );
                        }
                    }
                    Err(e) => tracing::warn!(
                        target: "viaduct::perf",
                        item = %item_label,
                        ?e,
                        "selection fetch failed"
                    ),
                }
            });
        });
    }

    /// Timeline selection to article render (split out of wire_models, v2.8.x).
    fn wire_timeline_selection(&self) {
        let timeline_selection = self.imp().timeline_view.get().selection();
        // Timeline selection → article render.
        let window_weak_for_article = self.downgrade();
        let account_for_article = self.account();
        timeline_selection.connect_selection_changed(move |_sel, _pos, _n| {
            let Some(window) = window_weak_for_article.upgrade() else {
                return;
            };
            let Some((node, article)) = window.selected_article() else {
                return;
            };
            let external = article.external_url.clone().or(article.url.clone());
            let feed_id = article.feed_id.clone();

            // Build the metadata that the NNW theme template wants:
            // title, byline, feed name + URL, publication date. The actual
            // string formatting / HTML escaping happens in render_article_body
            // when the substitutions dict is constructed.
            let title = article.title.clone().unwrap_or_else(|| "Untitled".into());
            let feed_link_title = window
                .imp()
                .sidebar_view
                .get()
                .feed_names()
                .borrow()
                .get(&feed_id)
                .cloned()
                .unwrap_or_default();
            let byline = article
                .authors
                .first()
                .and_then(|a| a.name.clone())
                .unwrap_or_default();
            // feed_link is filled in by the post-load fetch_feed_settings path
            // below if the feed has a home_page_url; for now stub to empty.
            let feed_link = String::new();

            // Prefer content_html → content_text → summary, in order. NNW
            // does the equivalent fall-through. If everything is empty
            // (sparse feeds like pragprog.com that ship title+link only),
            // synthesize a minimal HTML stub so the article pane shows the
            // title and an "open in browser" link instead of going blank.
            let body = article
                .content_html
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or(article.content_text.clone())
                .filter(|s| !s.trim().is_empty())
                .or(article.summary.clone())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| build_empty_body_fallback(&article));

            let is_stub = body.len() < 500
                || body.contains("Read more")
                || body.contains("Continue reading")
                || body.contains("Continue Reading");
            let has_url = external.is_some();

            // Hand the article + metadata to the pane. ArticleRenderContext
            // bundles every input the pane needs; the pane resets reader
            // state, refreshes the play-video button, and re-renders.
            let detected = crate::network::video_thumbs::detect_video(&article);
            window.imp().article_pane.get().set_article(
                crate::ui::article_pane_view::ArticleRenderContext {
                    raw_html: body,
                    article_url: external,
                    title,
                    byline,
                    feed_link,
                    feed_link_title,
                    date_published: article.date_published,
                    video: detected,
                },
            );
            // Adaptive layout (v1.5.5): when the inner split view is
            // collapsed, push to the article page so the user actually
            // sees the article they just selected. Without this, the
            // user taps an article in the timeline and the screen
            // doesn't change — the article stays hidden behind the
            // collapsed nav stack.
            //
            // (Phase 20c: no navigation stack on the `GtkPaned` shell; the
            // article pane is always visible beside the timeline.)

            // Auto-mark-read on selection — port of NNW
            // `tableViewSelectionDidChange` (TimelineViewController.swift:931).
            // Optimistic: flip the node first so the title goes dim and the
            // sidebar's unread totals can re-fetch. The DB upsert is async
            // and the sidebar refresh follows it.
            if !node.is_read() {
                node.set_status(true, node.is_starred());
                let status = crate::models::ArticleStatus {
                    article_id: article.article_id.clone(),
                    read: true,
                    starred: node.is_starred(),
                    date_arrived: chrono::Utc::now(),
                };
                window.upsert_and_refresh(vec![status], "auto-mark-read upsert failed");
            }

            // Async-resolve the feed's readerViewAlwaysEnabled preference
            // and push it into the pane. The pane flips the reader button
            // on (which fires the toggle handler installed in bootstrap
            // and kicks off the readability extraction).
            let account = account_for_article.clone();
            let window_weak = window_weak_for_article.clone();
            glib::spawn_future_local(async move {
                let auto = account
                    .fetch_feed_settings(feed_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|s| s.reader_view_always_enabled)
                    .unwrap_or(false)
                    || (is_stub && has_url);
                if let Some(window) = window_weak.upgrade() {
                    window.imp().article_pane.get().set_auto_reader(auto);
                }
            });
        });
    }

    /// Build the three right-click popover menus (timeline rows,
    /// sidebar feeds, sidebar folders) and attach `gtk::GestureClick`
    /// controllers to the list views that fire them.
    ///
    /// The hit-testing strategy is `widget.pick(x, y)` to get the leaf
    /// widget under the cursor, then walk up parents looking for a
    /// widget that has `viaduct-article` / `viaduct-sidebar-item` data
    /// attached during the row factory's `connect_bind`. That's how we
    /// recover the bound model object for the row the user right-
    /// clicked, without restructuring `setup_sidebar_list_view` or
    /// `setup_timeline_list_view` to accept a callback parameter.
    fn wire_context_menus(&self) {
        use gtk::gdk;

        // ---- Timeline popover ----
        // Operates on the currently-selected article — the gesture
        // handler manually selects the right-clicked row first so the
        // existing `toggle-read` / `toggle-star` / `open-in-browser` /
        // `copy-url` / `open-enclosure` actions all have the right
        // article in `timeline_selection`.
        let timeline_menu = gio::Menu::new();
        let status_section = gio::Menu::new();
        status_section.append(Some("Toggle Read"), Some("win.toggle-read"));
        status_section.append(Some("Toggle Star"), Some("win.toggle-star"));
        timeline_menu.append_section(None, &status_section);
        let open_section = gio::Menu::new();
        open_section.append(Some("Open in Browser"), Some("win.open-in-browser"));
        open_section.append(Some("Open Enclosure"), Some("win.open-enclosure"));
        open_section.append(Some("Copy URL"), Some("win.copy-url"));
        timeline_menu.append_section(None, &open_section);

        // v2.6.25 share submenu — same items as the article-pane
        // header-bar share menu so muscle memory transfers.
        let share_submenu = gio::Menu::new();
        share_submenu.append(Some("Copy URL with Title"), Some("win.copy-title-and-url"));
        share_submenu.append(Some("Email Link…"), Some("win.share-email"));
        share_submenu.append(Some("Save to Pocket"), Some("win.share-pocket"));
        share_submenu.append(Some("Save to Instapaper"), Some("win.share-instapaper"));
        timeline_menu.append_submenu(Some("Send to"), &share_submenu);

        let timeline_popover = gtk::PopoverMenu::from_model(Some(&timeline_menu));
        timeline_popover.set_has_arrow(false);
        timeline_popover.set_parent(&self.imp().timeline_view.get().list_view());
        let _ = self.imp().timeline_popover.set(timeline_popover);

        let timeline_gesture = gtk::GestureClick::new();
        timeline_gesture.set_button(gdk::BUTTON_SECONDARY);
        let window_weak = self.downgrade();
        timeline_gesture.connect_pressed(move |_, _n_press, x, y| {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let listview = window.imp().timeline_view.get().list_view();
            let Some(article) = pick_article_at(listview.upcast_ref::<gtk::Widget>(), x, y) else {
                return;
            };
            // Synchronise the selection to the right-clicked row so the
            // existing actions operate on it.
            window.select_timeline_article_by_id(&article.article_id);
            window.show_timeline_popover(x, y);
        });
        self.imp()
            .timeline_view
            .get()
            .list_view()
            .add_controller(timeline_gesture);

        // The sidebar feed + folder popovers and their gesture controller
        // are wired by `SidebarView::bootstrap` (v2.0.0-pre3) — this
        // method now only owns the timeline-row popover.
    }

    /// Walk the timeline `gio::ListStore` for an article whose ID
    /// matches `article_id` and select it. Used by the timeline
    /// right-click gesture to point existing selection-bound actions
    /// at the right-clicked article. Returns true if a match was
    /// found and the selection was updated.
    fn select_timeline_article_by_id(&self, article_id: &str) -> bool {
        let store = self.imp().timeline_view.get().store();
        let selection = self.imp().timeline_view.get().selection();
        for i in 0..store.n_items() {
            let Some(obj) = store.item(i) else { continue };
            let Some(node) = obj.downcast_ref::<ArticleNode>() else {
                continue;
            };
            let Some(article) = node.article() else {
                continue;
            };
            if article.article_id == article_id {
                selection.set_selected(i);
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------
    // Action handlers — invoked via win.<name> gio::SimpleActions. See
    // src/ui/actions.rs for accelerator bindings. Bodies are filled in by
    // subsequent Phase 9 tasks; stubs emit a trace so unbound keys are
    // visible during development.
    // -----------------------------------------------------------------

    /// NNW `scrollOrGoToNextUnread` for Space. Currently the article-pane
    /// scroll is owned by WebKit (no parent `GtkScrolledWindow` since
    /// pre1.6 — that wrapper's auto-viewport was clipping articles
    /// silently because NNW themes set `html { overflow: hidden }`).
    /// Without JS we can't query scroll position from the GTK side, so
    /// the "advance at bottom" half of the NNW behavior is on hold. For
    /// now Space falls through to WebKit's native page-down — this
    /// handler is a no-op that holds the action slot. v1.3 polish will
    /// reinstate the at-bottom advance via a webkit_load_changed scroll
    /// monitor.
    pub(crate) fn act_smart_read(&self) {
        // intentionally no-op — Space goes through to WebKit
    }

    /// Companion to `act_smart_read` — Shift+Space page-up is now
    /// WebKit's native binding too. No-op shell kept so the action
    /// remains registered.
    pub(crate) fn act_scroll_up(&self) {
        // intentionally no-op — Shift+Space goes through to WebKit
    }

    // Will be re-wired once the at-bottom scroll monitor returns —
    // see act_smart_read.
    #[allow(dead_code)]
    fn mark_current_read_then_advance(&self) {
        let imp = self.imp();
        let selection = imp.timeline_view.get().selection();
        if let Some(item) = selection.selected_item()
            && let Some(node) = item.downcast_ref::<ArticleNode>()
            && !node.is_read()
            && let Some(article) = node.article()
        {
            // Optimistic local update so next-unread sees the flip
            // immediately, without waiting for the DB round-trip.
            node.set_status(true, node.is_starred());
            let status = crate::models::ArticleStatus {
                article_id: article.article_id,
                read: true,
                starred: node.is_starred(),
                date_arrived: chrono::Utc::now(),
            };
            let account = self.account();
            let window_weak = self.downgrade();
            glib::spawn_future_local(async move {
                if let Err(e) = account.upsert_statuses(vec![status]).await {
                    tracing::warn!(?e, "smart-read: upsert_statuses failed");
                    return;
                }
                if let Some(window) = window_weak.upgrade() {
                    window.refresh_unread_counts();
                }
            });
        }
        self.advance_unread(Direction::Next);
    }
    pub(crate) fn act_next_unread(&self) {
        self.advance_unread(Direction::Next);
    }
    pub(crate) fn act_prev_unread(&self) {
        self.advance_unread(Direction::Prev);
    }

    /// Move the timeline selection by one row (spec.md §5 "move down/up
    /// list"). This is the arrow / j / k binding; `n` / `minus` do the
    /// unread-skip via `advance_unread`. Previously Down/Up were wired to
    /// `advance_unread`, so Up appeared dead once every row above was read.
    pub(crate) fn act_select_next(&self) {
        self.select_row(Direction::Next);
    }
    pub(crate) fn act_select_prev(&self) {
        self.select_row(Direction::Prev);
    }

    fn select_row(&self, dir: Direction) {
        let imp = self.imp();
        let store = imp.timeline_view.get().store();
        let selection = imp.timeline_view.get().selection();
        let n = store.n_items();
        if n == 0 {
            return;
        }
        let current = selection.selected();
        let target: i64 = match (dir, current) {
            (Direction::Next, pos) if pos == gtk::INVALID_LIST_POSITION => 0,
            (Direction::Prev, pos) if pos == gtk::INVALID_LIST_POSITION => n as i64 - 1,
            (Direction::Next, pos) => pos as i64 + 1,
            (Direction::Prev, pos) => pos as i64 - 1,
        };
        if target < 0 || target >= n as i64 {
            return; // at the boundary — stay put
        }
        selection.set_selected(target as u32);
        imp.timeline_view.get().list_view().scroll_to(
            target as u32,
            gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
            None,
        );
        // Same focus hand-off as advance_unread, so Space pages the article.
        self.act_focus_article();
    }

    /// Move the timeline selection to the next (or previous) unread row.
    /// If no unread article exists in the requested direction, the selection
    /// doesn't move — matching NNW's behavior of no-op at the boundary.
    fn advance_unread(&self, dir: Direction) {
        let imp = self.imp();
        let store = imp.timeline_view.get().store();
        let selection = imp.timeline_view.get().selection();
        let n = store.n_items();
        if n == 0 {
            return;
        }
        let current = selection.selected();
        let start_ix: i64 = match (dir, current) {
            // GTK_INVALID_LIST_POSITION is u32::MAX; treat as "nothing
            // selected" and start from the ends.
            (Direction::Next, pos) if pos == gtk::INVALID_LIST_POSITION => 0,
            (Direction::Prev, pos) if pos == gtk::INVALID_LIST_POSITION => n as i64 - 1,
            (Direction::Next, pos) => pos as i64 + 1,
            (Direction::Prev, pos) => pos as i64 - 1,
        };
        let step: i64 = match dir {
            Direction::Next => 1,
            Direction::Prev => -1,
        };
        let mut i = start_ix;
        while i >= 0 && i < n as i64 {
            if let Some(item) = store.item(i as u32)
                && let Some(node) = item.downcast_ref::<ArticleNode>()
                && !node.is_read()
            {
                selection.set_selected(i as u32);
                // Keep the newly-selected row on screen.
                self.imp().timeline_view.get().list_view().scroll_to(
                    i as u32,
                    gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                    None,
                );
                // Phase 19: hand focus to the article body, so Space and
                // Shift+Space reach WebKit's native paging without a click
                // into the pane first. Only the keyboard-nav path does this;
                // doing it from the selection-changed handler instead would
                // steal focus out of the search entry every time a search
                // repopulated the timeline. Safe because the nav keys are
                // captured on the WebView too, so j/k/n/Down/Up still work
                // from there.
                self.act_focus_article();
                return;
            }
            i += step;
        }
    }
    pub(crate) fn act_toggle_read(&self) {
        self.apply_status_to_current(|node| {
            let new_read = !node.is_read();
            (new_read, node.is_starred())
        });
    }

    /// Port of NNW `markUnreadAndGoToNextUnread:` (Shift+M in our binding).
    pub(crate) fn act_mark_unread_advance(&self) {
        self.apply_status_to_current(|node| (false, node.is_starred()));
        self.advance_unread(Direction::Next);
    }

    pub(crate) fn act_toggle_star(&self) {
        self.apply_status_to_current(|node| (node.is_read(), !node.is_starred()));
    }

    pub(crate) fn act_mark_all_read(&self) {
        self.mark_read_in_range(0, None);
    }

    pub(crate) fn act_mark_all_read_advance(&self) {
        self.mark_read_in_range(0, None);
        self.advance_unread(Direction::Next);
    }

    /// "Older" = rows below the current selection in the date-desc timeline.
    /// Matches NNW `markOlderArticlesAsRead:`.
    pub(crate) fn act_mark_older_read(&self) {
        let imp = self.imp();
        let selection = imp.timeline_view.get().selection();
        let cur = selection.selected();
        if cur == gtk::INVALID_LIST_POSITION {
            return;
        }
        self.mark_read_in_range(cur + 1, None);
    }

    /// The currently-selected timeline article, decoded from the selection.
    /// `None` when nothing is selected or the selected row isn't an article.
    fn selected_article(&self) -> Option<(ArticleNode, crate::models::Article)> {
        let item = self.imp().timeline_view.get().selection().selected_item()?;
        let node = item.downcast_ref::<ArticleNode>()?;
        let article = node.article()?;
        Some((node.clone(), article))
    }

    /// Applies a per-status change to the currently-selected article, both
    /// locally (on the ArticleNode for immediate UI response) and in the DB.
    fn apply_status_to_current<F>(&self, change: F)
    where
        F: FnOnce(&ArticleNode) -> (bool, bool),
    {
        let Some((node, article)) = self.selected_article() else {
            return;
        };
        let (read, starred) = change(&node);
        node.set_status(read, starred);
        let status = crate::models::ArticleStatus {
            article_id: article.article_id,
            read,
            starred,
            date_arrived: chrono::Utc::now(),
        };
        self.upsert_and_refresh(vec![status], "status upsert failed");
    }

    /// Persist `statuses` through the single-writer DB, then refresh the
    /// sidebar's unread counts on success. `context` labels the failure log.
    fn upsert_and_refresh(
        &self,
        statuses: Vec<crate::models::ArticleStatus>,
        context: &'static str,
    ) {
        let account = self.account();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            if let Err(e) = account.upsert_statuses(statuses).await {
                tracing::warn!(?e, "{context}");
                return;
            }
            if let Some(window) = window_weak.upgrade() {
                window.refresh_unread_counts();
            }
        });
    }

    /// Mark every not-yet-read article in `[start, end)` of the timeline store
    /// as read, in one DB batch. `end=None` means through the end of the list.
    fn mark_read_in_range(&self, start: u32, end: Option<u32>) {
        let imp = self.imp();
        let store = imp.timeline_view.get().store();
        let n = store.n_items();
        let end = end.unwrap_or(n).min(n);
        let now = chrono::Utc::now();
        let mut statuses: Vec<crate::models::ArticleStatus> = Vec::new();
        for i in start..end {
            let Some(item) = store.item(i) else { continue };
            let Some(node) = item.downcast_ref::<ArticleNode>() else {
                continue;
            };
            if node.is_read() {
                continue;
            }
            let Some(article) = node.article() else {
                continue;
            };
            node.set_status(true, node.is_starred());
            statuses.push(crate::models::ArticleStatus {
                article_id: article.article_id,
                read: true,
                starred: node.is_starred(),
                date_arrived: now,
            });
        }
        if statuses.is_empty() {
            return;
        }
        self.upsert_and_refresh(statuses, "bulk read upsert failed");
    }
    pub(crate) fn act_open_in_browser(&self) {
        let Some((_, article)) = self.selected_article() else {
            return;
        };
        // NNW's preferredLink prefers externalURL (original author's URL)
        // over url (the item URL). Mirror that.
        let Some(url) = article.external_url.or(article.url) else {
            return;
        };
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>) {
            tracing::warn!(%url, ?e, "failed to launch default browser");
        }
    }

    /// Copy the current article's preferred URL to the clipboard. Toast
    /// confirmation so the user knows it worked. Same NNW preferredLink
    /// fallback (`external_url` → `url`) as `act_open_in_browser`.
    pub(crate) fn act_copy_url(&self) {
        let Some((_, article)) = self.selected_article() else {
            return;
        };
        let Some(url) = article.external_url.or(article.url) else {
            self.show_toast("This article has no URL to copy.");
            return;
        };
        self.clipboard().set_text(&url);
        self.show_toast("Article URL copied.");
    }

    /// v2.6.25 — copy "Title <newline> URL" to the clipboard. Common
    /// quick-share pattern for chat / forum quoting where pasting just
    /// a URL loses context.
    pub(crate) fn act_copy_title_and_url(&self) {
        let Some((title, url)) = self.current_article_title_and_url() else {
            self.show_toast("This article has no URL to copy.");
            return;
        };
        let payload = if title.is_empty() {
            url
        } else {
            format!("{title}\n{url}")
        };
        self.clipboard().set_text(&payload);
        self.show_toast("Article title and URL copied.");
    }

    /// v2.6.25 — open the user's default mail client with a prefilled
    /// `mailto:` carrying the article title in the subject and the URL
    /// in the body. Body uses the URL only (not the article HTML);
    /// recipients will have to click through.
    pub(crate) fn act_share_email(&self) {
        let Some((title, url)) = self.current_article_title_and_url() else {
            self.show_toast("This article has no URL to share.");
            return;
        };
        let subject = mailto_encode(&title);
        let body = mailto_encode(&url);
        let mailto = format!("mailto:?subject={subject}&body={body}");
        if let Err(e) =
            gio::AppInfo::launch_default_for_uri(&mailto, None::<&gio::AppLaunchContext>)
        {
            tracing::warn!(?e, "failed to launch mail client");
            self.show_toast("Couldn't open your mail client.");
        }
    }

    /// v2.6.25 — hand the article URL to Pocket's web "save" intent.
    /// Pocket's `https://getpocket.com/edit?url=…` redirects to login
    /// if needed; afterward the article ends up in their reading list.
    pub(crate) fn act_share_pocket(&self) {
        self.share_to(
            "https://getpocket.com/edit?url=",
            "This article has no URL to send to Pocket.",
            "Couldn't open Pocket.",
        );
    }

    /// v2.6.25 — same shape as Pocket but pointed at Instapaper.
    pub(crate) fn act_share_instapaper(&self) {
        self.share_to(
            "https://www.instapaper.com/edit?url=",
            "This article has no URL to send to Instapaper.",
            "Couldn't open Instapaper.",
        );
    }

    fn share_to(&self, prefix: &str, no_url_msg: &'static str, fail_msg: &'static str) {
        let Some((_title, url)) = self.current_article_title_and_url() else {
            self.show_toast(no_url_msg);
            return;
        };
        let target = format!("{prefix}{}", percent_encode_uri_component(&url));
        if let Err(e) =
            gio::AppInfo::launch_default_for_uri(&target, None::<&gio::AppLaunchContext>)
        {
            tracing::warn!(target = %target, ?e, "failed to launch share target");
            self.show_toast(fail_msg);
        }
    }

    fn current_article_title_and_url(&self) -> Option<(String, String)> {
        let (_, article) = self.selected_article()?;
        let url = article.external_url.clone().or(article.url.clone())?;
        let title = article.title.clone().unwrap_or_default();
        Some((title, url))
    }

    /// Programmatic toggle of the Reader View button. Lets users flip in
    /// and out of reader mode via Ctrl+Shift+R without taking their hand
    /// off the keyboard.
    pub(crate) fn act_toggle_reader(&self) {
        self.imp().article_pane.get().toggle_reader();
    }

    /// v2.2.0: present the system print dialog for the current article.
    /// Delegates to `ArticlePaneView::print`, which wraps
    /// `webkit6::PrintOperation::run_dialog(parent)`. Bound to Ctrl+P.
    /// No-op when no article is selected (the article pane is on its
    /// "No article selected" empty page).
    pub(crate) fn act_print_article(&self) {
        let parent = self.upcast_ref::<gtk::Window>();
        self.imp().article_pane.get().print(Some(parent));
    }

    /// Close the article pane in narrow / collapsed layouts so Escape
    /// returns to the timeline. In wide layouts the inner split view
    /// stays mounted (collapsing it would jolt the chrome), but the
    /// timeline selection is cleared so the article pane shows the
    /// "No article selected" empty state.
    ///
    /// v1.5.7 — also explicitly grab focus on the timeline list view
    /// after the pop. This is a defensive recovery path: if the
    /// WebKitWebView in the article pane held keyboard focus and the
    /// nav-stack pop didn't release it cleanly, the focus stays
    /// orphaned on a hidden widget and subsequent key events go
    /// nowhere. Brandon noted that pressing Esc occasionally unstuck
    /// the back / close buttons — that's what this is fixing.
    pub(crate) fn act_close_article(&self) {
        let imp = self.imp();
        // Phase 20c: the `GtkPaned` shell has no back stack to pop; Escape
        // just clears the article selection so the empty status page shows.
        imp.timeline_view
            .get()
            .selection()
            .set_selected(gtk::INVALID_LIST_POSITION);
        imp.article_pane.get().clear();
        // Focus recovery — pull keyboard focus back to the timeline so any
        // stuck WebKit / dialog focus state gets released.
        let _ = imp.timeline_view.get().list_view().grab_focus();
    }

    // ---------------------------------------------------------------
    // Right-click context-menu action bodies (v1.7.1).
    //
    // These read from `right_clicked_feed` / `right_clicked_folder` —
    // the RefCells populated by the sidebar gesture handler just
    // before the popover was shown. Each clears the cell after
    // reading so a stale value from a previous right-click can't bleed
    // into an unrelated keyboard activation.
    // ---------------------------------------------------------------

    pub(crate) fn act_refresh_clicked_feed(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        self.refresh_specific_feeds(vec![feed]);
    }

    pub(crate) fn act_copy_clicked_feed_url(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        self.clipboard().set_text(&feed.url);
        self.show_toast("Feed URL copied.");
    }

    /// Confirmation-gated feed removal. Presents an `AdwAlertDialog`
    /// with destructive-action styling on Delete; on confirm, calls
    /// `Account::remove_feed` and reloads the sidebar. Article rows
    /// for the removed feed are pruned by the next `cleanup_at_startup`
    /// cycle (we don't fire it eagerly here to keep the perceived
    /// removal latency low).
    pub(crate) fn act_delete_clicked_feed(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        let display_name = display_name_for_feed(&feed);

        let alert = crate::ui::alert::Alert::new(
            self,
            Some(&format!("Remove “{display_name}”?")),
            Some(
                "Articles already saved from this feed will be cleaned up the next time \
                 viaduct starts. Starred articles in this feed will be deleted too. This \
                 cannot be undone.",
            ),
        );
        alert.add_response("cancel", "Cancel", crate::ui::alert::ResponseStyle::Normal);
        alert.add_response(
            "delete",
            "Delete",
            crate::ui::alert::ResponseStyle::Destructive,
        );
        alert.set_default_response("cancel");

        let window_weak = self.downgrade();
        let feed_url = feed.url.clone();
        alert.present(move |response| {
            if response != "delete" {
                return;
            }
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let account = window.account();
            let url = feed_url.clone();
            let display_name = display_name.clone();
            let window_for_done = window.downgrade();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let _ = tx.send(account.remove_feed(&url).await);
            });
            glib::spawn_future_local(async move {
                match rx.await {
                    Ok(Ok(true)) => {
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast(&format!("Removed “{display_name}”."));
                            window.reload_sidebar_after_opml_change();
                        }
                    }
                    Ok(Ok(false)) => {
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast("Feed wasn't in the subscription list.");
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "remove_feed failed");
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast("Couldn't remove the feed. See the log.");
                        }
                    }
                    Err(_) => {
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast("Removal task crashed.");
                        }
                    }
                }
            });
        });
    }

    /// v2.1.0: rename a feed via the right-click menu. Shows an
    /// `AdwAlertDialog` with a single text entry pre-filled with the
    /// feed's current display name; on save, calls `Account::rename_feed`
    /// and reloads the sidebar. Empty input clears `edited_name` (reverts
    /// to the parsed feed name / URL host fallback).
    pub(crate) fn act_rename_clicked_feed(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        let current_name = display_name_for_feed(&feed);

        let alert = crate::ui::alert::Alert::new(
            self,
            Some("Rename feed"),
            Some("Choose a display name for this feed in the sidebar."),
        );
        alert.add_response("cancel", "Cancel", crate::ui::alert::ResponseStyle::Normal);
        alert.add_response("save", "Save", crate::ui::alert::ResponseStyle::Suggested);
        alert.set_default_response("save");

        let entry = gtk::Entry::builder()
            .text(&current_name)
            .activates_default(true)
            .build();
        entry.select_region(0, -1);
        alert.set_extra_child(&entry);

        let window_weak = self.downgrade();
        let feed_url = feed.url.clone();
        let entry_for_response = entry.clone();

        // Focus the entry once the window is up so the user can type
        // immediately; select_region pre-selects the name to overwrite.
        let entry_for_focus = entry.clone();
        glib::idle_add_local_once(move || {
            entry_for_focus.grab_focus();
        });

        alert.present(move |response| {
            if response != "save" {
                return;
            }
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let new_name = entry_for_response.text().to_string();
            let account = window.account();
            let url = feed_url.clone();
            let window_for_done = window.downgrade();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let _ = tx.send(account.rename_feed(&url, new_name).await);
            });
            glib::spawn_future_local(async move {
                match rx.await {
                    Ok(Ok(true)) => {
                        if let Some(window) = window_for_done.upgrade() {
                            window.reload_sidebar_after_opml_change();
                        }
                    }
                    Ok(Ok(false)) => { /* no-op rename — feed not found */ }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "rename_feed failed");
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast("Couldn't rename the feed. See the log.");
                        }
                    }
                    Err(_) => {}
                }
            });
        });
    }

    /// v2.1.0: prompt for a folder name and create it via
    /// `Account::create_folder`. The folder appears in the sidebar
    /// (empty until the user moves feeds into it via "Move to Folder…").
    pub(crate) fn act_new_folder(&self) {
        let alert = crate::ui::alert::Alert::new(
            self,
            Some("New folder"),
            Some("Folders group related feeds in the sidebar."),
        );
        alert.add_response("cancel", "Cancel", crate::ui::alert::ResponseStyle::Normal);
        alert.add_response(
            "create",
            "Create",
            crate::ui::alert::ResponseStyle::Suggested,
        );
        alert.set_default_response("create");

        let entry = gtk::Entry::builder()
            .placeholder_text("Folder name")
            .activates_default(true)
            .build();
        alert.set_extra_child(&entry);

        let window_weak = self.downgrade();
        let entry_for_response = entry.clone();

        let entry_for_focus = entry.clone();
        glib::idle_add_local_once(move || {
            entry_for_focus.grab_focus();
        });

        alert.present(move |response| {
            if response != "create" {
                return;
            }
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let name = entry_for_response.text().to_string();
            if name.trim().is_empty() {
                return;
            }
            let account = window.account();
            let window_for_done = window.downgrade();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let _ = tx.send(account.create_folder(name).await);
            });
            glib::spawn_future_local(async move {
                match rx.await {
                    Ok(Ok(true)) => {
                        if let Some(window) = window_for_done.upgrade() {
                            window.reload_sidebar_after_opml_change();
                        }
                    }
                    Ok(Ok(false)) => {
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast("A folder with that name already exists.");
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "create_folder failed");
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast("Couldn't create the folder. See the log.");
                        }
                    }
                    Err(_) => {}
                }
            });
        });
    }

    /// v2.1.0: move the right-clicked feed to a different folder (or to
    /// the standalone list). Shows an `AdwAlertDialog` with a
    /// `GtkDropDown` listing existing folders plus a leading
    /// "(No folder)" option. The currently-selected entry mirrors the
    /// feed's current location.
    pub(crate) fn act_move_clicked_feed(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        let folders = self.imp().sidebar_view.get().list_folder_names();
        // Build the dropdown's label list. Index 0 is always
        // "(No folder)" → standalone; subsequent indices map 1:1 onto
        // `folders`. Empty folder list still works — user can move any
        // currently-foldered feed back to standalone.
        let mut labels: Vec<&str> = vec!["(No folder)"];
        for f in &folders {
            labels.push(f.as_str());
        }
        let model = gtk::StringList::new(&labels);
        let dropdown = gtk::DropDown::builder().model(&model).build();

        // Best-effort: pre-select the feed's current folder. We don't
        // know it from the Feed struct alone (folder membership lives
        // in OPML), but a fresh `list_folder_names()` snapshot plus a
        // walk of the controller children gives us the answer cheaply.
        if let Some(controller) = self.imp().sidebar_view.get().controller() {
            'outer: for top in controller.root_node.child_nodes() {
                let Some(rep) = top.represented_object() else {
                    continue;
                };
                let Some(item) = rep.downcast_ref::<crate::ui::sidebar::SidebarItem>() else {
                    continue;
                };
                if let crate::ui::sidebar::SidebarItem::Folder(folder) = item {
                    for child in top.child_nodes() {
                        let Some(c_rep) = child.represented_object() else {
                            continue;
                        };
                        let Some(c_item) = c_rep.downcast_ref::<crate::ui::sidebar::SidebarItem>()
                        else {
                            continue;
                        };
                        if let crate::ui::sidebar::SidebarItem::Feed(f) = c_item
                            && f.url == feed.url
                            && let Some(idx) = folders.iter().position(|n| n == &folder.name)
                        {
                            dropdown.set_selected((idx + 1) as u32);
                            break 'outer;
                        }
                    }
                }
            }
        }

        let alert = crate::ui::alert::Alert::new(
            self,
            Some("Move feed"),
            Some("Choose where this feed should appear in the sidebar."),
        );
        alert.add_response("cancel", "Cancel", crate::ui::alert::ResponseStyle::Normal);
        alert.add_response("move", "Move", crate::ui::alert::ResponseStyle::Suggested);
        alert.set_default_response("move");
        alert.set_extra_child(&dropdown);

        let window_weak = self.downgrade();
        let feed_url = feed.url.clone();
        let dropdown_for_response = dropdown.clone();
        let folders_for_response = folders.clone();
        alert.present(move |response| {
            if response != "move" {
                return;
            }
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let selected = dropdown_for_response.selected();
            let target = if selected == 0 {
                None
            } else {
                folders_for_response
                    .get((selected as usize).saturating_sub(1))
                    .cloned()
            };
            let account = window.account();
            let url = feed_url.clone();
            let window_for_done = window.downgrade();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let _ = tx.send(account.move_feed_to_folder(&url, target).await);
            });
            glib::spawn_future_local(async move {
                match rx.await {
                    Ok(Ok(true)) => {
                        if let Some(window) = window_for_done.upgrade() {
                            window.reload_sidebar_after_opml_change();
                        }
                    }
                    Ok(Ok(false)) => { /* no-op move — same destination */ }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "move_feed_to_folder failed");
                        if let Some(window) = window_for_done.upgrade() {
                            window.show_toast("Couldn't move the feed. See the log.");
                        }
                    }
                    Err(_) => {}
                }
            });
        });
    }

    /// v2.4.0: open the Feed Settings dialog for the right-clicked feed.
    /// Two `AdwSwitchRow` toggles bound to per-feed `FeedSettings`: "New
    /// article notifications" (the actual v2.4.0 feature) and "Always
    /// use Reader View" (existing field, exposed in the UI for the first
    /// time). On save: fetch the current `FeedSettings` from the DB,
    /// mutate the two flags, upsert. The window's
    /// `dispatch_refresh_notification` reads the flag per-feed when
    /// dispatching after a refresh cycle.
    pub(crate) fn act_feed_settings(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        let display_name = display_name_for_feed(&feed);

        let alert = crate::ui::alert::Alert::new(
            self,
            Some(&format!("Settings for “{display_name}”")),
            None,
        );
        alert.add_response("cancel", "Cancel", crate::ui::alert::ResponseStyle::Normal);
        alert.add_response("save", "Save", crate::ui::alert::ResponseStyle::Suggested);
        alert.set_default_response("save");

        let (group, group_list) = crate::ui::rows::group(None, None);
        let (notif_row, notif_switch) = crate::ui::rows::switch_row(
            "New article notifications",
            Some("Show a desktop notification when this feed has new articles."),
        );
        let (reader_row, reader_switch) = crate::ui::rows::switch_row(
            "Always use Reader View",
            Some("Open every article from this feed in extracted-text mode."),
        );
        group_list.append(&notif_row);
        group_list.append(&reader_row);
        alert.set_extra_child(&group);

        // Pre-load current values so the switches reflect the existing
        // state before the user touches them.
        let account = self.account();
        let feed_id_for_load = feed.id.clone();
        let notif_for_load = notif_switch.clone();
        let reader_for_load = reader_switch.clone();
        let (load_tx, load_rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = load_tx.send(account.fetch_feed_settings(feed_id_for_load).await);
        });
        glib::spawn_future_local(async move {
            if let Ok(Ok(Some(s))) = load_rx.await {
                notif_for_load.set_active(s.new_article_notifications_enabled);
                reader_for_load.set_active(s.reader_view_always_enabled);
            }
        });

        let window_weak = self.downgrade();
        let feed_for_response = feed.clone();
        let notif_for_response = notif_switch.clone();
        let reader_for_response = reader_switch.clone();
        alert.present(move |response| {
            if response != "save" {
                return;
            }
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let account = window.account();
            let feed = feed_for_response.clone();
            let notif_on = notif_for_response.is_active();
            let reader_on = reader_for_response.is_active();
            let display_name = display_name.clone();

            crate::spawn_on_runtime(async move {
                let existing = account
                    .fetch_feed_settings(feed.id.clone())
                    .await
                    .ok()
                    .flatten();
                let mut s = existing.unwrap_or_else(|| crate::models::FeedSettings {
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
                s.new_article_notifications_enabled = notif_on;
                s.reader_view_always_enabled = reader_on;
                if let Err(e) = account.upsert_feed_settings(s).await {
                    tracing::warn!(?e, %display_name, "feed settings upsert failed");
                }
            });
        });
    }

    pub(crate) fn act_mark_clicked_feed_read(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        let account = self.account();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            // Sort doesn't matter for mark-as-read — we iterate to mark
            // every article. Use the default to satisfy the API.
            let sort = crate::database::articles::SortOrder::default();
            let articles = match account.fetch_articles_by_feed(feed.id.clone(), sort).await {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(?e, "mark-feed-read: fetch_articles_by_feed failed");
                    return;
                }
            };
            let now = chrono::Utc::now();
            let statuses: Vec<crate::models::ArticleStatus> = articles
                .into_iter()
                .map(|a| crate::models::ArticleStatus {
                    article_id: a.article_id,
                    read: true,
                    starred: false,
                    date_arrived: now,
                })
                .collect();
            if statuses.is_empty() {
                return;
            }
            if let Err(e) = account.upsert_statuses(statuses).await {
                tracing::warn!(?e, "mark-feed-read: upsert_statuses failed");
                return;
            }
            if let Some(window) = window_weak.upgrade() {
                window.refresh_unread_counts();
                window.reload_current_timeline();
            }
        });
    }

    pub(crate) fn act_mark_clicked_folder_read(&self) {
        let Some(folder) = self.imp().sidebar_view.get().take_right_clicked_folder() else {
            return;
        };
        let account = self.account();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let now = chrono::Utc::now();
            let sort = crate::database::articles::SortOrder::default();
            let mut statuses: Vec<crate::models::ArticleStatus> = Vec::new();
            for feed in &folder.feeds {
                match account.fetch_articles_by_feed(feed.id.clone(), sort).await {
                    Ok(arts) => {
                        for a in arts {
                            statuses.push(crate::models::ArticleStatus {
                                article_id: a.article_id,
                                read: true,
                                starred: false,
                                date_arrived: now,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(?e, feed_id = %feed.id, "mark-folder-read: feed fetch failed")
                    }
                }
            }
            if statuses.is_empty() {
                return;
            }
            if let Err(e) = account.upsert_statuses(statuses).await {
                tracing::warn!(?e, "mark-folder-read: upsert_statuses failed");
                return;
            }
            if let Some(window) = window_weak.upgrade() {
                window.refresh_unread_counts();
                window.reload_current_timeline();
            }
        });
    }

    /// Position and present the timeline row's context popover. Called
    /// by the right-click gesture handler attached in `wire_models`.
    pub(crate) fn show_timeline_popover(&self, x: f64, y: f64) {
        let Some(popover) = self.imp().timeline_popover.get() else {
            return;
        };
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    }

    pub(crate) fn act_open_enclosure(&self) {
        let Some((_, article)) = self.selected_article() else {
            return;
        };
        // First-attachment-only by design (see Phase 11 plan). Multi-
        // enclosure podcasts with chapter splits aren't common enough to
        // warrant a picker UI for v1.0.
        let Some(att) = article.attachments.first() else {
            return;
        };
        if let Err(e) =
            gio::AppInfo::launch_default_for_uri(&att.url, None::<&gio::AppLaunchContext>)
        {
            tracing::warn!(url = %att.url, ?e, "failed to launch enclosure handler");
        }
    }

    pub(crate) fn act_focus_search(&self) {
        let imp = self.imp();
        imp.sidebar_view.get().search_btn().set_active(true);
        imp.timeline_view.get().focus_search_entry();
    }

    /// Move keyboard focus into the article body, mirroring how
    /// `act_focus_search` jumps into the search entry. Focus is what makes
    /// WebKit's native Space / Shift+Space paging apply, so without this a
    /// mouse-free user had no way to scroll a long article: the keyboard
    /// nav actions only move the timeline selection. `advance_unread` calls
    /// this too, so `j`/`k`/`n` land here automatically; the explicit action
    /// covers articles opened by mouse click.
    pub(crate) fn act_focus_article(&self) {
        self.imp().article_pane.get().focus_article();
    }

    /// Show or hide the sidebar pane (F9). Phase 20c: a plain
    /// `GtkPaned.start-child` visibility toggle now that the shell is
    /// `GtkPaned`, not `AdwNavigationSplitView`. Pressing this hands the
    /// user control — width-driven auto-collapse (see `size_allocate`) stops
    /// managing the sidebar once they've toggled it themselves.
    pub(crate) fn act_toggle_sidebar(&self) {
        self.imp().sidebar_manual_override.set(true);
        let showing = self.imp().sidebar_view.get().get_visible();
        self.set_sidebar_visible(!showing);
    }

    /// Show or hide the sidebar pane, moving keyboard focus out of it first
    /// when hiding. Without the focus move, a `set_visible(false)` while a
    /// sidebar row (or its search entry) holds focus strands the focus on a
    /// now-hidden widget and subsequent key events go nowhere until the user
    /// clicks. Shared by `act_toggle_sidebar` (F9) and the `size_allocate`
    /// auto-collapse.
    pub(crate) fn set_sidebar_visible(&self, show: bool) {
        let imp = self.imp();
        let sidebar = imp.sidebar_view.get();
        if sidebar.get_visible() == show {
            return;
        }
        if !show
            && let Some(focus) = self.focus()
            && focus.is_ancestor(&sidebar)
        {
            let _ = imp.timeline_view.get().list_view().grab_focus();
        }
        sidebar.set_visible(show);
    }
    pub(crate) fn act_shortcuts(&self) {
        let builder = gtk::Builder::from_string(include_str!("shortcuts.ui"));
        let Some(shortcuts_window) = builder.object::<gtk::ShortcutsWindow>("shortcuts_window")
        else {
            tracing::warn!("shortcuts.ui missing 'shortcuts_window' object");
            return;
        };
        shortcuts_window.set_transient_for(Some(self));
        shortcuts_window.present();
    }

    pub(crate) fn act_preferences(&self) {
        crate::ui::preferences_dialog::present(self);
    }

    /// v2.6.24 — open the Activity Log dialog. Renders a snapshot of
    /// the per-feed terminal events recorded by the refresher (success
    /// counts, 304s, HTTP / network / parse / DB errors, skips with
    /// reason). NetNewsWire's "Activity Log" surface.
    pub(crate) fn act_activity_log(&self) {
        crate::ui::activity_dialog::present(self);
    }

    /// Open the Add Feed dialog. Port of NNW's `Add Feed` window:
    /// URL field (feed or website — discovery handles either), optional
    /// name override, optional folder selection. On submit, runs the
    /// two-pass discovery (feed-first, HTML rel=alternate fallback) on
    /// the tokio runtime, adds the result to the OPML, refreshes the
    /// sidebar, and kicks off a refresh of just the new feed so the
    /// user sees its articles immediately.
    pub(crate) fn act_add_feed(&self) {
        crate::ui::add_feed_dialog::present(self);
    }

    /// v2.7.0 — open the New Smart Feed dialog. Reads the loaded OPML's
    /// feed list to populate the "Feed is" rule's dropdown.
    pub(crate) fn act_new_smart_feed(&self) {
        crate::ui::smart_feed_dialog::present(self);
    }

    /// v2.7.0 — delete the right-clicked custom Smart Feed. Reads the
    /// stashed `right_clicked_smart_feed` cell populated by the
    /// gesture handler before the popover was shown.
    pub(crate) fn act_delete_clicked_smart_feed(&self) {
        let Some(sf) = self.imp().right_clicked_smart_feed.take() else {
            return;
        };
        let account = self.account();
        let weak_window = self.downgrade();
        let id = sf.id.clone();
        let name = sf.name.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = tx.send(account.delete_smart_feed(id).await);
        });
        glib::spawn_future_local(async move {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            match rx.await {
                Ok(Ok(true)) => {
                    window.show_toast_public(&format!("Removed Smart Feed “{name}”."));
                    window.reload_custom_smart_feeds();
                }
                Ok(Ok(false)) => {
                    window.show_toast_public("Smart Feed wasn't found.");
                }
                Ok(Err(e)) => {
                    tracing::warn!(?e, "delete_smart_feed failed");
                    window.show_toast_public("Couldn't remove Smart Feed.");
                }
                Err(_) => {}
            }
        });
    }

    /// v2.7.0 — reload custom Smart Feeds from disk and rebuild the
    /// sidebar tree. Called on startup and after every add/delete.
    pub(crate) fn reload_custom_smart_feeds(&self) {
        let account = self.account();
        let weak_window = self.downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = tx.send(account.list_smart_feeds().await);
        });
        glib::spawn_future_local(async move {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            match rx.await {
                Ok(Ok(feeds)) => {
                    window
                        .imp()
                        .sidebar_view
                        .get()
                        .apply_custom_smart_feeds(feeds);
                }
                Ok(Err(e)) => {
                    tracing::warn!(?e, "list_smart_feeds failed");
                }
                Err(_) => {}
            }
        });
    }

    /// Import OPML — port of NNW `ImportOPMLWindowController.importOPML`.
    /// Single account, no picker sheet (NNW also short-circuits when
    /// `accounts.count == 1`). The file dialog routes through
    /// `org.freedesktop.portal.FileChooser` automatically under Flatpak.
    pub(crate) fn act_import_opml(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Import OPML")
            .modal(true)
            .filters(&Self::opml_filters())
            .build();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let file = match dialog.open_future(Some(&window)).await {
                Ok(f) => f,
                Err(e) => {
                    if !e.matches(gtk::DialogError::Dismissed) {
                        tracing::warn!(?e, "import OPML: file dialog failed");
                        window.show_toast("Could not open file picker.");
                    }
                    return;
                }
            };
            let Some(path) = file.path() else {
                window.show_toast("Selected file has no local path.");
                return;
            };

            let account = window.account();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let _ = tx.send(account.import_opml(path).await);
            });
            match rx.await {
                Ok(Ok(added)) => {
                    let count = added.len();
                    window.show_toast(&format!(
                        "Imported {} feed{}.",
                        count,
                        if count == 1 { "" } else { "s" }
                    ));
                    window.reload_sidebar_after_opml_change();
                    if !added.is_empty() {
                        window.refresh_specific_feeds(added);
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(?e, "import OPML failed");
                    window.show_toast("Couldn’t import OPML — file may be malformed.");
                }
                Err(_) => {
                    tracing::warn!("import OPML: worker oneshot dropped");
                }
            }
        });
    }

    /// Export OPML — port of NNW `ExportOPMLWindowController.exportOPML`.
    pub(crate) fn act_export_opml(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Export OPML")
            .modal(true)
            .initial_name("Subscriptions-viaduct.opml")
            .filters(&Self::opml_filters())
            .build();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let file = match dialog.save_future(Some(&window)).await {
                Ok(f) => f,
                Err(e) => {
                    if !e.matches(gtk::DialogError::Dismissed) {
                        tracing::warn!(?e, "export OPML: file dialog failed");
                        window.show_toast("Could not open file picker.");
                    }
                    return;
                }
            };
            let Some(path) = file.path() else {
                window.show_toast("Chosen path is not a local file.");
                return;
            };
            let title = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("Subscriptions.opml")
                .to_string();
            let display_path = path.display().to_string();

            let account = window.account();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let _ = tx.send(account.export_opml(path, &title).await);
            });
            match rx.await {
                Ok(Ok(())) => window.show_toast(&format!("Exported to {display_path}.")),
                Ok(Err(e)) => {
                    tracing::warn!(?e, "export OPML failed");
                    window.show_toast("Couldn’t export OPML — see logs.");
                }
                Err(_) => {
                    tracing::warn!("export OPML: worker oneshot dropped");
                }
            }
        });
    }

    fn opml_filters() -> gio::ListStore {
        let store = gio::ListStore::new::<gtk::FileFilter>();
        let opml = gtk::FileFilter::new();
        opml.set_name(Some("OPML files"));
        opml.add_pattern("*.opml");
        opml.add_pattern("*.xml");
        opml.add_mime_type("text/x-opml");
        opml.add_mime_type("application/xml");
        store.append(&opml);
        let any = gtk::FileFilter::new();
        any.set_name(Some("All files"));
        any.add_pattern("*");
        store.append(&any);
        store
    }

    pub(crate) fn act_about(&self) {
        // Phase 20c: plain `gtk::AboutDialog` (was `adw::AboutDialog`). It is
        // a toplevel window, so `transient_for` + `present()` rather than the
        // adw sheet's `present(parent)`.
        let about = gtk::AboutDialog::builder()
            .program_name("viaduct")
            .version(env!("CARGO_PKG_VERSION"))
            .authors(["Brandon LaRocque"])
            .website("https://github.com/virinvictus/viaduct")
            .website_label("Project page")
            .license_type(gtk::License::MitX11)
            .transient_for(self)
            .modal(true)
            .build();
        about.present();
    }

    pub(crate) fn act_debug_crash(&self) {
        panic!("Intentional crash triggered from Debug menu");
    }

    /// Debug-only memory snapshot (v2.6.16). Dumps the current
    /// `/proc/self/smaps_rollup` breakdown and mimalloc heap stats on
    /// demand. Useful when the user notices a spike between the
    /// periodic `diag:` log lines and wants to localise it. The
    /// `diag: memory snapshot` line goes to the normal tracing log
    /// while `mi_stats_print` writes a few hundred lines of allocator
    /// state to stderr (size classes, segment counts, commit /
    /// decommit counters).
    pub(crate) fn act_debug_memory_snapshot(&self) {
        let now = crate::rss_breakdown();
        let (rss, peak) = crate::read_memory_mb();
        tracing::info!(
            rss_mb = rss,
            peak_mb = peak,
            anon_mb = now.anon_mb,
            file_mb = now.file_mb,
            shmem_mb = now.shmem_mb,
            swap_mb = now.swap_mb,
            "diag: memory snapshot"
        );
        crate::mimalloc_print_stats();
        self.show_toast(&format!(
            "Snapshot: rss={} peak={} anon={} file={} shmem={} (mimalloc stats → stderr)",
            rss, peak, now.anon_mb, now.file_mb, now.shmem_mb
        ));
    }

    /// v2.6.5: nuke every file in the three `~/.cache/viaduct/`
    /// subdirs and drop the in-memory LRU. Useful when triaging a
    /// favicon / image / video-thumb caching bug — restart with a
    /// guaranteed-empty disk cache without `rm -rf`-ing by hand. Wired
    /// only when `--debug` is on.
    pub(crate) fn act_debug_clear_caches(&self) {
        if let Some(cache) = self.imp().image_cache.get() {
            cache.clear_memory();
        }
        let weak = self.downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel::<usize>();
        crate::spawn_on_runtime(async move {
            let total = tokio::task::spawn_blocking(|| {
                use crate::network::cache_sweep::wipe_dir;
                let mut total = 0usize;
                if let Ok(p) = crate::paths::favicon_cache_dir() {
                    total += wipe_dir(&p);
                }
                if let Ok(p) = crate::paths::image_cache_dir() {
                    total += wipe_dir(&p);
                }
                if let Ok(p) = crate::paths::video_thumb_cache_dir() {
                    total += wipe_dir(&p);
                }
                total
            })
            .await
            .unwrap_or(0);
            let _ = tx.send(total);
        });
        glib::spawn_future_local(async move {
            let total = rx.await.unwrap_or(0);
            if let Some(window) = weak.upgrade() {
                window.show_toast(&format!(
                    "Wiped {total} cache file{}.",
                    if total == 1 { "" } else { "s" }
                ));
            }
        });
    }

    fn show_toast(&self, message: &str) {
        let imp = self.imp();
        imp.toast_label.set_text(message);
        imp.toast_revealer.set_reveal_child(true);
        // Newest wins: cancel the pending hide before arming a new one, or a
        // burst of toasts would let the first one's timer dismiss the last.
        if let Some(previous) = imp.toast_timeout.borrow_mut().take() {
            previous.remove();
        }
        let revealer = imp.toast_revealer.get();
        let self_weak = self.downgrade();
        let source = glib::timeout_add_seconds_local_once(4, move || {
            revealer.set_reveal_child(false);
            if let Some(window) = self_weak.upgrade() {
                *window.imp().toast_timeout.borrow_mut() = None;
            }
        });
        *imp.toast_timeout.borrow_mut() = Some(source);
    }

    /// Re-emit OPML into the sidebar tree after import. Same tokio-context
    /// hop as the startup load — `Account::load_opml` uses `tokio::fs`.
    pub(crate) fn reload_sidebar_after_opml_change(&self) {
        let account = self.account();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = tx.send(account.load_opml().await);
        });
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            match rx.await {
                Ok(Ok(opml)) => {
                    if let Some(window) = window_weak.upgrade() {
                        window.imp().sidebar_view.get().apply_opml(opml);
                        window.refresh_unread_counts();
                    }
                }
                Ok(Err(e)) => tracing::warn!(?e, "reload sidebar after OPML import failed"),
                Err(_) => tracing::warn!("reload sidebar task aborted"),
            }
        });
    }

    /// Kick `AccountRefresher::refresh_feeds` against just the feeds
    /// that were added by an import. Mirrors the post-`importOPML`
    /// `delegate.refreshAll` step in NNW, but pre-filtered to the new feeds.
    /// Recompute sidebar unread badges from the current DB state. Fires one
    /// query each for per-feed counts and Smart-Feed counts, walks the tree
    /// once to apply, and propagates folder/group totals as the sum of their
    /// children. The notify::unread-count signal on each `TreeNode` drives
    /// the actual badge re-render (see `apply_unread_badge` in sidebar.rs).
    /// Re-fetch + re-populate the timeline pane for whatever sidebar item
    /// is currently selected. Called after a refresh cycle so newly-fetched
    /// articles appear without the user needing to click around the sidebar.
    /// No-op when nothing is selected. Public so `main.rs build_ui` can
    /// repopulate after re-summoning the window from background mode.
    pub fn reload_current_timeline(&self) {
        let imp = self.imp();
        let Some(model) = imp.sidebar_view.get().list_view().model() else {
            return;
        };
        let Some(sel) = model.downcast_ref::<gtk::SingleSelection>() else {
            return;
        };
        let Some(item) = selected_sidebar_item(sel) else {
            return;
        };
        let account = self.account();
        let weak_window = self.downgrade();
        glib::spawn_future_local(async move {
            let sort = current_timeline_sort();
            let result: crate::error::Result<Vec<_>> = match item {
                SidebarItem::Feed(feed) => account.fetch_articles_by_feed(feed.id, sort).await,
                SidebarItem::SmartFeed(name) => match name.as_str() {
                    "Today" => account.fetch_today_articles(sort).await,
                    "All Unread" => account.fetch_unread_articles(sort).await,
                    "Starred" => account.fetch_starred_articles(sort).await,
                    _ => Ok(Vec::new()),
                },
                SidebarItem::Folder(folder) => fetch_folder_articles(&account, &folder, sort).await,
                SidebarItem::CustomSmartFeed(sf) => {
                    account.fetch_smart_feed_articles(sf.rules, sort).await
                }
                SidebarItem::SmartFeedGroup | SidebarItem::CustomSmartFeedsGroup => Ok(Vec::new()),
            };
            match result {
                Ok(articles) => {
                    if let Some(window) = weak_window.upgrade() {
                        let timeline = window.imp().timeline_view.get();
                        timeline.populate(articles);
                        timeline.refresh_statuses(account.clone());
                    }
                }
                Err(e) => tracing::warn!(?e, "reload_current_timeline failed"),
            }
        });
    }

    pub(crate) fn refresh_unread_counts(&self) {
        self.imp()
            .sidebar_view
            .get()
            .refresh_unread_counts(self.account());
    }

    /// Capture-phase shortcut controller scoped to the timeline `ListView`.
    /// `gtk::Application::set_accels_for_action` installs window-bubble
    /// accelerators which fire AFTER the focused widget — and `GtkListView`
    /// consumes Up/Down/Home/End/Return/space in the target phase. Without
    /// this we'd lose `j`/`k`/`Down`/`Up`/`space`/`Return` and friends as
    /// soon as the user clicked a row. By attaching a Capture-phase
    /// controller directly to the list view, the action fires before the
    /// list view's built-in handlers.
    /// Install the capture-phase navigation shortcuts on every widget that
    /// can hold keyboard focus while reading.
    ///
    /// The timeline `GtkListView` needs them to beat its own cursor
    /// handling. The article `WebKitWebView` needs them for the opposite
    /// reason (Phase 19): once focus moves into the body so Space can reach
    /// WebKit's native paging, WebKit would otherwise swallow `Down`/`Up`
    /// as scrolling and strand `j`/`k`/`n` behind the focused widget, since
    /// app accelerators run at bubble phase. Capturing here keeps spec.md
    /// §5's two halves ("Space pages the article" and "Down moves the
    /// list") true at the same time. Space is deliberately absent from
    /// `NAV_BINDINGS`, so it alone falls through to WebKit.
    ///
    /// Each widget needs its own controller; a `GtkShortcutController` can
    /// only be added to one widget.
    fn install_timeline_capture_shortcuts(&self) {
        self.imp()
            .timeline_view
            .get()
            .list_view()
            .add_controller(Self::build_nav_capture_controller());

        // `ArticlePaneView::bootstrap` builds the WebView and runs before
        // `wire_models`, so this resolves. Warn rather than skip quietly if
        // that order ever changes: the symptom would be nav keys dying
        // whenever the article body has focus, which is easy to misread as
        // a WebKit quirk.
        match self.imp().article_pane.get().web_view() {
            Some(web_view) => web_view.add_controller(Self::build_nav_capture_controller()),
            None => tracing::warn!(
                "article WebView missing when installing nav shortcuts; \
                 keyboard navigation will not work from the article pane"
            ),
        }
    }

    fn build_nav_capture_controller() -> gtk::ShortcutController {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);

        const NAV_BINDINGS: &[(&str, &str)] = &[
            ("Down", "win.select-next"),
            ("j", "win.select-next"),
            ("n", "win.next-unread"),
            ("Up", "win.select-prev"),
            ("k", "win.select-prev"),
            ("minus", "win.prev-unread"),
            // Space / Shift+Space removed — WebKit owns article-pane
            // page-down/up natively (pre1.6). Re-add once we have an
            // at-bottom monitor for the smart-read advance behaviour.
            ("r", "win.toggle-read"),
            ("m", "win.toggle-read"),
            ("<Shift>m", "win.mark-unread-advance"),
            ("s", "win.toggle-star"),
            ("b", "win.open-in-browser"),
            ("Return", "win.open-in-browser"),
            ("<Ctrl>Return", "win.open-enclosure"),
            ("l", "win.mark-all-read-advance"),
            ("o", "win.mark-older-read"),
        ];

        for (trigger_str, action_name) in NAV_BINDINGS {
            let Some(trigger) = gtk::ShortcutTrigger::parse_string(trigger_str) else {
                tracing::warn!(trigger = %trigger_str, "failed to parse capture shortcut trigger");
                continue;
            };
            let action = gtk::NamedAction::new(action_name);
            let shortcut = gtk::Shortcut::builder()
                .trigger(&trigger)
                .action(&action)
                .build();
            controller.add_shortcut(shortcut);
        }

        controller
    }
}

/// v2.6.25 — RFC 3986 unreserved-set encoder used by share targets
/// (Pocket / Instapaper / mailto). Reuses the encoder already vetted
/// for the `viaduct-img://` scheme.
fn percent_encode_uri_component(input: &str) -> String {
    crate::ui::article_renderer::percent_encode(input)
}

/// v2.6.25 — `mailto:` subject/body encoder. RFC 6068 §2 uses standard
/// percent-encoding plus `%20` for space (rather than `+`). The
/// unreserved set already encodes space as `%20`, so the same encoder
/// works for both URI-component and mailto form fields.
fn mailto_encode(input: &str) -> String {
    percent_encode_uri_component(input)
}

/// v2.6.22: read `timeline-sort-order` fresh from GSettings on the
/// GTK thread. Falls back to `NewestFirst` (the schema default) when
/// the schema isn't installed. Same `!Send` rationale as
/// `current_retention_days`.
fn current_timeline_sort() -> crate::database::articles::SortOrder {
    use crate::database::articles::SortOrder;
    crate::preferences::settings()
        .map(|s| crate::preferences::timeline_sort_order(&s))
        .unwrap_or(SortOrder::NewestFirst)
}

#[derive(Copy, Clone)]
enum Direction {
    Next,
    Prev,
}

/// Synthesize a minimal HTML body for articles whose feed shipped no
/// `<description>` / `<content>` / `<summary>` (e.g. pragprog.com items).
/// Renders title as h1 + an "Open in browser" link so the pane isn't blank.
fn build_empty_body_fallback(article: &crate::models::Article) -> String {
    let title = article.title.as_deref().unwrap_or("Untitled");
    let url = article
        .external_url
        .as_deref()
        .or(article.url.as_deref())
        .unwrap_or("");
    let mut html = format!("<h1>{}</h1>", html_escape(title));
    if !url.is_empty() {
        html.push_str(&format!(
            "<p><a href=\"{}\">Open in browser →</a></p>",
            html_escape(url)
        ));
    } else {
        html.push_str("<p><em>No content available for this article.</em></p>");
    }
    html
}

/// Resolve a friendly display name for a feed: `edited_name` (user override)
/// wins, then `name` from the parsed feed, then the URL's host as a last
/// resort. Mirrors NNW's `WebFeed.nameForDisplay` semantics for the local
/// account.
fn display_name_for_feed(feed: &crate::models::Feed) -> String {
    if let Some(edited) = feed.edited_name.as_deref()
        && !edited.is_empty()
    {
        return edited.to_string();
    }
    if let Some(name) = feed.name.as_deref()
        && !name.is_empty()
    {
        return name.to_string();
    }
    if let Ok(parsed) = url::Url::parse(&feed.url)
        && let Some(host) = parsed.host_str()
    {
        return host.to_string();
    }
    feed.url.clone()
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Walk a list view's child widget tree from the click coordinates up
/// to the first ancestor that has `viaduct-article` data attached. The
/// data is set during the timeline row factory's `connect_bind`. Used
/// by the right-click context menu so we know which article was
/// right-clicked without restructuring the row factory's signature.
fn pick_article_at(listview: &gtk::Widget, x: f64, y: f64) -> Option<crate::models::Article> {
    let leaf = listview.pick(x, y, gtk::PickFlags::DEFAULT)?;
    let mut walker: Option<gtk::Widget> = Some(leaf);
    while let Some(w) = walker {
        // SAFETY: `viaduct-article` is set by the timeline row factory's
        // bind closure (in `timeline.rs`) to a `Box<Article>`. We only
        // read it here; ownership stays with the widget.
        unsafe {
            if let Some(ptr) = w.data::<crate::models::Article>("viaduct-article") {
                return Some(ptr.as_ref().clone());
            }
        }
        walker = w.parent();
    }
    None
}

/// Short, human-readable label for a `SidebarItem`. Used in the
/// timing-info log lines from the v1.9.0 instrumentation so users can
/// see which feed / folder / smart feed they were on when slow
/// behaviour happened. Reads the feed's edited name → name → URL host
/// fallback chain rather than dumping the raw URL into the log.
fn sidebar_item_label(item: &SidebarItem) -> String {
    match item {
        SidebarItem::Feed(feed) => display_name_for_feed(feed),
        SidebarItem::Folder(folder) => format!("[{}]", folder.name),
        SidebarItem::SmartFeed(name) => format!("Smart: {name}"),
        SidebarItem::SmartFeedGroup => "Smart Feeds (group)".to_string(),
        SidebarItem::CustomSmartFeed(sf) => format!("Custom Smart Feed: {}", sf.name),
        SidebarItem::CustomSmartFeedsGroup => "My Smart Feeds (group)".to_string(),
    }
}

/// Port of NNW's folder-as-article-source behavior: a folder row in the
/// sidebar yields the union of articles across its contained feeds, sorted
/// newest-first. For port-first MVP we fan out N fetches in parallel and
/// merge in memory. With realistic feed counts (1–50 per folder) this is
/// bounded and runs entirely through the single DB-writer thread, so there's
/// no write contention to worry about.
async fn fetch_folder_articles(
    account: &std::sync::Arc<Account>,
    folder: &crate::models::Folder,
    sort: crate::database::articles::SortOrder,
) -> crate::error::Result<Vec<crate::models::Article>> {
    if folder.feeds.is_empty() {
        return Ok(Vec::new());
    }
    // v1.9.0: was previously a sequential N-round-trip fan-out despite
    // the doc-comment claim of parallelism. Now one bulk DB op via the
    // FetchByFeeds variant (IN clause). For a folder with 50 feeds
    // this drops folder-selection latency from O(N · channel + plan)
    // to O(1 · channel + plan). v2.6.22: the bulk op handles cross-
    // chunk sort internally per `SortOrder`, so no second pass needed.
    let feed_ids: Vec<String> = folder.feeds.iter().map(|f| f.id.clone()).collect();
    account.fetch_articles_by_feeds(feed_ids, sort).await
}
