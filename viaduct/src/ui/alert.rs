// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! The `adw::AlertDialog` replacement (Phase 20c, spec.md §12.5): a small
//! modal `gtk::Window` with a heading, a body, an optional extra child, and
//! a row of response buttons. No pilot precedent — Colophon never used
//! `AlertDialog` — so this is authored against the exact shape the five
//! `window.rs` call sites need and nothing more.
//!
//! Deliberately narrower than `adw::AlertDialog`: every one of our call
//! sites early-returns on the non-affirmative response, so there is no
//! "close response" callback. Escape and the close button simply dismiss;
//! the handler fires only for a real button press.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The visual weight of a response button, mapping to the CSS classes the
/// owned stylesheet (20d) and libadwaita both define.
#[derive(Copy, Clone)]
pub enum ResponseStyle {
    Normal,
    Suggested,
    Destructive,
}

pub struct Alert {
    window: gtk::Window,
    buttons: gtk::Box,
    default_id: Rc<RefCell<Option<String>>>,
    button_by_id: Rc<RefCell<Vec<(String, gtk::Button)>>>,
    extra_anchor: gtk::Box,
}

impl Alert {
    pub fn new(parent: &impl IsA<gtk::Window>, heading: Option<&str>, body: Option<&str>) -> Self {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(18)
            .margin_start(24)
            .margin_end(24)
            .build();

        if let Some(heading) = heading {
            content.append(
                &gtk::Label::builder()
                    .label(heading)
                    .wrap(true)
                    .justify(gtk::Justification::Center)
                    .css_classes(["title-3"])
                    .build(),
            );
        }
        if let Some(body) = body {
            content.append(
                &gtk::Label::builder()
                    .label(body)
                    .wrap(true)
                    .justify(gtk::Justification::Center)
                    .css_classes(["body"])
                    .build(),
            );
        }

        // Extra children (a rename entry, the feed-settings switches) sit
        // between the body and the buttons.
        let extra_anchor = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&extra_anchor);

        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_top(6)
            .homogeneous(true)
            .build();
        content.append(&buttons);

        let window = gtk::Window::builder()
            .transient_for(parent)
            .modal(true)
            .resizable(false)
            .default_width(380)
            .titlebar(&gtk::HeaderBar::builder().visible(false).build())
            .child(&content)
            .build();
        crate::ui::close_on_escape(&window);

        Self {
            window,
            buttons,
            default_id: Rc::new(RefCell::new(None)),
            button_by_id: Rc::new(RefCell::new(Vec::new())),
            extra_anchor,
        }
    }

    /// Add a widget between the body text and the buttons.
    pub fn set_extra_child(&self, child: &impl IsA<gtk::Widget>) {
        self.extra_anchor.append(child);
    }

    pub fn add_response(&self, id: &str, label: &str, style: ResponseStyle) {
        let button = gtk::Button::with_label(label);
        match style {
            ResponseStyle::Normal => {}
            ResponseStyle::Suggested => button.add_css_class("suggested-action"),
            ResponseStyle::Destructive => button.add_css_class("destructive-action"),
        }
        self.buttons.append(&button);
        self.button_by_id
            .borrow_mut()
            .push((id.to_string(), button));
    }

    /// The response Enter activates. Also gets `.default` styling via GTK's
    /// default-widget mechanism, and lets an entry with `activates_default`
    /// submit on Enter.
    pub fn set_default_response(&self, id: &str) {
        *self.default_id.borrow_mut() = Some(id.to_string());
    }

    /// Show the dialog. `handler` fires with the response id on a real
    /// button press; Escape and the close button just dismiss.
    pub fn present<F: Fn(&str) + 'static>(self, handler: F) {
        let handler = Rc::new(handler);
        for (id, button) in self.button_by_id.borrow().iter() {
            let id = id.clone();
            let handler = handler.clone();
            let window = self.window.clone();
            button.connect_clicked(move |_| {
                window.close();
                handler(&id);
            });
        }

        if let Some(default_id) = self.default_id.borrow().as_ref()
            && let Some((_, button)) = self
                .button_by_id
                .borrow()
                .iter()
                .find(|(id, _)| id == default_id)
        {
            self.window.set_default_widget(Some(button));
        }

        self.window.present();
    }

    /// Escape hatch for the rare caller that needs the window itself (e.g.
    /// to grab focus into an entry after presenting).
    pub fn window(&self) -> &gtk::Window {
        &self.window
    }
}
