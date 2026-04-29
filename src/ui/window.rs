// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::sync::Arc;

use crate::database::accounts::Account;
use crate::network::ImageCache;
use crate::paths::{favicon_cache_dir, image_cache_dir};
use crate::ui::article_renderer;
use crate::ui::sidebar::{
    SidebarDataSource, SidebarItem, SidebarTreeControllerDelegate, selected_sidebar_item,
    setup_sidebar_list_view,
};
use crate::ui::timeline::{ArticleNode, FeedNameMap, setup_timeline_list_view};
use crate::ui::tree::TreeController;

mod imp {
    use super::*;
    use std::cell::OnceCell;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "window.ui")]
    pub struct ViaductWindow {
        #[template_child]
        pub outer_split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub inner_split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub sidebar_list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub timeline_list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub article_web_view: TemplateChild<webkit6::WebView>,
        #[template_child]
        pub url_overlay: TemplateChild<gtk::Label>,
        #[template_child]
        pub article_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub timeline_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub search_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub scope_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub reader_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub mark_all_read_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub primary_menu: TemplateChild<gio::Menu>,
        #[template_child]
        pub sync_btn_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub sync_btn_spinner: TemplateChild<gtk::Spinner>,

        pub account: OnceCell<Arc<Account>>,
        pub image_cache: OnceCell<Arc<ImageCache>>,
        pub timeline_store: OnceCell<gio::ListStore>,
        pub timeline_selection: OnceCell<gtk::SingleSelection>,
        pub sidebar_delegate: OnceCell<Rc<RefCell<SidebarTreeControllerDelegate>>>,
        pub sidebar_data_source: OnceCell<Rc<SidebarDataSource>>,
        pub sidebar_tree_controller: OnceCell<Rc<TreeController>>,
        /// Map from `feed_id` → display name. Built from OPML at load time
        /// and rebuilt on every import; the timeline factory reads through
        /// it on each bind so rows show "Daring Fireball" instead of the URL.
        pub feed_names: OnceCell<crate::ui::timeline::FeedNameMap>,
        /// Pending debounced search timeout, restarted on every keystroke.
        pub search_timeout: RefCell<Option<glib::SourceId>>,
        /// Feed ID of the currently-selected sidebar row. Used by the search
        /// scope toggle to restrict FTS5 queries to a single feed.
        pub selected_feed_id: RefCell<Option<String>>,
        /// Article-pane display state. Centralizes the four inputs to
        /// `render_article_body` so toggle flips and async extractor
        /// completions don't need to re-derive everything.
        pub article_display: RefCell<ArticleDisplayState>,
        pub batch_update: crate::ui::batch::BatchUpdate,
    }

    /// Captured state for whatever article is currently on-screen.
    /// `raw_html` is the feed-provided body, `extracted_html` caches a
    /// Reader-View extraction result. `auto_reader` is the feed's
    /// `reader_view_always_enabled` setting; when true the reader button
    /// is pre-toggled on article selection. Metadata fields feed the NNW
    /// theme macros and are populated by the timeline-selection handler.
    #[derive(Default)]
    pub struct ArticleDisplayState {
        pub raw_html: Option<String>,
        pub extracted_html: Option<String>,
        pub article_url: Option<String>,
        pub auto_reader: bool,
        pub title: String,
        pub byline: String,
        pub feed_link: String,
        pub feed_link_title: String,
        pub date_published: Option<chrono::DateTime<chrono::Utc>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ViaductWindow {
        const NAME: &'static str = "ViaductWindow";
        type Type = super::ViaductWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            // The window.ui template references `WebKitWebView` by class
            // name. The GType must be registered before the GTK builder
            // resolves the template, otherwise template loading fails.
            webkit6::WebView::ensure_type();
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
        window
            .imp()
            .image_cache
            .set(Arc::new(ImageCache::new(favicons, images)))
            .ok();
        window.wire_models();
        crate::ui::actions::install(&window, app);

        if crate::is_debug_mode() {
            let debug_section = gio::Menu::new();
            debug_section.append(Some("Crash (Panic)"), Some("win.debug-crash"));
            window
                .imp()
                .primary_menu
                .append_submenu(Some("Debug"), &debug_section);
        }

        window
    }

    fn account(&self) -> Arc<Account> {
        self.imp()
            .account
            .get()
            .cloned()
            .expect("ViaductWindow constructed without Account")
    }

    fn image_cache(&self) -> Arc<ImageCache> {
        self.imp()
            .image_cache
            .get()
            .cloned()
            .expect("ViaductWindow constructed without ImageCache")
    }

    fn wire_models(&self) {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;

        let imp = self.imp();

        // Lock down the article-pane WebView before any HTML can be loaded.
        // Idempotent; settings stay applied for the window's lifetime.
        article_renderer::apply_locked_down_settings(&imp.article_web_view.get());
        // Link clicks must route to the system browser instead of
        // navigating the WebView away from our rendered article.
        article_renderer::install_link_interceptor(&imp.article_web_view.get());
        // Register the viaduct-img:// URI scheme on the default WebContext
        // so the article pane's CSP-locked img-src can route through our
        // ImageCache. Process-wide; idempotent.
        article_renderer::install_image_uri_scheme(self.image_cache());
        // Register viaduct-font:// so themes that reference bundled
        // fonts (e.g. Hyperlegible → Atkinson Hyperlegible) resolve
        // even when the system doesn't have those fonts installed.
        article_renderer::install_font_uri_scheme();
        // Show the link URL in the article pane's bottom-left when the
        // user hovers a link — preview where Enter / click will go.
        article_renderer::install_hover_url_overlay(
            &imp.article_web_view.get(),
            &imp.url_overlay.get(),
        );

        // Re-render the article pane when:
        //   * the user changes the article-theme GSetting, or
        //   * the libadwaita color scheme flips (so "auto" mode swaps
        //     Sepia ↔ Tiqoe Dark live).
        // No-op when no article is selected (`render_article_body`
        // clears the pane).
        let win_for_theme = self.downgrade();
        if let Some(settings) = crate::preferences::settings() {
            settings.connect_changed(
                Some(crate::preferences::keys::ARTICLE_THEME),
                move |_, _| {
                    if let Some(win) = win_for_theme.upgrade() {
                        win.render_article_body();
                    }
                },
            );
        }
        let win_for_dark = self.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(win) = win_for_dark.upgrade() {
                win.render_article_body();
            }
        });

        // Sidebar: delegate → controller → data source → list view.
        let delegate = Rc::new(RefCell::new(SidebarTreeControllerDelegate::new()));
        let controller = Rc::new(TreeController::new_with_generic_root(
            Rc::downgrade(&delegate) as _,
        ));
        let data_source = Rc::new(SidebarDataSource::new());
        data_source.set_tree_controller(controller.clone());

        let sidebar_selection = setup_sidebar_list_view(
            &imp.sidebar_list_view,
            &data_source,
            self.account(),
            self.image_cache(),
        );

        // Feed-name resolver passed to the timeline factory. Empty until OPML
        // loads; the bind closure falls back to feed_id (URL) until then.
        let feed_names: FeedNameMap = Rc::new(RefCell::new(HashMap::new()));
        imp.feed_names.set(feed_names.clone()).ok();

        // Timeline store + selection.
        let timeline_store = gio::ListStore::new::<ArticleNode>();
        let timeline_selection =
            setup_timeline_list_view(&imp.timeline_list_view, &timeline_store, feed_names.clone());

        self.install_timeline_capture_shortcuts();

        // Empty-state plumbing — keep the timeline stack page in sync
        // with whether the store has any rows. Listens on items_changed
        // so every populate path (sidebar selection, search, refresh)
        // updates the visible page automatically.
        let win_for_timeline_empty = self.downgrade();
        timeline_store.connect_items_changed(move |store, _pos, _removed, _added| {
            if let Some(win) = win_for_timeline_empty.upgrade() {
                let name = if store.n_items() == 0 {
                    "empty"
                } else {
                    "content"
                };
                win.imp().timeline_stack.set_visible_child_name(name);
            }
        });
        // Initial state — empty until the first populate.
        imp.timeline_stack.set_visible_child_name("empty");
        // Article pane likewise starts in the empty state.
        imp.article_stack.set_visible_child_name("empty");

        // Persist references so they outlive `wire_models` and the GC.
        imp.sidebar_delegate.set(delegate.clone()).ok();
        imp.sidebar_tree_controller.set(controller.clone()).ok();
        imp.sidebar_data_source.set(data_source.clone()).ok();
        imp.timeline_store.set(timeline_store.clone()).ok();
        imp.timeline_selection.set(timeline_selection.clone()).ok();

        // Initial OPML load — populate the sidebar. `Account::load_opml`
        // calls `tokio::fs`, which requires a tokio runtime context — and
        // `glib::spawn_future_local` runs on the GLib main loop, NOT on tokio.
        // Hop through `spawn_on_runtime` for the read, deliver the parsed
        // OpmlFile back through a oneshot, and apply it on the GTK thread.
        let account = self.account();
        let delegate_for_load = delegate.clone();
        let controller_for_load = controller.clone();
        let data_source_for_load = data_source.clone();
        let window_weak_for_load = self.downgrade();
        let (load_tx, load_rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = load_tx.send(account.load_opml().await);
        });
        glib::spawn_future_local(async move {
            match load_rx.await {
                Ok(Ok(opml)) => {
                    if let Some(window) = window_weak_for_load.upgrade() {
                        window.rebuild_feed_names_from(&opml);
                    }
                    delegate_for_load.borrow().set_opml_file(opml);
                    controller_for_load.rebuild();
                    data_source_for_load.refresh_root();
                    if let Some(window) = window_weak_for_load.upgrade() {
                        window.refresh_unread_counts();
                    }
                }
                Ok(Err(e)) => tracing::warn!(?e, "failed to load OPML at startup"),
                Err(_) => tracing::warn!("OPML load task aborted"),
            }
        });

        // Sidebar selection → timeline fetch.
        let account_for_sidebar = self.account();
        let timeline_store_for_sidebar = timeline_store.clone();
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
                *window.imp().selected_feed_id.borrow_mut() = feed_id;
            }
            let account = account_for_sidebar.clone();
            let store = timeline_store_for_sidebar.clone();
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
                        populate_timeline(&store, articles);
                        refresh_timeline_statuses(account.clone(), store.clone());
                    }
                    Err(e) => tracing::warn!(?e, "failed to fetch articles for sidebar selection"),
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
                .feed_names
                .get()
                .and_then(|names| names.borrow().get(&feed_id).cloned())
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

            // Seed the display state for the new article. `auto_reader` is
            // loaded async from FeedSettings below; until it resolves we
            // render the raw body.
            {
                let mut state = window.imp().article_display.borrow_mut();
                state.raw_html = Some(body);
                state.extracted_html = None;
                state.article_url = external;
                state.auto_reader = false;
                state.title = title;
                state.byline = byline;
                state.feed_link = feed_link;
                state.feed_link_title = feed_link_title;
                state.date_published = article.date_published;
            }
            // Untoggle the reader button without re-firing its handler (we
            // want it to reflect `auto_reader` after the settings fetch).
            window.imp().reader_btn.set_active(false);
            window.render_article_body();

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

            // Async-resolve the feed's readerViewAlwaysEnabled preference.
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
                    window.imp().article_display.borrow_mut().auto_reader = auto;
                    if auto {
                        window.imp().reader_btn.set_active(true);
                    }
                }
            });
        });

        // Reader-button toggle → re-render with extracted or raw body.
        let window_weak_for_reader = self.downgrade();
        imp.reader_btn.connect_toggled(move |_| {
            if let Some(window) = window_weak_for_reader.upgrade() {
                window.render_article_body();
            }
        });

        // Mark-all-read button — fires the same action as Ctrl+K so click
        // and keyboard share a code path.
        let window_weak_for_mark = self.downgrade();
        imp.mark_all_read_btn.connect_clicked(move |_| {
            if let Some(window) = window_weak_for_mark.upgrade() {
                window.act_mark_all_read();
            }
        });

        self.wire_search(timeline_store);
    }

    /// Re-render the article pane based on the current display state +
    /// reader button. Handles kicking off a Reader-View extraction on
    /// demand when the button goes active and no extracted HTML is cached.
    pub(crate) fn render_article_body(&self) {
        let imp = self.imp();
        let view = imp.article_web_view.get();

        // Pull display state and metadata under one borrow.
        let state = imp.article_display.borrow();
        let reader_mode = imp.reader_btn.is_active();
        let raw = state.raw_html.clone();
        let extracted = state.extracted_html.clone();
        let url = state.article_url.clone();

        // Pick which body the active reader-button mode wants. Falls back
        // to raw HTML when extraction hasn't completed; the async kick-off
        // sits below and re-enters this function on completion.
        let body_html = if reader_mode {
            extracted.clone().or_else(|| raw.clone())
        } else {
            raw.clone()
        };
        let Some(body_html) = body_html else {
            drop(state);
            // Nothing to render — flip the stack to the empty status page.
            // Don't bother re-rendering the WebView since it's hidden.
            imp.article_stack.set_visible_child_name("empty");
            return;
        };
        // Article body present — make sure the stack is showing the WebView.
        imp.article_stack.set_visible_child_name("content");

        // Pick a theme: user's GSettings choice wins, "auto" pairs Sepia
        // (light) with Tiqoe Dark (dark). v1.2.0 wired the article-theme
        // GSetting + preferences dropdown — see preferences.rs.
        let is_dark = adw::StyleManager::default().is_dark();
        let theme = match crate::preferences::settings() {
            Some(s) => crate::preferences::resolve_article_theme(&s, is_dark),
            None => article_renderer::select_for_dark_mode(is_dark),
        };

        let subs = article_renderer::ArticleSubstitutions {
            title: article_renderer::escape_html(&state.title),
            body: body_html,
            preferred_link: state.article_url.clone().unwrap_or_default(),
            feed_link: state.feed_link.clone(),
            feed_link_title: article_renderer::escape_html(&state.feed_link_title),
            byline: article_renderer::escape_html(&state.byline),
            datetime_long: state
                .date_published
                .map(|d| d.format("%A, %B %e, %Y at %l:%M:%S %p").to_string())
                .unwrap_or_default(),
            datetime_medium: state
                .date_published
                .map(|d| d.format("%b %e, %Y at %l:%M %p").to_string())
                .unwrap_or_default(),
            datetime_short: state
                .date_published
                .map(|d| d.format("%-m/%-d/%y, %l:%M %p").to_string())
                .unwrap_or_default(),
            date_long: state
                .date_published
                .map(|d| d.format("%A, %B %e, %Y").to_string())
                .unwrap_or_default(),
            date_medium: state
                .date_published
                .map(|d| d.format("%b %e, %Y").to_string())
                .unwrap_or_default(),
            date_short: state
                .date_published
                .map(|d| d.format("%-m/%-d/%y").to_string())
                .unwrap_or_default(),
            time_long: state
                .date_published
                .map(|d| d.format("%l:%M:%S %p").to_string())
                .unwrap_or_default(),
            time_medium: state
                .date_published
                .map(|d| d.format("%l:%M:%S %p").to_string())
                .unwrap_or_default(),
            time_short: state
                .date_published
                .map(|d| d.format("%l:%M %p").to_string())
                .unwrap_or_default(),
            avatar_src: String::new(),
            external_link: String::new(),
            external_link_label: String::new(),
            external_link_stripped: String::new(),
        };
        drop(state);

        article_renderer::render_themed(&view, theme, subs, url.as_deref());

        // Reader-mode: if the user toggled in but extracted_html is still
        // None, kick off the extractor. The fallback render above already
        // showed raw body so the pane isn't blank during the wait.
        if reader_mode && extracted.is_none() {
            let Some(url) = url else { return };
            let window_weak = self.downgrade();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let result = crate::ui::reader_view::extract(&url, raw.as_deref()).await;
                let _ = tx.send(result);
            });
            glib::spawn_future_local(async move {
                match rx.await {
                    Ok(Ok(extracted)) => {
                        if let Some(window) = window_weak.upgrade() {
                            window.imp().article_display.borrow_mut().extracted_html =
                                Some(extracted);
                            // Only re-render if the user hasn't since toggled
                            // off or advanced to a different article.
                            if window.imp().reader_btn.is_active() {
                                window.render_article_body();
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "reader view extraction failed");
                        if let Some(window) = window_weak.upgrade() {
                            window.imp().reader_btn.set_active(false);
                        }
                    }
                    Err(_) => {
                        // Oneshot sender dropped — extraction task panicked.
                        tracing::warn!("reader view extraction task aborted");
                    }
                }
            });
        }
    }

    fn wire_search(&self, timeline_store: gio::ListStore) {
        use std::time::Duration;

        let imp = self.imp();

        // Bind the sidebar toggle to the search bar's reveal state.
        imp.search_btn
            .bind_property("active", &*imp.search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        // GtkSearchBar must be told which entry it owns so it can route
        // text input from `key-capture-widget` properly. Without this we
        // get the "search bar does not have an entry connected" warning on
        // every keystroke that hits the timeline list view.
        imp.search_bar.connect_entry(&*imp.search_entry);

        // Scope toggle: re-run the current search whenever it flips so the
        // user doesn't have to re-type.
        let window_weak_for_scope = self.downgrade();
        imp.scope_toggle.connect_toggled(move |_| {
            if let Some(window) = window_weak_for_scope.upgrade() {
                // Re-trigger the debounced handler so scope changes apply
                // without the user having to touch the entry again.
                window
                    .imp()
                    .search_entry
                    .get()
                    .emit_by_name::<()>("search-changed", &[]);
            }
        });

        // Debounce keystrokes ~150ms before running FTS5 (NNW spec). Without
        // this every character hits SQLite — fine on small DBs, painful at 50k.
        let account = self.account();
        let window_weak = self.downgrade();
        imp.search_entry.connect_search_changed(move |entry| {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            // Cancel any in-flight timeout.
            if let Some(prev) = window.imp().search_timeout.borrow_mut().take() {
                prev.remove();
            }
            let query = entry.text().to_string();
            let store = timeline_store.clone();
            let account = account.clone();
            let entry_for_clear = entry.clone();
            let window_weak_inner = window.downgrade();
            let new_timeout = glib::timeout_add_local_once(Duration::from_millis(150), move || {
                if query.trim().is_empty() {
                    // Clearing the search reverts the timeline to whatever
                    // the sidebar selection currently shows; for port-first
                    // we just empty it. The user can re-click the sidebar.
                    store.remove_all();
                    return;
                }
                // Wrap as a prefix MATCH so `rust*` matches `rustacean`, etc.
                let fts_query = format!("{}*", escape_fts5(&query));
                let store = store.clone();
                let _entry_keepalive = entry_for_clear.clone();
                let account = account.clone();

                // Resolve the scope right when the query fires — not at
                // debounce start — so toggling after typing works without a
                // second keystroke.
                let feed_filter = window_weak_inner.upgrade().and_then(|w| {
                    let imp = w.imp();
                    if imp.scope_toggle.is_active() {
                        imp.selected_feed_id.borrow().clone()
                    } else {
                        None
                    }
                });

                glib::spawn_future_local(async move {
                    match account
                        .search_articles_with_snippets(fts_query, feed_filter)
                        .await
                    {
                        Ok(results) => {
                            populate_timeline_with_snippets(&store, results);
                            refresh_timeline_statuses(account.clone(), store.clone());
                        }
                        Err(e) => tracing::warn!(?e, "FTS5 search failed"),
                    }
                    drop(_entry_keepalive);
                });
            });
            *window.imp().search_timeout.borrow_mut() = Some(new_timeout);
        });
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
        let Some(selection) = imp.timeline_selection.get() else {
            self.advance_unread(Direction::Next);
            return;
        };
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
        let Some(store) = imp.timeline_store.get() else {
            return;
        };
        let Some(selection) = imp.timeline_selection.get() else {
            return;
        };
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
                self.imp().timeline_list_view.scroll_to(
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
        let Some(selection) = imp.timeline_selection.get() else {
            return;
        };
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
        let Some(selection) = self.imp().timeline_selection.get() else {
            return;
        };
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
        let Some(store) = imp.timeline_store.get() else {
            return;
        };
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
        let Some(selection) = self.imp().timeline_selection.get() else {
            return;
        };
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

    pub(crate) fn act_open_enclosure(&self) {
        let Some(selection) = self.imp().timeline_selection.get() else {
            return;
        };
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
                window.dispatch_refresh_notification(tally.new_articles);
                window.show_refresh_toast(tally);
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
    /// pair with `set_refresh_in_progress(false)` at completion.
    pub(crate) fn set_refresh_in_progress(&self, on: bool) {
        let imp = self.imp();
        if on {
            imp.sync_btn_spinner.start();
            imp.sync_btn_stack.set_visible_child_name("spinner");
        } else {
            imp.sync_btn_spinner.stop();
            imp.sync_btn_stack.set_visible_child_name("icon");
        }
    }

    fn show_refresh_toast(&self, tally: RefreshTally) {
        let msg = if tally.feeds_attempted == 0 {
            "No feeds in subscription list.".to_string()
        } else if tally.new_articles == 0 {
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
                tally.new_articles,
                if tally.new_articles == 1 { "" } else { "s" },
            )
        };
        self.imp().toast_overlay.add_toast(adw::Toast::new(&msg));
    }

    /// Show a desktop notification summarizing a refresh cycle, gated by the
    /// `notifications-on-refresh` GSetting. Silent when total == 0 or the
    /// pref is off.
    fn dispatch_refresh_notification(&self, new_total: usize) {
        if new_total == 0 {
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
        let body = if new_total == 1 {
            "1 new article".to_string()
        } else {
            format!("{new_total} new articles")
        };
        let notif = gio::Notification::new("viaduct");
        notif.set_body(Some(&body));
        notif.set_priority(gio::NotificationPriority::Normal);
        app.send_notification(Some("viaduct.refresh"), &notif);
    }

    pub(crate) fn act_focus_search(&self) {
        let imp = self.imp();
        imp.search_btn.set_active(true);
        imp.search_entry.get().grab_focus();
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
    fn reload_sidebar_after_opml_change(&self) {
        let imp = self.imp();
        let Some(delegate) = imp.sidebar_delegate.get().cloned() else {
            return;
        };
        let Some(controller) = imp.sidebar_tree_controller.get().cloned() else {
            return;
        };
        let Some(data_source) = imp.sidebar_data_source.get().cloned() else {
            return;
        };
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
                        window.rebuild_feed_names_from(&opml);
                    }
                    delegate.borrow().set_opml_file(opml);
                    controller.rebuild();
                    data_source.refresh_root();
                    if let Some(window) = window_weak.upgrade() {
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
    /// No-op when nothing is selected.
    pub(crate) fn reload_current_timeline(&self) {
        let imp = self.imp();
        let Some(model) = imp.sidebar_list_view.model() else {
            return;
        };
        let Some(sel) = model.downcast_ref::<gtk::SingleSelection>() else {
            return;
        };
        let Some(item) = selected_sidebar_item(sel) else {
            return;
        };
        let Some(store) = imp.timeline_store.get().cloned() else {
            return;
        };
        let account = self.account();
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
                    populate_timeline(&store, articles);
                    refresh_timeline_statuses(account.clone(), store.clone());
                }
                Err(e) => tracing::warn!(?e, "reload_current_timeline failed"),
            }
        });
    }

    pub(crate) fn refresh_unread_counts(&self) {
        let Some(controller) = self.imp().sidebar_tree_controller.get().cloned() else {
            return;
        };
        let account = self.account();
        glib::spawn_future_local(async move {
            let per_feed = match account.unread_counts_by_feed().await {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(?e, "unread_counts_by_feed failed");
                    return;
                }
            };
            let smart = account.smart_feed_counts().await.ok();

            let to_u32 = |n: i64| n.max(0).min(u32::MAX as i64) as u32;
            let count_for_feed = |id: &str| to_u32(per_feed.get(id).copied().unwrap_or(0));

            for top in controller.root_node.child_nodes() {
                let Some(rep) = top.represented_object() else {
                    continue;
                };
                let Some(item) = rep.downcast_ref::<crate::ui::sidebar::SidebarItem>() else {
                    continue;
                };
                match item {
                    crate::ui::sidebar::SidebarItem::SmartFeedGroup => {
                        let mut group_total: u32 = 0;
                        for child in top.child_nodes() {
                            let Some(c_rep) = child.represented_object() else {
                                continue;
                            };
                            let Some(c_item) =
                                c_rep.downcast_ref::<crate::ui::sidebar::SidebarItem>()
                            else {
                                continue;
                            };
                            if let crate::ui::sidebar::SidebarItem::SmartFeed(name) = c_item {
                                let count = match (name.as_str(), smart) {
                                    ("Today", Some(s)) => to_u32(s.today_unread),
                                    ("All Unread", Some(s)) => to_u32(s.all_unread),
                                    ("Starred", Some(s)) => to_u32(s.starred_unread),
                                    _ => 0,
                                };
                                child.set_unread_count(count);
                                group_total = group_total.saturating_add(count);
                            }
                        }
                        top.set_unread_count(group_total);
                    }
                    crate::ui::sidebar::SidebarItem::Folder(_) => {
                        let mut folder_total: u32 = 0;
                        for child in top.child_nodes() {
                            let Some(c_rep) = child.represented_object() else {
                                continue;
                            };
                            let Some(c_item) =
                                c_rep.downcast_ref::<crate::ui::sidebar::SidebarItem>()
                            else {
                                continue;
                            };
                            if let crate::ui::sidebar::SidebarItem::Feed(feed) = c_item {
                                let count = count_for_feed(&feed.id);
                                child.set_unread_count(count);
                                folder_total = folder_total.saturating_add(count);
                            }
                        }
                        top.set_unread_count(folder_total);
                    }
                    crate::ui::sidebar::SidebarItem::Feed(feed) => {
                        top.set_unread_count(count_for_feed(&feed.id));
                    }
                    crate::ui::sidebar::SidebarItem::SmartFeed(_) => {}
                }
            }
        });
    }

    /// Walk an `OpmlFile` and (re)populate the feed-name resolver. Same name
    /// preference order as the sidebar: `edited_name` → `name` → URL host →
    /// raw URL. After repopulating, kick `items_changed` on the timeline
    /// store so already-bound rows pick up the new names.
    fn rebuild_feed_names_from(&self, opml: &crate::database::opml::OpmlFile) {
        let Some(map_rc) = self.imp().feed_names.get() else {
            return;
        };
        {
            let mut map = map_rc.borrow_mut();
            map.clear();
            for feed in &opml.standalone_feeds {
                map.insert(feed.id.clone(), display_name_for_feed(feed));
            }
            for folder in &opml.folders {
                for feed in &folder.feeds {
                    map.insert(feed.id.clone(), display_name_for_feed(feed));
                }
            }
        }
        if let Some(store) = self.imp().timeline_store.get() {
            let n = store.n_items();
            if n > 0 {
                store.items_changed(0, n, n);
            }
        }
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

        self.imp().timeline_list_view.add_controller(controller);
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
                window.dispatch_refresh_notification(tally.new_articles);
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
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RefreshTally {
    pub feeds_attempted: usize,
    pub new_articles: usize,
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
        let mut total: usize = 0;
        while let Some(changes) = changes_rx.recv().await {
            total = total.saturating_add(changes.new_articles.len());
        }
        total
    });
    let refresher = crate::network::AccountRefresher::new(account, changes_tx, retention_days);
    if force {
        refresher.refresh_feeds_forced(paired).await;
    } else {
        refresher.refresh_feeds(paired).await;
    }
    drop(refresher);
    let new_articles = drain.await.unwrap_or(0);
    RefreshTally {
        feeds_attempted,
        new_articles,
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

fn populate_timeline_with_snippets(
    store: &gio::ListStore,
    results: Vec<(crate::models::Article, String)>,
) {
    store.remove_all();
    for (article, snippet) in results {
        store.append(&ArticleNode::with_snippet(article, snippet));
    }
}

/// Escape FTS5 special characters so user input is treated as a literal token.
/// FTS5 reserves `"` for phrase quoting and treats unbalanced quotes as a
/// syntax error; wrapping the term in double quotes after escaping is the
/// minimum safe transform.
fn escape_fts5(term: &str) -> String {
    let escaped = term.replace('"', "\"\"");
    format!("\"{}\"", escaped)
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

fn populate_timeline(store: &gio::ListStore, articles: Vec<crate::models::Article>) {
    store.remove_all();
    for article in articles {
        store.append(&ArticleNode::new(article));
    }
}

/// Bulk-fetch statuses for every `ArticleNode` currently in the timeline
/// store and copy them onto the nodes. Runs after every timeline repopulate.
fn refresh_timeline_statuses(account: Arc<Account>, store: gio::ListStore) {
    let mut ids: Vec<String> = Vec::with_capacity(store.n_items() as usize);
    let mut nodes: Vec<ArticleNode> = Vec::with_capacity(ids.capacity());
    for i in 0..store.n_items() {
        let Some(obj) = store.item(i) else { continue };
        let Some(node) = obj.downcast_ref::<ArticleNode>() else {
            continue;
        };
        let Some(article) = node.article() else {
            continue;
        };
        ids.push(article.article_id);
        nodes.push(node.clone());
    }
    if ids.is_empty() {
        return;
    }
    glib::spawn_future_local(async move {
        match account.fetch_statuses_by_ids(ids).await {
            Ok(map) => {
                for node in nodes {
                    if let Some(article) = node.article() {
                        let (read, starred) = map
                            .get(&article.article_id)
                            .copied()
                            .unwrap_or((false, false));
                        node.set_status(read, starred);
                    }
                }
            }
            Err(e) => tracing::debug!(?e, "bulk status fetch failed"),
        }
    });
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
    let mut merged: Vec<crate::models::Article> = Vec::new();
    for feed in &folder.feeds {
        match account.fetch_articles_by_feed(feed.id.clone()).await {
            Ok(mut articles) => merged.append(&mut articles),
            Err(e) => tracing::warn!(
                feed_id = %feed.id,
                ?e,
                "folder aggregation: feed fetch failed (other feeds will still render)"
            ),
        }
    }
    // Sort newest-first. Articles without a published date sink to the
    // bottom (matches NNW's ordering for missing dates).
    merged.sort_by_key(|a| std::cmp::Reverse(a.date_published));
    Ok(merged)
}
