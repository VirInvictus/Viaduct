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
    register(window, "copy-url", |w| w.act_copy_url());
    register(window, "toggle-reader", |w| w.act_toggle_reader());
    register(window, "close-article", |w| w.act_close_article());
    // v2.2.0
    register(window, "print-article", |w| w.act_print_article());

    // App chrome
    register(window, "refresh", |w| w.act_refresh());
    register(window, "focus-search", |w| w.act_focus_search());
    register(window, "toggle-sidebar", |w| w.act_toggle_sidebar());
    register(window, "shortcuts", |w| w.act_shortcuts());
    register(window, "preferences", |w| w.act_preferences());

    // Feed management
    register(window, "add-feed", |w| w.act_add_feed());

    // Context-menu actions on the right-clicked sidebar feed / folder.
    // Read the `right_clicked_feed` / `right_clicked_folder` RefCell
    // populated by the gesture handler before the popover was shown.
    register(window, "refresh-feed", |w| w.act_refresh_clicked_feed());
    register(window, "copy-feed-url", |w| w.act_copy_clicked_feed_url());
    register(window, "delete-feed", |w| w.act_delete_clicked_feed());
    register(window, "mark-feed-read", |w| w.act_mark_clicked_feed_read());
    register(window, "mark-folder-read", |w| {
        w.act_mark_clicked_folder_read()
    });
    // v2.1.0 sidebar editing
    register(window, "rename-feed", |w| w.act_rename_clicked_feed());
    register(window, "move-feed", |w| w.act_move_clicked_feed());
    register(window, "new-folder", |w| w.act_new_folder());
    // v2.4.0 per-feed settings
    register(window, "feed-settings", |w| w.act_feed_settings());
    // v2.6.0: drag-and-drop sidebar reorder fires this action after a
    // successful drop to repopulate the tree.
    register(window, "reload-sidebar", |w| {
        w.reload_sidebar_after_opml_change()
    });

    // OPML import/export — menu only, no accelerators (NNW does the same).
    register(window, "import-opml", |w| w.act_import_opml());
    register(window, "export-opml", |w| w.act_export_opml());
    register(window, "about", |w| w.act_about());

    if crate::is_debug_mode() {
        // Debug
        register(window, "debug-crash", |w| w.act_debug_crash());
        register(window, "debug-clear-caches", |w| w.act_debug_clear_caches());
        register(window, "debug-memory-snapshot", |w| {
            w.act_debug_memory_snapshot()
        });
    }

    // v2.6.22: stateful timeline-sort action backing the
    // `view-sort-descending-symbolic` MenuButton in the timeline
    // header. Initial state pulled from `timeline-sort-order`
    // GSetting; on activate the parameter ("newest-first" /
    // "oldest-first") is the next state. We propagate the change
    // back to the GSetting so the choice persists across runs and
    // any external dconf flip syncs the action state. The window's
    // sidebar-selection handler reads the GSetting fresh each cycle
    // (`current_timeline_sort()`), so a flip + reload-current-
    // timeline picks up the new sort order on the very next click.
    register_stateful_timeline_sort(window);

    // Accelerators. NNW-exact where available; roadmap's additions stacked
    // on top as alternates so both muscle memories work.
    //
    // gtk-rs accelerator strings use GTK's shorthand: "<Ctrl>r", "space",
    // "<Shift>space", "question", etc. — see gtk_accelerator_parse.
    // Space / Shift+Space intentionally NOT bound at the window level —
    // WebKit handles them natively for page-down / page-up inside the
    // article pane. Reintroduces NNW's "advance at bottom" behaviour
    // pending a webkit-side scroll-position monitor (deferred, requires
    // JS bridge that's currently disabled by Phase 6 lockdown).
    // The smart-read / scroll-up actions remain registered so any
    // future binding (or programmatic activation) still routes here.
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
    app.set_accels_for_action("win.copy-url", &["<Ctrl><Shift>c"]);
    app.set_accels_for_action("win.toggle-reader", &["<Ctrl><Shift>r"]);
    app.set_accels_for_action("win.close-article", &["Escape"]);
    app.set_accels_for_action("win.print-article", &["<Ctrl>p"]);
    app.set_accels_for_action("win.add-feed", &["<Ctrl>n"]);
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

/// v2.6.22: install the `win.timeline-sort` stateful action.
/// Parameter type `"s"` (a string nick — "newest-first" or
/// "oldest-first"). Initial state read from the `timeline-sort-order`
/// GSetting; activation writes the new state back to the GSetting
/// and re-fetches the timeline so the user sees the new order
/// immediately. External flips of the GSetting (dconf, another
/// process) sync via `connect_changed`.
fn register_stateful_timeline_sort(window: &ViaductWindow) {
    let initial: glib::Variant = crate::preferences::settings()
        .map(|s| s.string(crate::preferences::keys::TIMELINE_SORT_ORDER))
        .map(|s| s.as_str().to_variant())
        .unwrap_or_else(|| "newest-first".to_variant());

    let action =
        gio::SimpleAction::new_stateful("timeline-sort", Some(glib::VariantTy::STRING), &initial);

    // Activation: GMenu sends the radio target as `parameter`. Hand
    // it to `change_state` so the standard activate→change-state
    // chain fires.
    action.connect_activate(|action, parameter| {
        if let Some(target) = parameter {
            action.change_state(target);
        }
    });

    let weak = window.downgrade();
    action.connect_change_state(move |action, value| {
        let Some(value) = value else { return };
        let Some(nick) = value.str() else { return };
        action.set_state(value);
        if let Some(s) = crate::preferences::settings()
            && s.string(crate::preferences::keys::TIMELINE_SORT_ORDER)
                .as_str()
                != nick
            && let Err(e) = s.set_string(crate::preferences::keys::TIMELINE_SORT_ORDER, nick)
        {
            tracing::warn!(?e, "failed to write timeline-sort-order");
        }
        if let Some(w) = weak.upgrade() {
            w.reload_current_timeline();
        }
    });

    // External flips sync the action state back so the menu's radio
    // mark stays accurate.
    if let Some(settings) = crate::preferences::settings() {
        let action_weak = action.downgrade();
        settings.connect_changed(
            Some(crate::preferences::keys::TIMELINE_SORT_ORDER),
            move |s, _| {
                let nick = s.string(crate::preferences::keys::TIMELINE_SORT_ORDER);
                if let Some(action) = action_weak.upgrade() {
                    action.set_state(&nick.as_str().to_variant());
                }
            },
        );
    }

    window.add_action(&action);
}
