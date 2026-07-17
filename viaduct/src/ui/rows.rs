// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Shared plain-GTK list rows and groups: the `adw::ActionRow` /
//! `ComboRow` / `SwitchRow` / `EntryRow` / `SpinRow` / `PreferencesGroup`
//! replacements (Phase 20c, spec.md §12.5).
//!
//! Ported from the Colophon pilot's `rows.rs`, which shipped only `row`
//! and `value_row`; viaduct uses four row flavours the pilot never did, so
//! they extend the same shape rather than inventing another. Each builder
//! returns the row **and** its control, because the caller always needs the
//! control to bind a GSetting to it, and digging it back out of the widget
//! tree would be worse than handing it over.
//!
//! The rows are deliberately plain `GtkListBoxRow`s in a `.boxed-list`
//! `GtkListBox`, which is the class name adwaita used, so the owned
//! stylesheet (20d) can define it without touching any call site.

use gtk::pango;
use gtk::prelude::*;

/// A non-activatable list row: title over an optional dim subtitle, with
/// an optional trailing control. Long labels ellipsize, and the subtitle
/// carries itself as its own tooltip so nothing is lost to the cut.
pub fn row(title: &str, subtitle: Option<&str>, suffix: Option<&gtk::Widget>) -> gtk::ListBoxRow {
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let title_label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    text.append(&title_label);
    if let Some(subtitle) = subtitle.filter(|s| !s.is_empty()) {
        let subtitle_label = gtk::Label::builder()
            .label(subtitle)
            .xalign(0.0)
            .wrap(false)
            .ellipsize(pango::EllipsizeMode::End)
            .tooltip_text(subtitle)
            .css_classes(["caption", "dim-label"])
            .build();
        text.append(&subtitle_label);
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&text);
    if let Some(suffix) = suffix {
        suffix.set_valign(gtk::Align::Center);
        content.append(suffix);
    }
    gtk::ListBoxRow::builder()
        .activatable(false)
        .child(&content)
        .build()
}

/// A row whose trailing control is a button, activatable by clicking the
/// row as well as the button (the `adw::ActionRow` +
/// `set_activatable_widget` idiom).
pub fn button_row(title: &str, subtitle: Option<&str>, button: &gtk::Button) -> gtk::ListBoxRow {
    let row = row(title, subtitle, Some(button.clone().upcast_ref()));
    row.set_activatable(true);
    let button = button.clone();
    // A GtkListBoxRow does not forward activation to its child, so mirror
    // what set_activatable_widget did for us.
    row.connect_activate(move |_| button.emit_clicked());
    row
}

/// `adw::SwitchRow`. The switch is returned so the caller can bind its
/// `active` property to a GSetting.
pub fn switch_row(title: &str, subtitle: Option<&str>) -> (gtk::ListBoxRow, gtk::Switch) {
    let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    let row = row(title, subtitle, Some(switch.clone().upcast_ref()));
    (row, switch)
}

/// `adw::ComboRow`. The dropdown is returned so the caller can set its
/// selection and watch `notify::selected`.
pub fn combo_row(
    title: &str,
    subtitle: Option<&str>,
    options: &[&str],
) -> (gtk::ListBoxRow, gtk::DropDown) {
    let model = gtk::StringList::new(options);
    let drop_down = gtk::DropDown::builder()
        .model(&model)
        .valign(gtk::Align::Center)
        .build();
    let row = row(title, subtitle, Some(drop_down.clone().upcast_ref()));
    (row, drop_down)
}

/// `adw::EntryRow`. adwaita drew the title *inside* the entry as a
/// floating label; a plain entry cannot, so the title sits to the left
/// like every other row here and the placeholder carries the hint.
pub fn entry_row(
    title: &str,
    subtitle: Option<&str>,
    placeholder: Option<&str>,
) -> (gtk::ListBoxRow, gtk::Entry) {
    let entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    if let Some(placeholder) = placeholder {
        entry.set_placeholder_text(Some(placeholder));
    }
    let row = row(title, subtitle, Some(entry.clone().upcast_ref()));
    (row, entry)
}

/// `adw::SpinRow`. The spin button is returned so the caller can bind its
/// `value` property, which is the same property name `adw::SpinRow` bound.
pub fn spin_row(
    title: &str,
    subtitle: Option<&str>,
    min: f64,
    max: f64,
    step: f64,
) -> (gtk::ListBoxRow, gtk::SpinButton) {
    let spin = gtk::SpinButton::with_range(min, max, step);
    spin.set_valign(gtk::Align::Center);
    let row = row(title, subtitle, Some(spin.clone().upcast_ref()));
    (row, spin)
}

/// `adw::PreferencesGroup`: a heading, an optional dim description, and a
/// `.boxed-list` `GtkListBox` to append rows to. Returns the outer box to
/// pack and the list to fill.
///
/// The list uses `SelectionMode::None`, because adwaita's groups were not
/// selectable and a stray selection highlight on a settings row reads as a
/// bug.
pub fn group(title: Option<&str>, description: Option<&str>) -> (gtk::Box, gtk::ListBox) {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    if let Some(title) = title {
        outer.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
    }
    if let Some(description) = description {
        outer.append(
            &gtk::Label::builder()
                .label(description)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
    }
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    outer.append(&list);
    (outer, list)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row builders are pure widget construction, so the useful thing
    /// to pin is the shape the stylesheet and the callers depend on.
    fn init() -> bool {
        gtk::init().is_ok()
    }

    #[test]
    fn group_list_is_a_non_selectable_boxed_list() {
        if !init() {
            return;
        }
        let (_, list) = group(Some("Title"), None);
        assert!(list.has_css_class("boxed-list"));
        assert_eq!(list.selection_mode(), gtk::SelectionMode::None);
    }

    #[test]
    fn rows_are_not_activatable_unless_they_act() {
        if !init() {
            return;
        }
        assert!(!row("Title", None, None).is_activatable());
        let button = gtk::Button::new();
        assert!(button_row("Title", None, &button).is_activatable());
    }

    #[test]
    fn spin_row_hands_back_a_bindable_control() {
        if !init() {
            return;
        }
        let (_, spin) = spin_row("Text Size", None, 75.0, 200.0, 5.0);
        spin.set_value(125.0);
        assert_eq!(spin.value(), 125.0);
    }
}
