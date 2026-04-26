// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Keyboard / menu actions for the main window.
//!
//! Port of NNW's `Shared/Resources/GlobalKeyboardShortcuts.plist` bindings
//! plus the Mac `MainWindowController` IBActions they drive. Every action is
//! a `gio::SimpleAction` on the window's `"win"` group; the application
//! installs accelerators via `adw::Application::set_accels_for_action`.
//!
//! The action bodies themselves live as methods on `ViaductWindow` (see
//! `window.rs`). This module is purely wiring.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::ui::window::ViaductWindow;

/// Registers every window-scoped action and sets its default accelerators.
///
/// Must be called after `ViaductWindow::new` has constructed the widget
/// (we store weak refs on the window inside each closure).
pub fn install(window: &ViaductWindow, app: &adw::Application) {
    // Navigation
    register(window, "smart-read", |w| w.act_smart_read());
    register(window, "scroll-up", |w| w.act_scroll_up());
    register(window, "next-unread", |w| w.act_next_unread());
    register(window, "prev-unread", |w| w.act_prev_unread());

    // Status
    register(window, "toggle-read", |w| w.act_toggle_read());
    register(window, "mark-unread-advance", |w| {
        w.act_mark_unread_advance()
    });
    register(window, "toggle-star", |w| w.act_toggle_star());
    register(window, "mark-all-read", |w| w.act_mark_all_read());
    register(window, "mark-all-read-advance", |w| {
        w.act_mark_all_read_advance()
    });
    register(window, "mark-older-read", |w| w.act_mark_older_read());

    // Open / external
    register(window, "open-in-browser", |w| w.act_open_in_browser());
    register(window, "open-enclosure", |w| w.act_open_enclosure());

    // App chrome
    register(window, "refresh", |w| w.act_refresh());
    register(window, "focus-search", |w| w.act_focus_search());
    register(window, "toggle-sidebar", |w| w.act_toggle_sidebar());
    register(window, "shortcuts", |w| w.act_shortcuts());
    register(window, "preferences", |w| w.act_preferences());

    // OPML import/export — menu only, no accelerators (NNW does the same).
    register(window, "import-opml", |w| w.act_import_opml());
    register(window, "export-opml", |w| w.act_export_opml());

    // Accelerators. NNW-exact where available; roadmap's additions stacked
    // on top as alternates so both muscle memories work.
    //
    // gtk-rs accelerator strings use GTK's shorthand: "<Ctrl>r", "space",
    // "<Shift>space", "question", etc. — see gtk_accelerator_parse.
    app.set_accels_for_action("win.smart-read", &["space"]);
    app.set_accels_for_action("win.scroll-up", &["<Shift>space"]);
    app.set_accels_for_action("win.next-unread", &["n", "Down", "j"]);
    app.set_accels_for_action("win.prev-unread", &["minus", "Up", "k"]);
    app.set_accels_for_action("win.toggle-read", &["r", "m"]);
    app.set_accels_for_action("win.mark-unread-advance", &["<Shift>m"]);
    app.set_accels_for_action("win.toggle-star", &["s"]);
    app.set_accels_for_action("win.mark-all-read", &["<Ctrl>k"]);
    app.set_accels_for_action("win.mark-all-read-advance", &["l"]);
    app.set_accels_for_action("win.mark-older-read", &["o"]);
    app.set_accels_for_action("win.open-in-browser", &["b", "Return"]);
    app.set_accels_for_action("win.open-enclosure", &["<Ctrl>Return"]);
    app.set_accels_for_action("win.refresh", &["<Ctrl>r"]);
    app.set_accels_for_action("win.focus-search", &["<Ctrl>f"]);
    app.set_accels_for_action("win.toggle-sidebar", &["F9"]);
    app.set_accels_for_action("win.shortcuts", &["<Ctrl>question"]);
}

fn register<F>(window: &ViaductWindow, name: &str, body: F)
where
    F: Fn(&ViaductWindow) + 'static,
{
    let action = gio::SimpleAction::new(name, None);
    let weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(w) = weak.upgrade() {
            body(&w);
        }
    });
    window.add_action(&action);
    // Silence unused-import on glib if nothing else references it.
    let _ = glib::MainContext::default();
}
