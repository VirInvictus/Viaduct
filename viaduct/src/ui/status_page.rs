// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! The `adw::StatusPage` replacement (Phase 20c, spec.md §12.5): a centred
//! icon over a title over a dim description, for the empty states in the
//! timeline and article panes.
//!
//! A `gtk::Widget` subclass rather than the static `Box` composite
//! `activity_dialog` uses, because `timeline_view::set_empty_state` swaps
//! the icon / title / description at runtime (no-feed-selected vs
//! no-search-results), so the setters have to exist on the widget.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct StatusPage {
        pub image: OnceCell<gtk::Image>,
        pub title: OnceCell<gtk::Label>,
        pub description: OnceCell<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StatusPage {
        const NAME: &'static str = "ViaductStatusPage";
        type Type = super::StatusPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for StatusPage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let outer = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(12)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .hexpand(true)
                .vexpand(true)
                .margin_start(24)
                .margin_end(24)
                .build();

            let image = gtk::Image::new();
            image.set_pixel_size(64);
            image.add_css_class("dim-label");
            outer.append(&image);

            let title = gtk::Label::builder()
                .css_classes(["title-2"])
                .justify(gtk::Justification::Center)
                .wrap(true)
                .max_width_chars(30)
                .build();
            outer.append(&title);

            let description = gtk::Label::builder()
                .css_classes(["dim-label"])
                .justify(gtk::Justification::Center)
                .wrap(true)
                .max_width_chars(40)
                .build();
            outer.append(&description);

            outer.set_parent(&*obj);
            let _ = self.image.set(image);
            let _ = self.title.set(title);
            let _ = self.description.set(description);
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for StatusPage {}
}

glib::wrapper! {
    pub struct StatusPage(ObjectSubclass<imp::StatusPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for StatusPage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl StatusPage {
    pub fn set_icon_name(&self, icon: Option<&str>) {
        if let Some(image) = self.imp().image.get() {
            image.set_icon_name(icon);
        }
    }

    pub fn set_title(&self, title: &str) {
        if let Some(label) = self.imp().title.get() {
            label.set_label(title);
        }
    }

    pub fn set_description(&self, description: Option<&str>) {
        if let Some(label) = self.imp().description.get() {
            label.set_label(description.unwrap_or(""));
            label.set_visible(description.is_some_and(|d| !d.is_empty()));
        }
    }
}
