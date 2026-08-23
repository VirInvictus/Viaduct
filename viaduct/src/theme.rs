use gtk::prelude::*;
use std::cell::RefCell;

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

thread_local! {
    static SHEET_PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

pub fn install_stylesheet() {
    apply_stylesheet();
    connect_dark_changed(&global_owner(), |_| apply_stylesheet());
}

fn apply_stylesheet() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let palette = if is_dark() {
        vir_gtk::theme::Palette::dragon()
    } else {
        vir_gtk::theme::Palette::lotus()
    };

    let css = format!("{}{}", palette.to_css_custom_properties(), STRUCTURE);
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);

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

fn global_owner() -> gtk::Settings {
    gtk::Settings::default().expect("GtkSettings requires a display")
}
