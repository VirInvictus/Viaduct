//! Dark/light resolution over `org.freedesktop.portal.Settings`, the
//! replacement for `adw::StyleManager` (Phase 20b, spec.md §12.3).
//!
//! Ported from the Colophon pilot's `theme.rs`, which proved the D-Bus
//! shape. Two deliberate divergences, both to preserve viaduct's behavior
//! rather than adopt the pilot's:
//!
//! * **The no-portal default is light, not dark.** Colophon degrades to a
//!   dark default because its palette is dark. `AdwStyleManager` with no
//!   portal answering resolves to light, and `color-scheme` defaults to
//!   `"default"` (follow system), so light is what viaduct does today.
//! * **The system scheme is composed with the app's own `color-scheme`
//!   preference.** `AdwStyleManager::is_dark()` already folds ForceLight /
//!   ForceDark over the system value; a raw portal read does not, and
//!   swapping one for the other would quietly break "Force light".
//!
//! While libadwaita is still present this runs *alongside* it: both read
//! the same portal and compose the same preference, so they agree. The
//! toolkit cut (20c) removes `set_color_scheme` and makes this the only
//! source.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};

/// A dark-state listener and the object whose life it is tied to. The
/// owner is weak so a dead widget's listener prunes itself.
type Listener = (glib::WeakRef<glib::Object>, Box<dyn Fn(bool)>);

/// Matches `AdwStyleManager`: of the portal's `color-scheme` values (0 no
/// preference, 1 prefer dark, 2 prefer light) only an explicit 1 is dark,
/// so behavior under GNOME is unchanged.
fn portal_scheme_is_dark(scheme: u32) -> bool {
    scheme == 1
}

/// Fold the user's `color-scheme` preference over the system value, the
/// way `AdwStyleManager::is_dark()` does. Any nick other than the two
/// force-* values means follow the system, matching `update_style_manager`.
fn resolve_is_dark(nick: &str, system_dark: bool) -> bool {
    match nick {
        "force-light" => false,
        "force-dark" => true,
        _ => system_dark,
    }
}

thread_local! {
    /// Guards against a second subscription; see `init`.
    static INITIALIZED: Cell<bool> = const { Cell::new(false) };
    /// Light unless the portal says otherwise; see the module note.
    static SYSTEM_DARK: Cell<bool> = const { Cell::new(false) };
    /// The system value with the user's preference folded in: what
    /// `is_dark()` returns and what listeners are told about. Cached so a
    /// flip can be deduplicated, matching `notify::dark`'s semantics.
    static RESOLVED_DARK: Cell<bool> = const { Cell::new(false) };
    /// Held so the preference can be re-read when the portal flips, and so
    /// the `color-scheme` subscription outlives `init`.
    static SETTINGS: RefCell<Option<gio::Settings>> = const { RefCell::new(None) };
    /// The subscription dies with the connection, so the connection has to
    /// outlive `init`'s stack frame.
    static BUS: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
    /// Weak-ref listener registry. The pilot's migration exposed a real
    /// leak here: per-widget `connect_dark_notify` closures accumulated on
    /// the `StyleManager` singleton and were never disconnected, so every
    /// widget ever constructed kept being called. Holding weak refs and
    /// pruning dead ones on each flip is strictly better than what
    /// libadwaita gave us.
    static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
}

/// Whether the desktop currently prefers dark, ignoring app preference.
pub fn system_is_dark() -> bool {
    SYSTEM_DARK.with(Cell::get)
}

/// The resolved dark state: the system preference with the user's
/// `color-scheme` folded over it. Drop-in for
/// `adw::StyleManager::default().is_dark()`.
pub fn is_dark() -> bool {
    RESOLVED_DARK.with(Cell::get)
}

/// Recompute the resolved state, telling listeners only if it actually
/// flipped. Both triggers land here, which is the point: libadwaita's
/// `notify::dark` fired for a system change *and* for a `set_color_scheme`
/// call, so a listener watching only the portal would miss the user picking
/// "Force dark" in Preferences.
fn re_resolve() {
    let nick = SETTINGS.with(|s| {
        s.borrow()
            .as_ref()
            .map(|s| s.string(super::preferences::keys::COLOR_SCHEME).to_string())
    });
    // No schema installed (a dev environment): follow the system outright,
    // which is what the "default" nick resolves to anyway.
    let dark = match nick {
        Some(nick) => resolve_is_dark(&nick, system_is_dark()),
        None => system_is_dark(),
    };
    if RESOLVED_DARK.with(Cell::get) == dark {
        return;
    }
    RESOLVED_DARK.with(|d| d.set(dark));
    broadcast(dark);
}

/// Run `f` whenever the resolved dark state may have changed, for as long
/// as `owner` is alive. `owner` is held weakly and its death prunes the
/// entry, so a listener can never outlive the widget it redraws.
pub fn connect_dark_changed<F>(owner: &impl IsA<glib::Object>, f: F)
where
    F: Fn(bool) + 'static,
{
    let weak = owner.upcast_ref::<glib::Object>().downgrade();
    LISTENERS.with(|l| l.borrow_mut().push((weak, Box::new(f))));
}

/// Notify live listeners and drop dead ones.
fn broadcast(dark: bool) {
    // Move the registry out before calling anything: a listener is free to
    // register another (a widget rebuilding itself), and holding the borrow
    // across the callbacks would panic on the re-entrant borrow_mut.
    let mut listeners = LISTENERS.with(|l| std::mem::take(&mut *l.borrow_mut()));
    listeners.retain(|(owner, _)| owner.upgrade().is_some());
    for (_, f) in &listeners {
        f(dark);
    }
    LISTENERS.with(|l| {
        let mut current = l.borrow_mut();
        // Anything registered during the callbacks is in `current` now;
        // keep it, ordered after the pre-existing entries.
        listeners.append(&mut current);
        *current = listeners;
    });
}

/// Read the desktop's dark preference and keep following it, folding the
/// user's `color-scheme` over it. The first read is synchronous, once,
/// before the first frame, so the app never paints the wrong polarity and
/// then corrects itself. Every failure path leaves the default in place
/// rather than erroring: a missing portal is a normal state on a bare
/// session, not a fault.
///
/// `settings` is `None` when the schema isn't installed (a dev environment
/// without `glib-compile-schemas`), in which case the app follows the
/// system with no preference to fold.
pub fn init(settings: Option<gio::Settings>) {
    // `build_ui` runs per activation, and a second D-Bus subscription would
    // mean every listener firing twice per flip.
    if INITIALIZED.with(Cell::get) {
        return;
    }
    INITIALIZED.with(|i| i.set(true));

    if let Some(settings) = settings {
        // The preference is half of the resolved value, so a flip of it is
        // as much a "dark changed" event as a system flip is.
        settings.connect_changed(Some(super::preferences::keys::COLOR_SCHEME), |_, _| {
            re_resolve()
        });
        SETTINGS.with(|s| *s.borrow_mut() = Some(settings));
    }

    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        tracing::debug!("no session bus; dark/light stays at the light default");
        re_resolve();
        return;
    };
    if let Some(scheme) = read_portal_scheme(&conn) {
        SYSTEM_DARK.with(|d| d.set(portal_scheme_is_dark(scheme)));
    } else {
        tracing::debug!("settings portal did not answer; dark/light stays at the light default");
    }

    conn.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Settings"),
        Some("SettingChanged"),
        Some("/org/freedesktop/portal/desktop"),
        None,
        gio::DBusSignalFlags::NONE,
        |_, _, _, _, _, params| {
            let ns = params.child_value(0).get::<String>();
            let key = params.child_value(1).get::<String>();
            if ns.as_deref() != Some("org.freedesktop.appearance")
                || key.as_deref() != Some("color-scheme")
            {
                return;
            }
            let Some(dark) = params
                .child_value(2)
                .as_variant()
                .and_then(|v| v.get::<u32>())
                .map(portal_scheme_is_dark)
            else {
                return;
            };
            SYSTEM_DARK.with(|d| d.set(dark));
            // Through re_resolve, not broadcast: a forced theme holds its
            // polarity across a system flip, and the dedupe keeps a no-op
            // flip from re-rendering the article pane.
            re_resolve();
        },
    );
    BUS.with(|b| *b.borrow_mut() = Some(conn));

    // Seed the resolved value from the first read. Deliberately last, so a
    // listener registered before init (there are none today, but the order
    // shouldn't be load-bearing) sees a consistent state.
    re_resolve();
}

/// Ask the settings portal for the current colour scheme. `None` on any
/// failure (no portal backend, no bus): the caller keeps its default.
fn read_portal_scheme(conn: &gio::DBusConnection) -> Option<u32> {
    let args = ("org.freedesktop.appearance", "color-scheme").to_variant();
    let reply_ty = glib::VariantTy::new("(v)").ok()?;
    let call = |method: &str| {
        conn.call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            method,
            Some(&args),
            Some(reply_ty),
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )
    };
    match call("ReadOne") {
        Ok(reply) => reply.child_value(0).as_variant()?.get::<u32>(),
        // Portals predating ReadOne answer Read, which wraps the value in
        // a second layer of variant.
        Err(_) => call("Read")
            .ok()?
            .child_value(0)
            .as_variant()?
            .as_variant()?
            .get::<u32>(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_scheme_maps_like_adwaita() {
        // 0 = no preference, 1 = prefer dark, 2 = prefer light.
        assert!(portal_scheme_is_dark(1));
        assert!(!portal_scheme_is_dark(0));
        assert!(!portal_scheme_is_dark(2));
        // Unknown future values must not read as dark.
        assert!(!portal_scheme_is_dark(99));
    }

    #[test]
    fn force_preferences_beat_the_system() {
        assert!(!resolve_is_dark("force-light", true));
        assert!(resolve_is_dark("force-dark", false));
    }

    #[test]
    fn default_follows_the_system() {
        assert!(resolve_is_dark("default", true));
        assert!(!resolve_is_dark("default", false));
        // An unrecognised nick follows the system, matching
        // update_style_manager's catch-all arm.
        assert!(resolve_is_dark("nonsense", true));
    }
}
