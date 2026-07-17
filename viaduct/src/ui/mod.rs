// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

// UI layer for GTK4/Libadwaita
use gtk::glib;
use gtk::prelude::*;

/// Close `window` when Escape is pressed. Phase 20c: plain `gtk::Window`
/// has no built-in Escape handling, which the `adw::Dialog` sheets this
/// replaces did for free, so every owned dialog has to ask for it.
///
/// Capture phase, so the dialog closes even while a child entry has focus;
/// `GtkText` would otherwise swallow the key.
pub fn close_on_escape(window: &impl IsA<gtk::Window>) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let window = window.as_ref().clone();
    controller.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(controller);
}

pub mod actions;
pub mod activity_dialog;
pub mod add_feed_dialog;
pub mod article_pane_view;
pub mod article_renderer;
pub mod article_renderer_widget;
pub mod batch;
pub mod coalescing_queue;
pub mod fetch_queue;
pub mod preferences_dialog;
pub mod reader_view;
pub mod refresh;
pub mod rows;
pub mod sidebar;
pub mod sidebar_view;
pub mod smart_feed_dialog;
pub mod timeline;
pub mod timeline_view;
pub mod tree;
pub mod welcome_dialog;
pub mod window;

pub fn init_ui() {
    // Phase 5: Setup AdwNavigationSplitView
}
