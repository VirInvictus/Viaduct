// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::models::Article;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;

glib::wrapper! {
    /// A GObject wrapper around the domain `Article` so it can be used in `gio::ListModel`.
    pub struct ArticleNode(ObjectSubclass<imp::ArticleNode>);
}

pub mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ArticleNode {
        pub article: RefCell<Option<Article>>,
        /// Optional FTS5 snippet for search-result rows. When set, the timeline
        /// row renders this in the preview area instead of the article summary.
        pub snippet: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArticleNode {
        const NAME: &'static str = "ViaductArticleNode";
        type Type = super::ArticleNode;
    }

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
}

/// Sets up the Timeline ListView with models and the row factory.
/// Returns the `SingleSelection` so the caller can drive article rendering.
pub fn setup_timeline_list_view(
    list_view: &gtk::ListView,
    list_store: &gtk::gio::ListStore,
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
        // Use bold text for title
        title_label.add_css_class("heading");

        let date_label = gtk::Label::new(None);
        date_label.set_halign(gtk::Align::End);
        date_label.add_css_class("dim-label");

        top_hbox.append(&title_label);
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
        preview_label.add_css_class("dim-label"); // Might not be dim, but preview is usually lighter

        vbox.append(&top_hbox);
        vbox.append(&feed_name_label);
        vbox.append(&preview_label);

        item.set_child(Some(&vbox));
    });

    factory.connect_bind(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        let node = item.item().and_downcast::<ArticleNode>().unwrap();
        let vbox = item.child().and_downcast::<gtk::Box>().unwrap();

        let top_hbox = vbox.first_child().and_downcast::<gtk::Box>().unwrap();
        let title_label = top_hbox.first_child().and_downcast::<gtk::Label>().unwrap();
        let date_label = title_label
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

            let date_str = article
                .date_published
                .map(|d| d.format("%b %e, %Y").to_string())
                .unwrap_or_default();
            date_label.set_text(&date_str);

            // Feed name requires a join with the Feed store, for now we show feed_id or placeholder
            // In a full implementation, we'd pass a resolver or include the feed name in the Article/Node
            feed_name_label.set_text(&article.feed_id);

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
    });

    list_view.set_factory(Some(&factory));
    selection_model
}
