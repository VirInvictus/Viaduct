// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Phase 18 / v2.0.0-pre2 — `ViaductTimelineView`. Owns the timeline list
//! view, its `gio::ListStore`, the `gtk::SingleSelection`, the search bar
//! with its scope toggle, the empty-state stack page, the FTS5 debounce
//! pipeline, and the `selected_feed_id` cell that scopes "this feed"
//! searches. Lifted out of `ViaductWindow` so the god-object shrinks one
//! pane at a time. The window keeps orchestrating cross-pane state
//! (auto-mark-read on selection, sidebar unread refresh, article-pane
//! render) — the timeline pane just owns *its* widgets and storage.
//!
//! NetNewsWire counterpart: `Mac/MainWindow/Timeline/TimelineViewController.swift`.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use crate::database::accounts::Account;
use crate::network::ImageCache;
use crate::ui::timeline::{ArticleNode, FeedNameMap, setup_timeline_list_view};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "timeline_view.ui")]
    pub struct TimelineView {
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub scope_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub timeline_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub timeline_list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub timeline_empty_status: TemplateChild<adw::StatusPage>,

        pub store: std::cell::OnceCell<gio::ListStore>,
        pub selection: std::cell::OnceCell<gtk::SingleSelection>,
        pub selected_feed_id: RefCell<Option<String>>,
        /// Pending debounced FTS5 timeout, restarted on every keystroke.
        pub search_timeout: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TimelineView {
        const NAME: &'static str = "ViaductTimelineView";
        type Type = super::TimelineView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // Phase 20c: what `adw::Bin` was for. BinLayout gives the same
            // "size to my one child" behaviour with no libadwaita.
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TimelineView {
        // `adw::Bin` unparented its child for us; plain `gtk::Widget`
        // does not, and GTK warns about surviving children at finalize.
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for TimelineView {}
}

glib::wrapper! {
    pub struct TimelineView(ObjectSubclass<imp::TimelineView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TimelineView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TimelineView {
    /// Wire the row factory + selection model, hook the timeline-stack to
    /// auto-flip between content and empty pages, and set up the search
    /// bar (bidirectional bind to the sidebar's `search_btn`, debounced
    /// FTS5 query, scope toggle re-trigger). Run once from
    /// `ViaductWindow::wire_models` after the OPML + feed names are
    /// available. Idempotent in the sense that the OnceCells guard
    /// against double-bootstrap.
    pub fn bootstrap(
        &self,
        account: Arc<Account>,
        image_cache: Arc<ImageCache>,
        feed_names: FeedNameMap,
        search_btn: &gtk::ToggleButton,
    ) {
        let imp = self.imp();

        // Build the store + factory + SingleSelection. Reuses the row
        // factory in `crate::ui::timeline` byte-for-byte.
        let store = gio::ListStore::new::<ArticleNode>();
        let selection =
            setup_timeline_list_view(&imp.timeline_list_view, &store, feed_names, image_cache);
        imp.store.set(store.clone()).ok();
        imp.selection.set(selection.clone()).ok();

        // Stack auto-flip: items_changed → swap content/empty pages.
        // Atomic splices (used by `populate` / `populate_with_snippets`)
        // fire a single items_changed so the stack flips exactly once
        // per repopulate, no flash between empty and full states.
        let weak_for_stack = self.downgrade();
        store.connect_items_changed(move |store, _pos, _removed, _added| {
            let name = if store.n_items() == 0 {
                "empty"
            } else {
                "content"
            };
            if let Some(view) = weak_for_stack.upgrade() {
                view.imp().timeline_stack.set_visible_child_name(name);
            }
        });
        // Initial empty state.
        imp.timeline_stack.set_visible_child_name("empty");

        // ---------------------- Search wiring ----------------------

        // Bidirectional bind: sidebar's search_btn ↔ this pane's search_bar.
        search_btn
            .bind_property("active", &*imp.search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        // GtkSearchBar must be told which entry it owns so it can route
        // text input from `key-capture-widget` properly. Without this we
        // get the "search bar does not have an entry connected" warning on
        // every keystroke that hits the timeline list view.
        imp.search_bar.connect_entry(&*imp.search_entry);

        // Scope toggle: re-fire the current query so a flip applies
        // without the user having to touch the entry again.
        let weak_for_scope = self.downgrade();
        imp.scope_toggle.connect_toggled(move |_| {
            if let Some(view) = weak_for_scope.upgrade() {
                view.imp()
                    .search_entry
                    .get()
                    .emit_by_name::<()>("search-changed", &[]);
            }
        });

        // Debounce keystrokes ~150ms before running FTS5 (NNW spec).
        // Without this every character hits SQLite — fine on small DBs,
        // painful at 50k.
        let weak_for_search = self.downgrade();
        imp.search_entry.connect_search_changed(move |entry| {
            let Some(view) = weak_for_search.upgrade() else {
                return;
            };
            // Cancel any in-flight timeout.
            if let Some(prev) = view.imp().search_timeout.borrow_mut().take() {
                prev.remove();
            }
            let query = entry.text().to_string();
            let account = account.clone();
            let weak_inner = view.downgrade();
            let new_timeout = glib::timeout_add_local_once(Duration::from_millis(150), move || {
                let Some(view) = weak_inner.upgrade() else {
                    return;
                };
                // The _once source auto-removes after firing; drop our stored
                // handle so the next keystroke doesn't .remove() a dead id
                // (a GLib CRITICAL, fatal under G_DEBUG=fatal-criticals).
                *view.imp().search_timeout.borrow_mut() = None;
                if query.trim().is_empty() {
                    // Clearing the search reverts the timeline to
                    // whatever the sidebar selection currently shows;
                    // for port-first we just empty it. The user can
                    // re-click the sidebar.
                    view.clear();
                    return;
                }
                let fts_query = format!("{}*", escape_fts5(&query));
                let feed_filter = if view.imp().scope_toggle.is_active() {
                    view.imp().selected_feed_id.borrow().clone()
                } else {
                    None
                };
                let account = account.clone();
                let weak_for_result = view.downgrade();
                glib::spawn_future_local(async move {
                    match account
                        .search_articles_with_snippets(fts_query, feed_filter)
                        .await
                    {
                        Ok(results) => {
                            if let Some(view) = weak_for_result.upgrade() {
                                view.populate_with_snippets(results);
                                view.refresh_statuses(account.clone());
                            }
                        }
                        Err(e) => tracing::warn!(?e, "FTS5 search failed"),
                    }
                });
            });
            *view.imp().search_timeout.borrow_mut() = Some(new_timeout);
        });
    }

    // -------------------- Public accessors --------------------

    pub fn list_view(&self) -> gtk::ListView {
        self.imp().timeline_list_view.get()
    }

    pub fn store(&self) -> gio::ListStore {
        self.imp()
            .store
            .get()
            .cloned()
            .expect("TimelineView used before bootstrap")
    }

    pub fn selection(&self) -> gtk::SingleSelection {
        self.imp()
            .selection
            .get()
            .cloned()
            .expect("TimelineView used before bootstrap")
    }

    pub fn selected_feed_id(&self) -> Option<String> {
        self.imp().selected_feed_id.borrow().clone()
    }

    pub fn set_selected_feed_id(&self, feed_id: Option<String>) {
        *self.imp().selected_feed_id.borrow_mut() = feed_id;
    }

    /// Read the currently-selected `ArticleNode`, if any.
    pub fn current_article_node(&self) -> Option<ArticleNode> {
        let selection = self.imp().selection.get()?;
        selection
            .selected_item()
            .and_then(|i| i.downcast::<ArticleNode>().ok())
    }

    // -------------------- Populate / clear --------------------

    /// Atomic splice of fresh articles into the timeline. Fires a single
    /// `items_changed` so the stack flips at most once per repopulate.
    pub fn populate(&self, articles: Vec<crate::models::Article>) {
        let store = self.store();
        let n_existing = store.n_items();
        let nodes: Vec<ArticleNode> = articles.into_iter().map(ArticleNode::new).collect();
        self.set_empty_state(false);
        store.splice(0, n_existing, &nodes);
    }

    /// Populate with FTS5 snippet matches. Same atomic-splice pattern;
    /// each row gets its highlighted excerpt via `ArticleNode::with_snippet`.
    pub fn populate_with_snippets(&self, results: Vec<(crate::models::Article, String)>) {
        let store = self.store();
        let n_existing = store.n_items();
        // A search that matched nothing should explain itself, not fall back
        // to the generic "select a feed" empty state.
        self.set_empty_state(results.is_empty());
        let nodes: Vec<ArticleNode> = results
            .into_iter()
            .map(|(article, snippet)| ArticleNode::with_snippet(article, snippet))
            .collect();
        store.splice(0, n_existing, &nodes);
    }

    pub fn clear(&self) {
        self.set_empty_state(false);
        let store = self.store();
        let n = store.n_items();
        if n > 0 {
            store.splice(0, n, &[] as &[ArticleNode]);
        }
    }

    /// Swap the empty-state status page between the default "no feed selected"
    /// copy and the "search returned nothing" copy.
    fn set_empty_state(&self, no_search_results: bool) {
        let status = self.imp().timeline_empty_status.get();
        if no_search_results {
            status.set_icon_name(Some("system-search-symbolic"));
            status.set_title("No results");
            status.set_description(Some("No articles match your search."));
        } else {
            status.set_icon_name(Some("document-open-recent-symbolic"));
            status.set_title("No articles");
            status.set_description(Some(
                "Select a feed in the sidebar to view its articles, or hit Refresh to fetch new ones.",
            ));
        }
    }

    /// Bulk-fetch read/starred status for every node currently in the
    /// store and copy it onto the nodes. Called after every repopulate.
    pub fn refresh_statuses(&self, account: Arc<Account>) {
        let store = self.store();
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

    /// Whether the search bar is currently revealed. Bound to the
    /// sidebar's `search_btn`'s `active` property.
    pub fn search_active(&self) -> bool {
        self.imp().search_bar.is_search_mode()
    }

    /// Pull keyboard focus into the search entry. Called by the window's
    /// `act_focus_search` (Ctrl+F) after activating the search button.
    pub fn focus_search_entry(&self) {
        self.imp().search_entry.get().grab_focus();
    }
}

/// Escape FTS5 special characters so user input is treated as a literal
/// token. FTS5 reserves `"` for phrase quoting and treats unbalanced
/// quotes as a syntax error; wrapping the term in double quotes after
/// escaping is the minimum safe transform.
fn escape_fts5(term: &str) -> String {
    let escaped = term.replace('"', "\"\"");
    format!("\"{}\"", escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_fts5_wraps_and_doubles_quotes() {
        assert_eq!(escape_fts5("rust"), "\"rust\"");
        assert_eq!(escape_fts5("ru\"st"), "\"ru\"\"st\"");
        assert_eq!(escape_fts5(""), "\"\"");
    }
}
