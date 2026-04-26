// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Preferences dialog. Port of NNW's `AppearancePreferencesView` and the
//! "General" tab of NSPreferencesWindow into a single `AdwPreferencesDialog`.
//!
//! For v0.8.0 we expose color-scheme follow + new-article notifications.
//! Other schema keys (refresh interval, retention days, font overrides) are
//! reserved for future phases that wire them into behavior.

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::preferences::keys;

/// Build and present the preferences dialog. When the schema isn't installed
/// (dev environment without `glib-compile-schemas` having run), the dialog
/// renders a single explanatory row and the toggles remain inert.
pub fn present(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .build();
    let page = adw::PreferencesPage::new();
    page.set_title("General");
    page.set_icon_name(Some("preferences-system-symbolic"));

    let appearance = adw::PreferencesGroup::new();
    appearance.set_title("Appearance");
    page.add(&appearance);

    let notifications = adw::PreferencesGroup::new();
    notifications.set_title("Notifications");
    page.add(&notifications);

    if let Some(settings) = crate::preferences::settings() {
        appearance.add(&color_scheme_row(&settings));
        notifications.add(&notifications_row(&settings));
    } else {
        let warn = adw::ActionRow::new();
        warn.set_title("Settings unavailable");
        warn.set_subtitle(
            "GSettings schema isn’t installed. Run `glib-compile-schemas data/` and retry.",
        );
        appearance.add(&warn);
    }

    dialog.add(&page);
    dialog.present(Some(parent));
}

fn color_scheme_row(settings: &gio::Settings) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title("Color scheme")
        .subtitle("Follow the system theme or force light/dark.")
        .build();

    let model = gtk::StringList::new(&["Follow system", "Force light", "Force dark"]);
    row.set_model(Some(&model));
    row.set_selected(nick_to_index(&settings.string(keys::COLOR_SCHEME)));

    let settings_for_row = settings.clone();
    row.connect_selected_notify(move |row| {
        let nick = index_to_nick(row.selected());
        if settings_for_row.string(keys::COLOR_SCHEME).as_str() != nick
            && let Err(e) = settings_for_row.set_string(keys::COLOR_SCHEME, nick)
        {
            tracing::warn!(?e, "failed to write color-scheme setting");
        }
    });

    // External flips (e.g. dconf-editor) sync back to the dropdown.
    settings.connect_changed(
        Some(keys::COLOR_SCHEME),
        glib::clone!(
            #[weak]
            row,
            move |s, _| {
                let nick = s.string(keys::COLOR_SCHEME);
                row.set_selected(nick_to_index(&nick));
            }
        ),
    );

    row
}

fn notifications_row(settings: &gio::Settings) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title("New article notifications")
        .subtitle("Show a desktop notification after each refresh that fetches new articles.")
        .build();
    settings
        .bind(keys::NOTIFICATIONS_ON_REFRESH, &row, "active")
        .build();
    row
}

fn nick_to_index(nick: &str) -> u32 {
    match nick {
        "force-light" => 1,
        "force-dark" => 2,
        _ => 0,
    }
}

fn index_to_nick(index: u32) -> &'static str {
    match index {
        1 => "force-light",
        2 => "force-dark",
        _ => "default",
    }
}
