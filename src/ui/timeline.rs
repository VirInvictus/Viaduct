// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::models::Article;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

glib::wrapper! {
    /// A GObject wrapper around the domain `Article` so it can be used in `gio::ListModel`.
    pub struct ArticleNode(ObjectSubclass<imp::ArticleNode>);
}

pub mod imp {
    use super::*;
    use std::cell::Cell;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ArticleNode)]
    pub struct ArticleNode {
        pub article: RefCell<Option<Article>>,
        /// Optional FTS5 snippet for search-result rows. When set, the timeline
        /// row renders this in the preview area instead of the article summary.
        pub snippet: RefCell<Option<String>>,
        /// Cached status from the `statuses` table. Exposed as glib properties so
        /// the row factory can subscribe to `notify::read` and re-style the title
        /// without waiting for a recycle.
        #[property(get, set)]
        pub read: Cell<bool>,
        #[property(get, set)]
        pub starred: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArticleNode {
        const NAME: &'static str = "ViaductArticleNode";
        type Type = super::ArticleNode;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ArticleNode {}
}

impl ArticleNode {
    pub fn new(article: Article) -> Self {
        let node: Self = glib::Object::builder().build();
        node.imp().article.replace(Some(article));
        node
    }

    pub fn with_snippet(article: Article, snippet: String) -> Self {
        let node: Self = glib::Object::builder().build();
        node.imp().article.replace(Some(article));
        node.imp().snippet.replace(Some(snippet));
        node
    }

    pub fn article(&self) -> Option<Article> {
        self.imp().article.borrow().clone()
    }

    pub fn snippet(&self) -> Option<String> {
        self.imp().snippet.borrow().clone()
    }

    pub fn is_read(&self) -> bool {
        self.read()
    }

    pub fn is_starred(&self) -> bool {
        self.starred()
    }

    pub fn set_status(&self, read: bool, starred: bool) {
        if self.read() != read {
            self.set_read(read);
        }
        if self.starred() != starred {
            self.set_starred(starred);
        }
    }
}

/// Resolver from `feed_id` to display name. Built off the OPML tree once at
/// load time and rebuilt on every OPML mutation. The timeline row factory
/// reads through it on each bind, falling back to the feed_id (URL) when the
/// feed isn't in the map yet.
pub type FeedNameMap = Rc<RefCell<HashMap<String, String>>>;

/// Sets up the Timeline ListView with models and the row factory.
/// Returns the `SingleSelection` so the caller can drive article rendering.
pub fn setup_timeline_list_view(
    list_view: &gtk::ListView,
    list_store: &gtk::gio::ListStore,
    feed_names: FeedNameMap,
) -> gtk::SingleSelection {
    let selection_model = gtk::SingleSelection::new(Some(list_store.clone()));
    selection_model.set_autoselect(false);
    selection_model.set_can_unselect(true);
    list_view.set_model(Some(&selection_model));

    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        // The timeline cell needs: title, source/feed name, date, 2-line preview
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
        vbox.set_margin_start(8);
        vbox.set_margin_end(8);
        vbox.set_margin_top(8);
        vbox.set_margin_bottom(8);

        let top_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let title_label = gtk::Label::new(None);
        title_label.set_hexpand(true);
        title_label.set_halign(gtk::Align::Start);
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

        // Media indicator: small icon when the article has attachments
        // (podcast/video enclosures, MRSS media). Count badge appears when
        // the article carries more than one — bind logic decides the digits.
        let media_icon = gtk::Image::from_icon_name("audio-x-generic-symbolic");
        media_icon.set_pixel_size(12);
        media_icon.add_css_class("dim-label");
        media_icon.set_visible(false);
        let media_count = gtk::Label::new(None);
        media_count.add_css_class("numeric");
        media_count.add_css_class("dim-label");
        media_count.set_visible(false);

        let date_label = gtk::Label::new(None);
        date_label.set_halign(gtk::Align::End);
        date_label.add_css_class("dim-label");

        top_hbox.append(&title_label);
        top_hbox.append(&media_icon);
        top_hbox.append(&media_count);
        top_hbox.append(&date_label);

        let feed_name_label = gtk::Label::new(None);
        feed_name_label.set_halign(gtk::Align::Start);
        feed_name_label.add_css_class("dim-label");

        let preview_label = gtk::Label::new(None);
        preview_label.set_halign(gtk::Align::Start);
        preview_label.set_wrap(true);
        preview_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        preview_label.set_lines(2);
        preview_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview_label.add_css_class("dim-label");

        vbox.append(&top_hbox);
        vbox.append(&feed_name_label);
        vbox.append(&preview_label);

        item.set_child(Some(&vbox));
    });

    let feed_names_for_bind = feed_names.clone();
    factory.connect_bind(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        let node = item.item().and_downcast::<ArticleNode>().unwrap();
        let vbox = item.child().and_downcast::<gtk::Box>().unwrap();

        let top_hbox = vbox.first_child().and_downcast::<gtk::Box>().unwrap();
        let title_label = top_hbox.first_child().and_downcast::<gtk::Label>().unwrap();
        let media_icon = title_label
            .next_sibling()
            .and_downcast::<gtk::Image>()
            .unwrap();
        let media_count = media_icon
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();
        let date_label = media_count
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();

        let feed_name_label = top_hbox
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();
        let preview_label = feed_name_label
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();

        if let Some(article) = node.article() {
            let title = article.title.as_deref().unwrap_or("Untitled");
            title_label.set_text(title);

            apply_read_styling(&title_label, node.read());

            // Media indicator. Pick a roughly-correct icon based on the first
            // attachment's MIME type so podcasts and videos look distinct.
            let n = article.attachments.len();
            if n > 0 {
                let first_type = article.attachments[0].mime_type.as_deref().unwrap_or("");
                let icon_name = if first_type.starts_with("video/") {
                    "video-x-generic-symbolic"
                } else if first_type.starts_with("image/") {
                    "image-x-generic-symbolic"
                } else {
                    "audio-x-generic-symbolic"
                };
                media_icon.set_icon_name(Some(icon_name));
                media_icon.set_visible(true);
                if n > 1 {
                    media_count.set_text(&n.to_string());
                    media_count.set_visible(true);
                } else {
                    media_count.set_visible(false);
                }
            } else {
                media_icon.set_visible(false);
                media_count.set_visible(false);
            }

            let date_str = article
                .date_published
                .map(|d| d.format("%b %e, %Y").to_string())
                .unwrap_or_default();
            date_label.set_text(&date_str);

            // Resolve display name through the feed-name map. Falls back to
            // the feed_id (which is the URL) when the feed isn't loaded yet
            // — startup race when the timeline beats the OPML load.
            let display_name = feed_names_for_bind
                .borrow()
                .get(&article.feed_id)
                .cloned()
                .unwrap_or_else(|| article.feed_id.clone());
            feed_name_label.set_text(&display_name);

            // Search results carry an FTS5 snippet; prefer that over the
            // generic summary/content preview so the user sees why the row
            // matched.
            let preview_source = node.snippet().unwrap_or_else(|| {
                article
                    .summary
                    .as_deref()
                    .or(article.content_text.as_deref())
                    .unwrap_or("")
                    .to_string()
            });
            let clean_preview = preview_source.replace('\n', " ").replace('\r', "");
            preview_label.set_text(&clean_preview);
        }

        // Re-style the title whenever the node's read flag flips. Stored on
        // the list_item so connect_unbind can disconnect cleanly when the
        // row recycles to a different node.
        let title_for_notify = title_label.downgrade();
        let id = node.connect_notify_local(Some("read"), move |node, _| {
            if let Some(label) = title_for_notify.upgrade() {
                apply_read_styling(&label, node.read());
            }
        });
        unsafe {
            item.set_data("viaduct-read-handler", id);
        }
    });

    factory.connect_unbind(|_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");
        let Some(node) = item.item().and_downcast::<ArticleNode>() else {
            return;
        };
        unsafe {
            if let Some(id) = item.steal_data::<glib::SignalHandlerId>("viaduct-read-handler") {
                node.disconnect(id);
            }
        }
    });

    list_view.set_factory(Some(&factory));
    selection_model
}

/// Toggle bold/dim-label classes on the title to reflect read state. NNW
/// renders unread titles in bold full color and read titles in regular
/// weight + slight gray.
fn apply_read_styling(title: &gtk::Label, read: bool) {
    if read {
        title.remove_css_class("heading");
        title.add_css_class("dim-label");
    } else {
        title.remove_css_class("dim-label");
        title.add_css_class("heading");
    }
}
