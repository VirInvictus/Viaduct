// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::sync::Arc;

use crate::database::accounts::LocalAccount;
use crate::network::ImageCache;
use crate::paths::{favicon_cache_dir, image_cache_dir};
use crate::ui::article;
use crate::ui::sidebar::{
    SidebarDataSource, SidebarItem, SidebarTreeControllerDelegate, selected_sidebar_item,
    setup_sidebar_list_view,
};
use crate::ui::timeline::{ArticleNode, setup_timeline_list_view};
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
        pub article_text_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub search_btn: TemplateChild<gtk::ToggleButton>,

        pub account: OnceCell<Arc<LocalAccount>>,
        pub image_cache: OnceCell<Arc<ImageCache>>,
        pub timeline_store: OnceCell<gio::ListStore>,
        pub sidebar_delegate: OnceCell<Rc<RefCell<SidebarTreeControllerDelegate>>>,
        pub sidebar_data_source: OnceCell<Rc<SidebarDataSource>>,
        pub sidebar_tree_controller: OnceCell<Rc<TreeController>>,
        /// Pending debounced search timeout, restarted on every keystroke.
        pub search_timeout: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ViaductWindow {
        const NAME: &'static str = "ViaductWindow";
        type Type = super::ViaductWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
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
    pub fn new(app: &adw::Application, account: Arc<LocalAccount>) -> Self {
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
        window
    }

    fn account(&self) -> Arc<LocalAccount> {
        self.imp()
            .account
            .get()
            .cloned()
            .expect("ViaductWindow constructed without LocalAccount")
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
        use std::rc::Rc;

        let imp = self.imp();

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

        // Timeline store + selection.
        let timeline_store = gio::ListStore::new::<ArticleNode>();
        let timeline_selection = setup_timeline_list_view(&imp.timeline_list_view, &timeline_store);

        // Persist references so they outlive `wire_models` and the GC.
        imp.sidebar_delegate.set(delegate.clone()).ok();
        imp.sidebar_tree_controller.set(controller.clone()).ok();
        imp.sidebar_data_source.set(data_source.clone()).ok();
        imp.timeline_store.set(timeline_store.clone()).ok();

        // Initial OPML load — populate the sidebar.
        let account = self.account();
        let delegate_for_load = delegate.clone();
        let controller_for_load = controller.clone();
        let data_source_for_load = data_source.clone();
        glib::spawn_future_local(async move {
            match account.load_opml().await {
                Ok(opml) => {
                    delegate_for_load.borrow().set_opml_file(opml);
                    controller_for_load.rebuild();
                    data_source_for_load.refresh_root();
                }
                Err(e) => {
                    tracing::warn!(?e, "failed to load OPML at startup");
                }
            }
        });

        // Sidebar selection → timeline fetch.
        let account_for_sidebar = self.account();
        let timeline_store_for_sidebar = timeline_store.clone();
        sidebar_selection.connect_selection_changed(move |sel, _pos, _n| {
            let Some(item) = selected_sidebar_item(sel) else {
                return;
            };
            let account = account_for_sidebar.clone();
            let store = timeline_store_for_sidebar.clone();
            glib::spawn_future_local(async move {
                let result = match item {
                    SidebarItem::Feed(feed) => account.fetch_articles_by_feed(feed.id).await,
                    SidebarItem::SmartFeed(name) => match name.as_str() {
                        "Today" => account.fetch_today_articles().await,
                        "All Unread" => account.fetch_unread_articles().await,
                        "Starred" => account.fetch_starred_articles().await,
                        _ => Ok(Vec::new()),
                    },
                    SidebarItem::Folder(_) | SidebarItem::SmartFeedGroup => Ok(Vec::new()),
                };
                match result {
                    Ok(articles) => populate_timeline(&store, articles),
                    Err(e) => tracing::warn!(?e, "failed to fetch articles for sidebar selection"),
                }
            });
        });

        // Timeline selection → article render.
        let text_view = imp.article_text_view.get();
        let image_cache_for_article = self.image_cache();
        timeline_selection.connect_selection_changed(move |sel, _pos, _n| {
            let Some(item) = sel.selected_item() else {
                return;
            };
            let Some(node) = item.downcast_ref::<ArticleNode>() else {
                return;
            };
            let Some(article) = node.article() else {
                return;
            };
            let body = article
                .content_html
                .or(article.content_text)
                .unwrap_or_default();
            article::render_html(&text_view, &body, Some(image_cache_for_article.clone()));
        });

        self.wire_search(timeline_store);
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
                glib::spawn_future_local(async move {
                    match account.search_articles(fts_query).await {
                        Ok(articles) => populate_timeline(&store, articles),
                        Err(e) => tracing::warn!(?e, "FTS5 search failed"),
                    }
                    drop(_entry_keepalive);
                });
            });
            *window.imp().search_timeout.borrow_mut() = Some(new_timeout);
        });
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

fn populate_timeline(store: &gio::ListStore, articles: Vec<crate::models::Article>) {
    store.remove_all();
    for article in articles {
        store.append(&ArticleNode::new(article));
    }
}
