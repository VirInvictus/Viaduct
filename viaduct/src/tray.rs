// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! v2.5.0 — System tray indicator for run-in-background mode.
//!
//! Wires a `ksni`-backed StatusNotifierItem when the `run-in-background`
//! GSetting is on, so closing the window doesn't make the app vanish
//! invisibly. The tray icon shows up wherever the desktop hosts an SNI
//! tray (KDE / XFCE / Cinnamon / MATE natively; GNOME via the
//! AppIndicator extension). Left-click = `Application::activate()` (i.e.
//! re-summon the existing window via the same path the dock icon uses
//! in `main.rs build_ui`); right-click menu = "Show viaduct" / "Quit
//! viaduct". Quit goes through `gio::Application::quit()` which bypasses
//! our `connect_close_request` hide-instead-of-quit branch — that's the
//! whole point of the menu item.
//!
//! Lifecycle: the tray runs whenever `run-in-background` is enabled,
//! regardless of window-visibility state. Flip the GSetting off and the
//! handle's `shutdown()` is called; flip it on and a fresh service
//! spawns. Initial start at app startup reads the current GSetting
//! value.
//!
//! The `ksni::TrayService` runs on its own tokio runtime thread inside
//! ksni's worker. Menu callbacks fire there, so we use a
//! `tokio::sync::mpsc` channel to deliver `TrayAction`s back to the GTK
//! main thread, where a `glib::spawn_future_local` task awaits and
//! dispatches.

use gtk::glib;
use gtk::prelude::*;
use ksni::menu::StandardItem;
use ksni::{MenuItem, Tray, TrayMethods};
use std::cell::RefCell;

#[derive(Debug, Clone, Copy)]
enum TrayAction {
    Show,
    Quit,
}

struct ViaductTray {
    tx: tokio::sync::mpsc::UnboundedSender<TrayAction>,
}

impl Tray for ViaductTray {
    fn title(&self) -> String {
        "viaduct".into()
    }

    fn id(&self) -> String {
        "org.virinvictus.Viaduct".into()
    }

    fn icon_name(&self) -> String {
        // Resolves against the system icon theme. After Flatpak / Meson
        // install the desktop entry's `Icon=org.virinvictus.Viaduct`
        // resolves; in dev builds without install the SNI host falls
        // back to a default placeholder. Acceptable trade — tray work
        // is primarily for shipped installs.
        "org.virinvictus.Viaduct".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayAction::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show viaduct".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(TrayAction::Show);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit viaduct".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

thread_local! {
    /// Active service handle. `None` when run-in-background is off (no
    /// tray showing). Lives on the GTK main thread; the inner
    /// `ksni::Handle` is `Send` but we don't need to share it — start /
    /// stop calls happen exclusively here.
    static TRAY_HANDLE: RefCell<Option<ksni::Handle<ViaductTray>>> =
        const { RefCell::new(None) };
}

/// Wire the tray: install the GSetting change listener, start the
/// service if run-in-background is currently on, and attach the
/// receiver loop that dispatches `TrayAction`s to the GTK main thread.
/// Call once from `main.rs build_ui` after the application is built.
pub fn wire(app: &adw::Application) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<TrayAction>();

    // Receiver loop on the GTK main thread.
    let app_for_loop = app.clone();
    glib::spawn_future_local(receive_loop(rx, app_for_loop));

    let Some(settings) = crate::preferences::settings() else {
        // Schema not installed (dev env without `glib-compile-schemas`).
        // Tray simply won't fire — the app still works.
        return;
    };

    if crate::preferences::run_in_background(&settings) {
        start_service(tx.clone());
    }

    let tx_for_changes = tx.clone();
    settings.connect_changed(
        Some(crate::preferences::keys::RUN_IN_BACKGROUND),
        move |s, _| {
            if crate::preferences::run_in_background(s) {
                start_service(tx_for_changes.clone());
            } else {
                stop_service();
            }
        },
    );
}

async fn receive_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<TrayAction>,
    app: adw::Application,
) {
    while let Some(action) = rx.recv().await {
        match action {
            TrayAction::Show => {
                // Same path the dock-icon click takes (Phase 17 D-Bus
                // re-summon). build_ui will find the existing window
                // and `present()` it, or build a new one if the user
                // ran the binary without one.
                app.activate();
            }
            TrayAction::Quit => {
                // gio::Application::quit() ends the main loop without
                // firing connect_close_request, so the run-in-background
                // hide-instead-of-quit branch is bypassed cleanly.
                stop_service();
                app.quit();
            }
        }
    }
}

fn start_service(tx: tokio::sync::mpsc::UnboundedSender<TrayAction>) {
    TRAY_HANDLE.with(|cell| {
        if cell.borrow().is_some() {
            // Already running — flip-on-while-on is a no-op.
            return;
        }
        let tray = ViaductTray { tx };
        // ksni's `spawn` consumes the tray and yields a Handle on the
        // current Tokio runtime (the global one we install in `main`).
        // Has to run from a Tokio context; the wire() call site is on
        // the GTK thread but the global runtime is reachable from
        // anywhere via the runtime-builder we stored.
        let handle = match crate::block_on_runtime(async move { tray.spawn().await }) {
            Ok(handle) => handle,
            Err(e) => {
                tracing::warn!(?e, "ksni tray spawn failed; sys-tray disabled");
                return;
            }
        };
        cell.borrow_mut().replace(handle);
    });
}

fn stop_service() {
    TRAY_HANDLE.with(|cell| {
        if let Some(handle) = cell.borrow_mut().take() {
            handle.shutdown();
        }
    });
}
