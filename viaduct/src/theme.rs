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
    vir_gtk::portal::init(settings, Some("color-scheme"), false);
}

/// The app-owned sheet: Viaduct's deliberate overrides over vir-gtk's
/// `base_css` plus its own classes, written against the `--c-*` custom
/// properties (Viaduct is the one consumer on the var() mechanism; the
/// properties block is installed at the crate tier in [`apply`]). Everything
/// here either disagrees with the base (transparent lists, solid selection,
/// card-toned entries, painted destructive text) or is Viaduct-only.
const APP_SHEET: &str = "\
/* viaduct-specific overrides and classes over the vir-gtk base sheet. */
.title { font-weight: 700; }
.subtitle { color: var(--c-fg-dim); font-size: 90%; }
list, listview { background-color: transparent; }
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
row:selected .viaduct-unread-badge { background-color: var(--c-on-accent); color: var(--c-accent); }
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
