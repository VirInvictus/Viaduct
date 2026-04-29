// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::sync::Arc;

use crate::database::accounts::Account;
use crate::network::ImageCache;
use crate::paths::{favicon_cache_dir, image_cache_dir, video_thumb_cache_dir};
use crate::ui::sidebar::{SidebarItem, selected_sidebar_item};
use crate::ui::timeline::ArticleNode;

mod imp {
    use super::*;
    use std::cell::OnceCell;
    use std::cell::RefCell;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "window.ui")]
    pub struct ViaductWindow {
        #[template_child]
        pub outer_split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub inner_split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub sidebar_view: TemplateChild<crate::ui::sidebar_view::SidebarView>,
        #[template_child]
        pub timeline_view: TemplateChild<crate::ui::timeline_view::TimelineView>,
        #[template_child]
        pub article_pane: TemplateChild<crate::ui::article_pane_view::ArticlePaneView>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,

        pub account: OnceCell<Arc<Account>>,
        pub image_cache: OnceCell<Arc<ImageCache>>,
        pub batch_update: crate::ui::batch::BatchUpdate,
        /// Right-click context-menu state for the timeline (v1.7.1). The
        /// sidebar's equivalent right_clicked_feed/folder cells live on
        /// `SidebarView` (v2.0.0-pre3). Window-level action bodies read
        /// through `sidebar_view.take_right_clicked_*()` accessors.
        pub right_clicked_article: RefCell<Option<crate::models::Article>>,
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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ViaductWindow {
        const NAME: &'static str = "ViaductWindow";
        type Type = super::ViaductWindow;
        type ParentType = adw::ApplicationWindow;

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
    impl WidgetImpl for ViaductWindow {}
    impl WindowImpl for ViaductWindow {}
    impl ApplicationWindowImpl for ViaductWindow {}
    impl adw::subclass::prelude::AdwApplicationWindowImpl for ViaductWindow {}
}

glib::wrapper! {
    pub struct ViaductWindow(ObjectSubclass<imp::ViaductWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl ViaductWindow {
    pub fn new(app: &adw::Application, account: Arc<Account>) -> Self {
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
        let pane_for_dark = imp.article_pane.get().downgrade();
        adw::StyleManager::default().connect_dark_notify(move |_| {
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
        let sidebar_selection = imp.sidebar_view.get().selection();
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
        let timeline_selection = imp.timeline_view.get().selection();

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
                        window.imp().sidebar_view.get().apply_opml(opml);
                        window.refresh_unread_counts();
                    }
                }
                Ok(Err(e)) => tracing::warn!(?e, "failed to load OPML at startup"),
                Err(_) => tracing::warn!("OPML load task aborted"),
            }
        });

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
                // Adaptive layout (v1.5.5): when the outer split view is
                // collapsed (mobile-shaped window), tapping a sidebar
                // entry must push to the timeline page or the user is
                // stuck on the feed list with no way to read anything.
                // The split view back-pops naturally via system Back; the
                // forward push has to be explicit.
                //
                // v1.5.7 — only push when the state actually needs to
                // change. Calling set_show_content(true) while it's
                // already true (e.g. during an in-flight transition
                // animation) confuses the SplitView's state machine
                // and leaves the navigation stack stuck — symptom is
                // "back / close button doesn't respond until Escape."
                let outer = &window.imp().outer_split_view;
                if outer.is_collapsed() && !outer.shows_content() {
                    outer.set_show_content(true);
                }
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
                let result: crate::error::Result<Vec<_>> = match item {
                    SidebarItem::Feed(feed) => account.fetch_articles_by_feed(feed.id).await,
                    SidebarItem::SmartFeed(name) => match name.as_str() {
                        "Today" => account.fetch_today_articles().await,
                        "All Unread" => account.fetch_unread_articles().await,
                        "Starred" => account.fetch_starred_articles().await,
                        _ => Ok(Vec::new()),
                    },
                    SidebarItem::Folder(folder) => fetch_folder_articles(&account, &folder).await,
                    SidebarItem::SmartFeedGroup => Ok(Vec::new()),
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

        // Timeline selection → article render.
        let window_weak_for_article = self.downgrade();
        let account_for_article = self.account();
        timeline_selection.connect_selection_changed(move |sel, _pos, _n| {
            let Some(window) = window_weak_for_article.upgrade() else {
                return;
            };
            let Some(item) = sel.selected_item() else {
                return;
            };
            let Some(node) = item.downcast_ref::<ArticleNode>() else {
                return;
            };
            let Some(article) = node.article() else {
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
            // v1.5.7 — only push when state needs to change. See
            // matching note on the sidebar handler above.
            let inner = &window.imp().inner_split_view;
            if inner.is_collapsed() && !inner.shows_content() {
                inner.set_show_content(true);
            }

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
                let account = account_for_article.clone();
                let window_for_count = window.downgrade();
                glib::spawn_future_local(async move {
                    if let Err(e) = account.upsert_statuses(vec![status]).await {
                        tracing::warn!(?e, "auto-mark-read upsert failed");
                        return;
                    }
                    if let Some(window) = window_for_count.upgrade() {
                        window.refresh_unread_counts();
                    }
                });
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
    fn wire_auto_refresh(&self) {
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
                    window.act_refresh();
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
        let secs = (minutes as u32).saturating_mul(60);
        let weak = self.downgrade();
        let source_id = glib::timeout_add_seconds_local(secs, move || {
            if let Some(window) = weak.upgrade() {
                window.act_refresh();
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

    /// Applies a per-status change to the currently-selected article, both
    /// locally (on the ArticleNode for immediate UI response) and in the DB.
    fn apply_status_to_current<F>(&self, change: F)
    where
        F: FnOnce(&ArticleNode) -> (bool, bool),
    {
        let selection = self.imp().timeline_view.get().selection();
        let Some(item) = selection.selected_item() else {
            return;
        };
        let Some(node) = item.downcast_ref::<ArticleNode>() else {
            return;
        };
        let Some(article) = node.article() else {
            return;
        };
        let (read, starred) = change(node);
        node.set_status(read, starred);
        let status = crate::models::ArticleStatus {
            article_id: article.article_id,
            read,
            starred,
            date_arrived: chrono::Utc::now(),
        };
        let account = self.account();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            if let Err(e) = account.upsert_statuses(vec![status]).await {
                tracing::warn!(?e, "status upsert failed");
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
        let account = self.account();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            if let Err(e) = account.upsert_statuses(statuses).await {
                tracing::warn!(?e, "bulk read upsert failed");
                return;
            }
            if let Some(window) = window_weak.upgrade() {
                window.refresh_unread_counts();
            }
        });
    }
    pub(crate) fn act_open_in_browser(&self) {
        let selection = self.imp().timeline_view.get().selection();
        let Some(item) = selection.selected_item() else {
            return;
        };
        let Some(node) = item.downcast_ref::<ArticleNode>() else {
            return;
        };
        let Some(article) = node.article() else {
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
        let selection = self.imp().timeline_view.get().selection();
        let Some(item) = selection.selected_item() else {
            return;
        };
        let Some(node) = item.downcast_ref::<ArticleNode>() else {
            return;
        };
        let Some(article) = node.article() else {
            return;
        };
        let Some(url) = article.external_url.or(article.url) else {
            self.imp()
                .toast_overlay
                .add_toast(adw::Toast::new("This article has no URL to copy."));
            return;
        };
        self.clipboard().set_text(&url);
        self.imp()
            .toast_overlay
            .add_toast(adw::Toast::new("Article URL copied."));
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
        if imp.inner_split_view.is_collapsed() && imp.inner_split_view.shows_content() {
            // In collapsed (mobile-shaped) mode AdwNavigationSplitView
            // exposes a back stack — pop it. Skip when already at the
            // sidebar page, otherwise we briefly trigger an animation
            // that nobody asked for.
            imp.inner_split_view.set_show_content(false);
        }
        // Always clear the article display so wide-layout users get the
        // empty status page when they hit Escape too.
        imp.timeline_view
            .get()
            .selection()
            .set_selected(gtk::INVALID_LIST_POSITION);
        imp.article_pane.get().clear();
        // Focus recovery — pull keyboard focus back to the timeline so
        // any stuck WebKit / dialog focus state gets released.
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

        let alert = adw::AlertDialog::new(
            Some(&format!("Remove “{display_name}”?")),
            Some(
                "Articles already saved from this feed will be cleaned up the next time \
                 viaduct starts. Starred articles in this feed will be deleted too. This \
                 cannot be undone.",
            ),
        );
        alert.add_response("cancel", "Cancel");
        alert.add_response("delete", "Delete");
        alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let window_weak = self.downgrade();
        let feed_url = feed.url.clone();
        alert.connect_response(None, move |dialog, response| {
            if response != "delete" {
                return;
            }
            dialog.close();
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

        alert.present(Some(self));
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

        let alert = adw::AlertDialog::new(
            Some("Rename feed"),
            Some("Choose a display name for this feed in the sidebar."),
        );
        alert.add_response("cancel", "Cancel");
        alert.add_response("save", "Save");
        alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("save"));
        alert.set_close_response("cancel");

        let entry = gtk::Entry::builder()
            .text(&current_name)
            .activates_default(true)
            .build();
        entry.select_region(0, -1);
        alert.set_extra_child(Some(&entry));

        let window_weak = self.downgrade();
        let feed_url = feed.url.clone();
        let entry_for_response = entry.clone();
        alert.connect_response(None, move |dialog, response| {
            if response != "save" {
                return;
            }
            dialog.close();
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

        // Focus the entry after dialog presents so the user can type
        // immediately. The select_region above pre-selects the existing
        // name so they can overwrite it with one keystroke.
        let entry_for_focus = entry.clone();
        glib::idle_add_local_once(move || {
            entry_for_focus.grab_focus();
        });

        alert.present(Some(self));
    }

    /// v2.1.0: prompt for a folder name and create it via
    /// `Account::create_folder`. The folder appears in the sidebar
    /// (empty until the user moves feeds into it via "Move to Folder…").
    pub(crate) fn act_new_folder(&self) {
        let alert = adw::AlertDialog::new(
            Some("New folder"),
            Some("Folders group related feeds in the sidebar."),
        );
        alert.add_response("cancel", "Cancel");
        alert.add_response("create", "Create");
        alert.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("create"));
        alert.set_close_response("cancel");

        let entry = gtk::Entry::builder()
            .placeholder_text("Folder name")
            .activates_default(true)
            .build();
        alert.set_extra_child(Some(&entry));

        let window_weak = self.downgrade();
        let entry_for_response = entry.clone();
        alert.connect_response(None, move |dialog, response| {
            if response != "create" {
                return;
            }
            dialog.close();
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

        let entry_for_focus = entry.clone();
        glib::idle_add_local_once(move || {
            entry_for_focus.grab_focus();
        });

        alert.present(Some(self));
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

        let alert = adw::AlertDialog::new(
            Some("Move feed"),
            Some("Choose where this feed should appear in the sidebar."),
        );
        alert.add_response("cancel", "Cancel");
        alert.add_response("move", "Move");
        alert.set_response_appearance("move", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("move"));
        alert.set_close_response("cancel");
        alert.set_extra_child(Some(&dropdown));

        let window_weak = self.downgrade();
        let feed_url = feed.url.clone();
        let dropdown_for_response = dropdown.clone();
        let folders_for_response = folders.clone();
        alert.connect_response(None, move |dialog, response| {
            if response != "move" {
                return;
            }
            dialog.close();
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

        alert.present(Some(self));
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

        let alert = adw::AlertDialog::new(Some(&format!("Settings for “{display_name}”")), None);
        alert.add_response("cancel", "Cancel");
        alert.add_response("save", "Save");
        alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("save"));
        alert.set_close_response("cancel");

        let group = adw::PreferencesGroup::new();
        let notif_row = adw::SwitchRow::builder()
            .title("New article notifications")
            .subtitle("Show a desktop notification when this feed has new articles.")
            .build();
        let reader_row = adw::SwitchRow::builder()
            .title("Always use Reader View")
            .subtitle("Open every article from this feed in extracted-text mode.")
            .build();
        group.add(&notif_row);
        group.add(&reader_row);
        alert.set_extra_child(Some(&group));

        // Pre-load current values so the switches reflect the existing
        // state before the user touches them.
        let account = self.account();
        let feed_id_for_load = feed.id.clone();
        let notif_for_load = notif_row.clone();
        let reader_for_load = reader_row.clone();
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
        let notif_for_response = notif_row.clone();
        let reader_for_response = reader_row.clone();
        alert.connect_response(None, move |dialog, response| {
            if response != "save" {
                return;
            }
            dialog.close();
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
                });
                s.new_article_notifications_enabled = notif_on;
                s.reader_view_always_enabled = reader_on;
                if let Err(e) = account.upsert_feed_settings(s).await {
                    tracing::warn!(?e, %display_name, "feed settings upsert failed");
                }
            });
        });

        alert.present(Some(self));
    }

    pub(crate) fn act_mark_clicked_feed_read(&self) {
        let Some(feed) = self.imp().sidebar_view.get().take_right_clicked_feed() else {
            return;
        };
        let account = self.account();
        let window_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let articles = match account.fetch_articles_by_feed(feed.id.clone()).await {
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
            let mut statuses: Vec<crate::models::ArticleStatus> = Vec::new();
            for feed in &folder.feeds {
                match account.fetch_articles_by_feed(feed.id.clone()).await {
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
        let selection = self.imp().timeline_view.get().selection();
        let Some(item) = selection.selected_item() else {
            return;
        };
        let Some(node) = item.downcast_ref::<ArticleNode>() else {
            return;
        };
        let Some(article) = node.article() else {
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

    /// Drive `AccountRefresher::refresh_feeds` for every feed in the
    /// current OPML. Refresher needs a tokio runtime context, so we dispatch
    /// on the global runtime (not the GLib main loop). Tallies `new_articles`
    /// across the whole cycle and routes the count back to the GTK thread for
    /// an optional desktop notification (see `dispatch_refresh_notification`).
    pub(crate) fn act_refresh(&self) {
        let account = self.account();
        let window_weak = self.downgrade();
        let retention_days = current_retention_days();
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
            // Manual refresh = force=true. Bypasses the 29-min throttle so
            // an explicit user click always hits the network.
            let tally = run_refresh_with_tally(account, paired, retention_days, true).await;
            let _ = done_tx.send(tally);
        });
        self.imp().batch_update.start();
        self.set_refresh_in_progress(true);
        glib::spawn_future_local(async move {
            let Ok(tally) = done_rx.await else {
                if let Some(window) = window_weak.upgrade() {
                    window.imp().batch_update.end();
                    window.set_refresh_in_progress(false);
                }
                return;
            };
            if let Some(window) = window_weak.upgrade() {
                window.dispatch_refresh_notification(&tally);
                window.show_refresh_toast(&tally);
                window.refresh_unread_counts();
                // Re-fetch the timeline for the currently-selected sidebar
                // item so newly-fetched articles appear without the user
                // having to click around. Without this, the timeline shows
                // stale (often empty) results until the next sidebar click.
                window.reload_current_timeline();
                window.imp().batch_update.end();
                window.set_refresh_in_progress(false);
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
        self.imp().toast_overlay.add_toast(adw::Toast::new(&msg));
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

    pub(crate) fn act_focus_search(&self) {
        let imp = self.imp();
        imp.sidebar_view.get().search_btn().set_active(true);
        imp.timeline_view.get().focus_search_entry();
    }

    /// Toggles the outer split view between uncollapsed (both panes visible)
    /// and collapsed (only the content pane, content-mode-shown). Not a true
    /// "hide sidebar" on wide screens because AdwNavigationSplitView doesn't
    /// expose that — `AdwOverlaySplitView` would be the upgrade path if
    /// full-hide is required.
    pub(crate) fn act_toggle_sidebar(&self) {
        let sv = self.imp().outer_split_view.get();
        sv.set_collapsed(!sv.is_collapsed());
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
        let about = adw::AboutDialog::builder()
            .application_name("viaduct")
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("Brandon LaRocque")
            .issue_url("https://github.com/virinvictus/viaduct/issues")
            .website("https://github.com/virinvictus/viaduct")
            .license_type(gtk::License::MitX11)
            .build();
        about.present(Some(self));
    }

    pub(crate) fn act_debug_crash(&self) {
        panic!("Intentional crash triggered from Debug menu");
    }

    fn show_toast(&self, message: &str) {
        let toast = adw::Toast::new(message);
        self.imp().toast_overlay.add_toast(toast);
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
            let result: crate::error::Result<Vec<_>> = match item {
                SidebarItem::Feed(feed) => account.fetch_articles_by_feed(feed.id).await,
                SidebarItem::SmartFeed(name) => match name.as_str() {
                    "Today" => account.fetch_today_articles().await,
                    "All Unread" => account.fetch_unread_articles().await,
                    "Starred" => account.fetch_starred_articles().await,
                    _ => Ok(Vec::new()),
                },
                SidebarItem::Folder(folder) => fetch_folder_articles(&account, &folder).await,
                SidebarItem::SmartFeedGroup => Ok(Vec::new()),
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
    fn install_timeline_capture_shortcuts(&self) {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);

        const NAV_BINDINGS: &[(&str, &str)] = &[
            ("Down", "win.next-unread"),
            ("j", "win.next-unread"),
            ("n", "win.next-unread"),
            ("Up", "win.prev-unread"),
            ("k", "win.prev-unread"),
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

        self.imp()
            .timeline_view
            .get()
            .list_view()
            .add_controller(controller);
    }

    fn refresh_specific_feeds(&self, feeds: Vec<crate::models::Feed>) {
        let account = self.account();
        let window_weak = self.downgrade();
        let retention_days = current_retention_days();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<RefreshTally>();
        self.set_refresh_in_progress(true);
        crate::spawn_on_runtime(async move {
            let paired = pair_feeds_with_settings(&account, feeds).await;
            // Force=true: post-import re-fetch is also an explicit user
            // intent, not auto-refresh.
            let tally = run_refresh_with_tally(account, paired, retention_days, true).await;
            let _ = done_tx.send(tally);
        });
        glib::spawn_future_local(async move {
            let Ok(tally) = done_rx.await else {
                if let Some(window) = window_weak.upgrade() {
                    window.set_refresh_in_progress(false);
                }
                return;
            };
            if let Some(window) = window_weak.upgrade() {
                window.dispatch_refresh_notification(&tally);
                window.refresh_unread_counts();
                window.reload_current_timeline();
                window.set_refresh_in_progress(false);
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
) -> RefreshTally {
    let feeds_attempted = paired.len();
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
    let refresher = crate::network::AccountRefresher::new(account, changes_tx, retention_days);
    if force {
        refresher.refresh_feeds_forced(paired).await;
    } else {
        refresher.refresh_feeds(paired).await;
    }
    drop(refresher);
    let per_feed_new = drain.await.unwrap_or_default();
    RefreshTally {
        feeds_attempted,
        per_feed_new,
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
) -> crate::error::Result<Vec<crate::models::Article>> {
    if folder.feeds.is_empty() {
        return Ok(Vec::new());
    }
    // v1.9.0: was previously a sequential N-round-trip fan-out despite
    // the doc-comment claim of parallelism. Now one bulk DB op via the
    // FetchByFeeds variant (IN clause). For a folder with 50 feeds
    // this drops folder-selection latency from O(N · channel + plan)
    // to O(1 · channel + plan).
    let feed_ids: Vec<String> = folder.feeds.iter().map(|f| f.id.clone()).collect();
    let mut merged = account.fetch_articles_by_feeds(feed_ids).await?;
    // Sort newest-first. Articles without a published date sink to the
    // bottom (matches NNW's ordering for missing dates). The DB layer
    // already sorted within each chunk; this final pass is the
    // cross-chunk merge.
    merged.sort_by_key(|a| std::cmp::Reverse(a.date_published));
    Ok(merged)
}
