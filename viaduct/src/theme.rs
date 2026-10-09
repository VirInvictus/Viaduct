// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use gtk::prelude::*;

pub fn system_is_dark() -> bool {
    vir_gtk::portal::system_is_dark()
}
pub fn is_dark() -> bool {
    vir_gtk::portal::is_dark()
}
pub fn connect_dark_changed<F>(owner: &impl IsA<gtk::glib::Object>, f: F)
where
    F: Fn(bool) + 'static,
{
    vir_gtk::portal::connect_dark_changed(owner, f)
}

pub fn init(settings: Option<gtk::gio::Settings>) {
    // Positional args: (portal settings key, default_dark). The `false`
    // is the recorded no-portal default = light (spec.md §12.3; the
    // recorded divergence from the Colophon pilot, which degrades dark).
    vir_gtk::portal::init(settings, Some("color-scheme"), false);
}

/// The app-owned sheet: Viaduct's deliberate overrides over vir-gtk's
/// `base_css` plus its own classes, written against the `--c-*` custom
/// properties (Viaduct is the one consumer on the var() mechanism; the
/// properties block is installed at the crate tier in [`apply`]). Everything
/// here either disagrees with the base (transparent lists, lifted selection,
/// card-toned entries, painted destructive text) or is Viaduct-only.
const APP_SHEET: &str = "\
/* viaduct-specific overrides and classes over the vir-gtk base sheet. */
.title { font-weight: 700; }
.subtitle { color: var(--c-fg-dim); font-size: 90%; }
list, listview { background-color: transparent; }
row.activatable:hover { background-color: var(--c-grid); }
/* The lifted selection (Conservatory's idiom): a selected row raises to
 * the raised surface and gains a 2px accent edge on its leading side. A
 * solid dragonRed wash read as permanent garish chrome, not a highlight. */
row:selected {
  background-color: var(--c-bg-raised);
  color: var(--c-fg);
  box-shadow: inset 2px 0 0 var(--c-accent);
}
row:selected label { color: var(--c-fg); }
listview > row { transition: background-color 150ms ease; }
/* TreeExpander indent/expander geometry: foreign GTK themes size these
 * builtin icons differently (a theme without a rule leaves them at the
 * 16px initial instead of Default's 8px, doubling every indent level and
 * starving row titles of width). Own the Default-theme values. */
treeexpander { border-spacing: 4px; }
treeexpander indent { -gtk-icon-size: 8px; }
entry, spinbutton {
  background-color: var(--c-bg-card);
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  border-radius: 0;
  box-shadow: none;
  min-height: 24px;
}
button.destructive-action {
  background-color: var(--c-err);
  color: var(--c-bg-window);
  border-color: var(--c-err);
}
.toast {
  background-color: var(--c-bg-card);
  color: var(--c-fg);
  border: 1px solid var(--c-grid);
  padding: 6px 12px;
}
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
.viaduct-row-read { opacity: 0.55; }
.viaduct-timeline-thumb { border-radius: 3px; }
.viaduct-avatar-image { border-radius: 999px; }
";

fn global_owner() -> gtk::Settings {
    gtk::Settings::default().expect("GtkSettings requires a display")
}

/// Install the crate tier (the shared base sheet plus the custom-properties
/// block this sheet's var() references consume) and the app tier, and
/// re-apply both on a resolved dark/light flip.
pub fn install_stylesheet() {
    apply();
    connect_dark_changed(&global_owner(), |_| apply());
}

fn apply() {
    let palette = if is_dark() {
        vir_gtk::theme::Palette::dragon()
    } else {
        vir_gtk::theme::Palette::lotus()
    };
    let crate_tier = format!(
        "{}{}",
        vir_gtk::theme::base_css(&palette),
        palette.to_css_custom_properties()
    );
    vir_gtk::theme::install_stylesheet(&crate_tier);
    vir_gtk::theme::install_app_stylesheet(APP_SHEET);
}
