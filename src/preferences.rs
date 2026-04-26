// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! GSettings-backed user preferences. Schema lives at
//! `data/org.virinvictus.Viaduct.gschema.xml`.
//!
//! Port of NNW's `AppDefaults` / `AppearancePreferencesView` for Linux.
//! NNW uses `UserDefaults` (NSUserDefaults); we use `gio::Settings`. Storage
//! is dconf (gsettings backend) — sandbox-friendly under Flatpak via
//! `org.freedesktop.portal.Settings`.

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

pub const SCHEMA_ID: &str = "org.virinvictus.Viaduct";

pub mod keys {
    pub const COLOR_SCHEME: &str = "color-scheme";
    pub const NOTIFICATIONS_ON_REFRESH: &str = "notifications-on-refresh";
    pub const RETENTION_DAYS: &str = "retention-days";
}

/// Open the user-visible preferences. Returns `None` when the schema isn't
/// installed (dev environment without `glib-compile-schemas`); callers fall
/// back to defaults.
pub fn settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(SCHEMA_ID, true)?;
    Some(gio::Settings::new(SCHEMA_ID))
}

/// Apply the color-scheme preference to the global `AdwStyleManager` and
/// keep it in sync when the user flips the dropdown later.
pub fn apply_color_scheme(settings: &gio::Settings) {
    let manager = adw::StyleManager::default();
    update_style_manager(settings, &manager);
    settings.connect_changed(
        Some(keys::COLOR_SCHEME),
        glib::clone!(
            #[weak]
            manager,
            move |s, _| update_style_manager(s, &manager)
        ),
    );
}

fn update_style_manager(settings: &gio::Settings, manager: &adw::StyleManager) {
    let nick = settings.string(keys::COLOR_SCHEME);
    let scheme = match nick.as_str() {
        "force-light" => adw::ColorScheme::ForceLight,
        "force-dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    };
    manager.set_color_scheme(scheme);
}

/// Whether desktop notifications fire after a refresh cycle delivering new
/// articles. Read fresh on each call so flips in the prefs dialog take
/// effect on the next refresh without restart.
pub fn notifications_enabled(settings: &gio::Settings) -> bool {
    settings.boolean(keys::NOTIFICATIONS_ON_REFRESH)
}

/// Article retention in days, used by the per-update prune in
/// `LocalAccount::update_feed`. Read fresh on each refresh so dialog
/// changes take effect on the next cycle without restart. Schema constrains
/// the value to `[1, 365]`; clamped here regardless to keep callers honest.
pub fn retention_days(settings: &gio::Settings) -> i64 {
    settings.int(keys::RETENTION_DAYS).clamp(1, 365) as i64
}
