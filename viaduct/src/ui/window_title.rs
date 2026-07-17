// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! The `adw::WindowTitle` replacement (Phase 20c, spec.md §12.5): a centred
//! title over an optional dim subtitle, for the article pane's header bar.
//! Both lines ellipsize. A `gtk::Widget` subclass so the header bar's
//! `title-widget` slot and the `set_title` / `set_subtitle` call sites in
//! `article_pane_view` port unchanged.

use gtk::glib;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct WindowTitle {
        pub title: OnceCell<gtk::Label>,
        pub subtitle: OnceCell<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WindowTitle {
        const NAME: &'static str = "ViaductWindowTitle";
        type Type = super::WindowTitle;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for WindowTitle {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let outer = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .valign(gtk::Align::Center)
                .build();

            let title = gtk::Label::builder()
                .ellipsize(pango::EllipsizeMode::End)
                .single_line_mode(true)
                .css_classes(["title"])
                .build();
            outer.append(&title);

            let subtitle = gtk::Label::builder()
                .ellipsize(pango::EllipsizeMode::End)
                .single_line_mode(true)
                .visible(false)
                .css_classes(["subtitle"])
                .build();
            outer.append(&subtitle);

            outer.set_parent(&*obj);
            let _ = self.title.set(title);
            let _ = self.subtitle.set(subtitle);
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for WindowTitle {}
}

glib::wrapper! {
    pub struct WindowTitle(ObjectSubclass<imp::WindowTitle>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for WindowTitle {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl WindowTitle {
    pub fn set_title(&self, title: &str) {
        if let Some(label) = self.imp().title.get() {
            label.set_label(title);
        }
    }

    pub fn set_subtitle(&self, subtitle: &str) {
        if let Some(label) = self.imp().subtitle.get() {
            label.set_label(subtitle);
            label.set_visible(!subtitle.is_empty());
        }
    }
}
