// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! The `adw::Avatar` replacement (Phase 20c, spec.md §12.5): a small round
//! sidebar icon that shows a feed's favicon when we have one and falls back
//! to initials on a deterministic colour otherwise. Net-new — Colophon had
//! no avatars.
//!
//! A `gtk::Widget` subclass wrapping a `gtk::Stack` of two pages: a Cairo
//! `DrawingArea` that paints the colour circle + initials, and a
//! `gtk::Picture` for the favicon. The favicon rides a Picture rather than
//! being Cairo-painted so we never have to download a `gdk::Texture` into an
//! image surface by hand.
//!
//! The colour is `network::cache::color_for` (NNW `ColorHash`, MD5 of the
//! text), matching the sidebar's prior behaviour. Because a raw hash can
//! land anywhere, the initials are drawn black or white by the background's
//! luminance rather than assuming white reads.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Avatar {
        pub stack: RefCell<Option<gtk::Stack>>,
        pub drawing: RefCell<Option<gtk::DrawingArea>>,
        pub picture: RefCell<Option<gtk::Picture>>,
        pub initials: RefCell<String>,
        pub rgb: Cell<(f64, f64, f64)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Avatar {
        const NAME: &'static str = "ViaductAvatar";
        type Type = super::Avatar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for Avatar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let drawing = gtk::DrawingArea::new();
            let imp_weak = obj.downgrade();
            drawing.set_draw_func(move |_area, cr, w, h| {
                let Some(obj) = imp_weak.upgrade() else {
                    return;
                };
                obj.imp().draw(cr, w, h);
            });

            let picture = gtk::Picture::new();
            picture.set_content_fit(gtk::ContentFit::Cover);
            picture.add_css_class("viaduct-avatar-image");
            picture.set_overflow(gtk::Overflow::Hidden);

            let stack = gtk::Stack::new();
            stack.set_transition_type(gtk::StackTransitionType::None);
            stack.add_named(&drawing, Some("initials"));
            stack.add_named(&picture, Some("image"));
            stack.set_parent(&*obj);

            self.stack.replace(Some(stack));
            self.drawing.replace(Some(drawing));
            self.picture.replace(Some(picture));
        }

        fn dispose(&self) {
            if let Some(stack) = self.stack.borrow_mut().take() {
                stack.unparent();
            }
        }
    }

    impl WidgetImpl for Avatar {}

    impl Avatar {
        fn draw(&self, cr: &gtk::cairo::Context, w: i32, h: i32) {
            let w = w as f64;
            let h = h as f64;
            let d = w.min(h);
            let cx = w / 2.0;
            let cy = h / 2.0;
            let r = d / 2.0;

            let (br, bg, bb) = self.rgb.get();
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            cr.set_source_rgb(br, bg, bb);
            let _ = cr.fill();

            let initials = self.initials.borrow();
            if initials.is_empty() {
                return;
            }

            // Black or white by the background's relative luminance, so the
            // initials read on any hash colour.
            let luminance = 0.299 * br + 0.587 * bg + 0.114 * bb;
            let ink = if luminance > 0.6 { 0.0 } else { 1.0 };
            cr.set_source_rgb(ink, ink, ink);

            // Cairo's toy text API rather than pango: one or two glyphs need
            // no shaping, and it avoids a pangocairo dependency.
            cr.select_font_face(
                "sans-serif",
                gtk::cairo::FontSlant::Normal,
                gtk::cairo::FontWeight::Bold,
            );
            cr.set_font_size(r * 0.9);
            if let Ok(ext) = cr.text_extents(&initials) {
                let tx = cx - (ext.width() / 2.0 + ext.x_bearing());
                let ty = cy - (ext.height() / 2.0 + ext.y_bearing());
                cr.move_to(tx, ty);
                let _ = cr.show_text(&initials);
            }
        }
    }
}

glib::wrapper! {
    pub struct Avatar(ObjectSubclass<imp::Avatar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Avatar {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Avatar {
    pub fn new(size: i32) -> Self {
        let obj: Self = glib::Object::new();
        obj.set_size_request(size, size);
        obj
    }

    /// Show initials derived from `text`, on a colour hashed from it. Clears
    /// any favicon so a recycled row falls back correctly.
    pub fn set_text(&self, text: &str) {
        let imp = self.imp();
        *imp.initials.borrow_mut() = initials_for(text);
        imp.rgb.set(parse_hex(&crate::network::color_for(text)));
        self.set_custom_image(None);
        if let Some(drawing) = imp.drawing.borrow().as_ref() {
            drawing.queue_draw();
        }
    }

    /// Show `texture` as the favicon, or fall back to initials when `None`.
    pub fn set_custom_image(&self, texture: Option<&gtk::gdk::Texture>) {
        let imp = self.imp();
        let (Some(stack), Some(picture)) =
            (imp.stack.borrow().clone(), imp.picture.borrow().clone())
        else {
            return;
        };
        match texture {
            Some(texture) => {
                picture.set_paintable(Some(texture));
                stack.set_visible_child_name("image");
            }
            None => {
                picture.set_paintable(gtk::gdk::Paintable::NONE);
                stack.set_visible_child_name("initials");
            }
        }
    }
}

/// First letter of the first one or two words, uppercased.
fn initials_for(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace().take(2) {
        if let Some(c) = word.chars().next() {
            out.extend(c.to_uppercase());
        }
    }
    out
}

/// `#rrggbb` → 0.0–1.0 RGB, mid-grey on anything malformed.
fn parse_hex(hex: &str) -> (f64, f64, f64) {
    let byte = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .map(|b| b as f64 / 255.0)
    };
    match (byte(1), byte(3), byte(5)) {
        (Some(r), Some(g), Some(b)) => (r, g, b),
        _ => (0.5, 0.5, 0.5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_first_two_words() {
        assert_eq!(initials_for("Daring Fireball"), "DF");
        assert_eq!(initials_for("xkcd"), "X");
        assert_eq!(initials_for("  the  quick  brown"), "TQ");
        assert_eq!(initials_for(""), "");
    }

    #[test]
    fn hex_parses_and_falls_back() {
        assert_eq!(parse_hex("#ff0000"), (1.0, 0.0, 0.0));
        assert_eq!(parse_hex("#000000"), (0.0, 0.0, 0.0));
        assert_eq!(parse_hex("garbage"), (0.5, 0.5, 0.5));
    }
}
