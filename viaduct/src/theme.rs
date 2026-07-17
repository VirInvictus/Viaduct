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

// ---------------------------------------------------------------------------
// The owned application stylesheet (Phase 20d, spec.md §12.4).
//
// Replaces libadwaita's stylesheet with one viaduct controls: a flat, square,
// hard-bordered Kanagawa palette. Two palettes (Dragon dark / Lotus light)
// picked by `is_dark()`; the structure below is palette-independent and reads
// every colour through `--c-*` GTK 4.16 custom properties.
//
// Loaded at `STYLE_PROVIDER_PRIORITY_USER + 1`, deliberately: a global
// `~/.config/gtk-4.0/gtk.css` (the dev machine's Kanagawa system theme) sits
// at USER (800) and outranks APPLICATION (600), so an app sheet at
// APPLICATION would be silently half-overridden. USER+1 beats it. This is
// also what makes Force light work: the system dark gtk.css no longer wins.
// ---------------------------------------------------------------------------

/// A flat colour palette. Field names map 1:1 to the `--c-*` custom
/// properties the structure reads.
struct Palette {
    bg: &'static str,
    bg_window: &'static str,
    bg_header: &'static str,
    bg_card: &'static str,
    fg: &'static str,
    fg_dim: &'static str,
    heading: &'static str,
    accent: &'static str,
    on_accent: &'static str,
    grid: &'static str,
    warn: &'static str,
    err: &'static str,
    ok: &'static str,
}

/// Kanagawa Dragon (dark). Lifted from the Colophon pilot, which lifted it
/// from kanagawa.nvim's Dragon variant, the theme Brandon's desktop runs.
const DRAGON: Palette = Palette {
    bg: "#12120f",
    bg_window: "#181616",
    bg_header: "#0d0c0c",
    bg_card: "#1d1c19",
    fg: "#c5c9c5",
    fg_dim: "#a6a69c",
    heading: "#c8c093",
    accent: "#8ba4b0",
    on_accent: "#0d0c0c",
    grid: "#282727",
    warn: "#c4b28a",
    err: "#c4746e",
    ok: "#87a987",
};

/// Kanagawa Lotus (light), the Dragon counterpart for Force-light / a light
/// desktop.
const LOTUS: Palette = Palette {
    bg: "#f2ecbc",
    bg_window: "#e7dba0",
    bg_header: "#e5ddb0",
    bg_card: "#e5ddb0",
    fg: "#545464",
    fg_dim: "#8a8980",
    heading: "#43436c",
    accent: "#4d699b",
    on_accent: "#f2ecbc",
    grid: "#d5cea3",
    warn: "#836f4a",
    err: "#c84053",
    ok: "#6f894e",
};

/// Palette-independent structure. Reads colours through `var(--c-*)`; the
/// scoped focus ring is lifted verbatim from the pilot (see the comment).
const STRUCTURE: &str = "\
window { background-color: var(--c-bg-window); color: var(--c-fg); }
window.csd, decoration { border-radius: 0; box-shadow: none; }

headerbar {
  background-color: var(--c-bg-header);
  background-image: none;
  color: var(--c-fg);
  box-shadow: none;
  border-bottom: 1px solid var(--c-grid);
  min-height: 34px;
  padding: 0 4px;
}
headerbar button { min-height: 24px; }

paned > separator {
  background-color: var(--c-grid);
  background-image: none;
  min-width: 1px;
  min-height: 1px;
}

.title-1 { font-weight: 800; font-size: 170%; }
.title-2 { font-weight: 800; font-size: 140%; }
.title-3 { font-weight: 700; font-size: 120%; }
.title-4 { font-weight: 700; font-size: 105%; }
.heading { font-weight: 700; }
.title { font-weight: 700; }
.subtitle { color: var(--c-fg-dim); font-size: 90%; }
.caption { font-size: 82%; }
.dim-label { color: var(--c-fg-dim); }
.success { color: var(--c-ok); }
.error { color: var(--c-err); }

.card, list.boxed-list {
  background-color: var(--c-bg-card);
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  border-radius: 0;
  box-shadow: none;
}
list.boxed-list > row { border-bottom: 1px solid var(--c-grid); }
list.boxed-list > row:last-child { border-bottom: none; }

list, listview { background-color: transparent; }
row { border-radius: 0; }
row.activatable:hover { background-color: var(--c-grid); }
row:selected { background-color: var(--c-accent); color: var(--c-on-accent); }
row:selected label { color: var(--c-on-accent); }

entry, spinbutton {
  background-color: var(--c-bg-card);
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  border-radius: 0;
  box-shadow: none;
  min-height: 24px;
}

button {
  background-color: var(--c-bg-card);
  background-image: none;
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  border-radius: 0;
  box-shadow: none;
  min-height: 24px;
  padding: 2px 10px;
}
button:hover { background-color: var(--c-grid); }
button:active, button:checked {
  background-color: var(--c-accent);
  color: var(--c-on-accent);
  border-color: var(--c-accent);
}
button.flat { background-color: transparent; border-color: transparent; }
button.flat:hover { background-color: var(--c-grid); }
button.suggested-action {
  background-color: var(--c-accent);
  color: var(--c-on-accent);
  border-color: var(--c-accent);
}
button.destructive-action {
  background-color: var(--c-err);
  color: var(--c-bg-window);
  border-color: var(--c-err);
}
.linked > button:not(:first-child) { border-left-width: 0; }

popover > arrow { background-color: var(--c-bg-card); }
popover > contents {
  background-color: var(--c-bg-card);
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  border-radius: 0;
  box-shadow: none;
  padding: 4px;
}
popover.menu modelbutton { border-radius: 0; padding: 5px 8px; }
modelbutton:hover { background-color: var(--c-accent); color: var(--c-on-accent); }
popover.menu separator { background-color: var(--c-grid); min-height: 1px; margin: 4px 0; }

.toast {
  background-color: var(--c-bg-card);
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  padding: 6px 12px;
}

tooltip, tooltip.background {
  background-color: var(--c-bg-header);
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  border-radius: 0;
  box-shadow: none;
  padding: 4px 8px;
}

scrollbar { background-color: transparent; }
scrollbar slider {
  background-color: var(--c-grid);
  border-radius: 0;
  min-width: 6px;
  min-height: 6px;
}
scrollbar slider:hover { background-color: var(--c-fg-dim); }

selection { background-color: var(--c-accent); color: var(--c-on-accent); }

/* Keyboard-focus ring, scoped to discrete interactive controls, NOT a
   universal `*`: pressing a bare modifier (a tiling workspace-switch chord)
   flips GTK into keyboard-focus-visible mode, and a `*` rule then outlines
   every widget in the focus chain at once, flashing the accent across the
   whole window. It does not reproduce in a screenshot, which is how it
   escaped the pilot's verification. Rows show position via the selection
   background, so they need no outline. */
button:focus-visible,
entry:focus-visible,
spinbutton:focus-visible,
switch:focus-visible,
checkbutton:focus-visible,
check:focus-visible,
dropdown:focus-visible,
scale:focus-visible { outline: 1px solid var(--c-accent); outline-offset: -1px; }

/* viaduct-specific classes. */
.viaduct-sidebar-heading {
  font-size: 80%;
  font-weight: 700;
  letter-spacing: 1px;
  color: var(--c-fg-dim);
}
.viaduct-unread-badge {
  background-color: var(--c-grid);
  color: var(--c-fg);
  border-radius: 999px;
  padding: 0 7px;
  font-size: 80%;
}
row:selected .viaduct-unread-badge { background-color: var(--c-on-accent); color: var(--c-accent); }
.viaduct-row-read { opacity: 0.55; }
.viaduct-timeline-thumb { border-radius: 3px; }
.viaduct-avatar-image { border-radius: 999px; }
";

fn stylesheet_css(p: &Palette) -> String {
    let root = format!(
        ":root {{\n  --c-bg: {bg};\n  --c-bg-window: {bg_window};\n  --c-bg-header: {bg_header};\n  \
         --c-bg-card: {bg_card};\n  --c-fg: {fg};\n  --c-fg-dim: {fg_dim};\n  --c-heading: {heading};\n  \
         --c-accent: {accent};\n  --c-on-accent: {on_accent};\n  --c-grid: {grid};\n  \
         --c-warn: {warn};\n  --c-err: {err};\n  --c-ok: {ok};\n}}\n",
        bg = p.bg,
        bg_window = p.bg_window,
        bg_header = p.bg_header,
        bg_card = p.bg_card,
        fg = p.fg,
        fg_dim = p.fg_dim,
        heading = p.heading,
        accent = p.accent,
        on_accent = p.on_accent,
        grid = p.grid,
        warn = p.warn,
        err = p.err,
        ok = p.ok,
    );
    format!("{root}{STRUCTURE}")
}

thread_local! {
    static SHEET_PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// Install (or re-install) the owned stylesheet for the current dark/light
/// state, and keep following flips. Idempotent: removes the previous provider
/// before adding the new one. No-op with no display (headless dev).
pub fn install_stylesheet() {
    apply_stylesheet();
    connect_dark_changed(&global_owner(), |_| apply_stylesheet());
}

fn apply_stylesheet() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let palette = if is_dark() { &DRAGON } else { &LOTUS };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&stylesheet_css(palette));

    SHEET_PROVIDER.with(|slot| {
        if let Some(old) = slot.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
        *slot.borrow_mut() = Some(provider);
    });
}

/// Process-lifetime owner for the stylesheet's dark-changed listener; the GTK
/// Settings singleton outlives every window.
fn global_owner() -> gtk::Settings {
    gtk::Settings::default().expect("GtkSettings requires a display")
}
