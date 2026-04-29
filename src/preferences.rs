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
    pub const FONT_MONOSPACE: &str = "font-monospace";
    pub const FONT_SERIF: &str = "font-serif";
    pub const ARTICLE_THEME: &str = "article-theme";
}

/// Open the user-visible preferences. Returns `None` when the schema isn't
/// installed (dev environment without `glib-compile-schemas`); callers fall
/// back to defaults. Process-singleton: every call returns the same
/// `gio::Settings` GObject so signal handlers registered through one call
/// site remain connected even after that call site's stack frame returns.
/// Without this, `connect_changed` handlers attached to a transient
/// Settings instance get torn down with it and silently stop firing —
/// which is exactly how v1.2.0-pre1 shipped a non-functional theme picker.
///
/// Thread-local because `gio::Settings` is `!Send` (GObjects are bound to
/// the thread that created them). All viaduct callers are on the GTK main
/// thread, so a thread_local cell is the right shape.
pub fn settings() -> Option<gio::Settings> {
    use std::cell::OnceCell;
    thread_local! {
        static CELL: OnceCell<Option<gio::Settings>> = const { OnceCell::new() };
    }
    CELL.with(|cell| {
        cell.get_or_init(|| {
            let source = gio::SettingsSchemaSource::default()?;
            source.lookup(SCHEMA_ID, true)?;
            Some(gio::Settings::new(SCHEMA_ID))
        })
        .clone()
    })
}

/// Apply the color-scheme preference to the global `AdwStyleManager` and
/// keep it in sync when the user flips the dropdown later. The `settings`
/// argument is the process-singleton `gio::Settings` from `settings()`,
/// so signal handlers registered here outlive this call.
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

/// Apply typography overrides to the application using a global CSS provider.
/// Syncs live when settings change.
pub fn apply_fonts(settings: &gio::Settings) {
    let provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    update_fonts(settings, &provider);
    settings.connect_changed(
        Some(keys::FONT_SERIF),
        glib::clone!(
            #[weak]
            provider,
            move |s, _| update_fonts(s, &provider)
        ),
    );
    settings.connect_changed(
        Some(keys::FONT_MONOSPACE),
        glib::clone!(
            #[weak]
            provider,
            move |s, _| update_fonts(s, &provider)
        ),
    );
}

fn update_fonts(settings: &gio::Settings, provider: &gtk::CssProvider) {
    let mut css = String::from(
        "window { font-family: \"Inter\", system-ui, sans-serif; }
        #article_text_view { font-size: 17px; line-height: 1.7; }
        #article_title_label { font-size: 32px; font-weight: 800; letter-spacing: -0.02em; margin-bottom: 8px; }
        #article_meta_label { font-size: 14px; font-weight: 500; letter-spacing: 0.01em; }
        ",
    );

    let serif = font_serif(settings);
    if !serif.is_empty() {
        css.push_str(&format!(
            "#article_text_view {{ font-family: \"{}\", serif; }}\n",
            serif
        ));
    } else {
        css.push_str(
            "#article_text_view { font-family: \"Source Serif 4\", \"Georgia\", serif; }\n",
        );
    }

    let mono = font_monospace(settings);
    if !mono.is_empty() {
        css.push_str(&format!(
            "code, pre {{ font-family: \"{}\", monospace; }}\n",
            mono
        ));
    } else {
        css.push_str("code, pre { font-family: \"JetBrains Mono\", monospace; }\n");
    }

    provider.load_from_string(&css);
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
/// `Account::update_feed`. Read fresh on each refresh so dialog
/// changes take effect on the next cycle without restart. Schema constrains
/// the value to `[1, 365]`; clamped here regardless to keep callers honest.
pub fn retention_days(settings: &gio::Settings) -> i64 {
    settings.int(keys::RETENTION_DAYS).clamp(1, 365) as i64
}

/// Monospace font family override for the article body. Empty string means
/// "use system/app default".
pub fn font_monospace(settings: &gio::Settings) -> String {
    settings.string(keys::FONT_MONOSPACE).to_string()
}

/// Serif font family override for the article body. Empty string means
/// "use system/app default".
pub fn font_serif(settings: &gio::Settings) -> String {
    settings.string(keys::FONT_SERIF).to_string()
}

/// User's chosen article theme. Schema enum nick (e.g. "auto", "sepia",
/// "tiqoe-dark"). The dash form matches the GSettings convention; our
/// internal `Theme::id` uses underscores, so callers feeding this into
/// `article_renderer::theme_by_id` must convert dashes to underscores.
pub fn article_theme_nick(settings: &gio::Settings) -> String {
    settings.string(keys::ARTICLE_THEME).to_string()
}

/// Resolve the current article theme: explicit selection wins; "auto"
/// pairs Sepia (light) with Tiqoe Dark (dark) like the v1.1.0 default.
/// `is_dark` should be `adw::StyleManager::default().is_dark()` taken
/// fresh on the GTK thread.
pub fn resolve_article_theme(
    settings: &gio::Settings,
    is_dark: bool,
) -> crate::ui::article_renderer::Theme {
    let nick = article_theme_nick(settings);
    if nick == "auto" || nick.is_empty() {
        return crate::ui::article_renderer::select_for_dark_mode(is_dark);
    }
    // Schema nicks use dashes (`tiqoe-dark`) but Theme::id uses
    // underscores (`tiqoe_dark`) to match the data/themes/ directory
    // names. Translate before lookup.
    let id = nick.replace('-', "_");
    crate::ui::article_renderer::theme_by_id(&id)
}

/// Apply the article theme's accent color to the GTK chrome via a
/// CSS provider on the default `gdk::Display`. Reads the current setting
/// fresh, resolves through `resolve_article_theme`, and pushes the hex
/// to `article_renderer::apply_app_accent`. Connects a notify handler so
/// the accent re-applies when the user flips the dropdown later — no
/// restart needed.
pub fn apply_article_theme_accent(settings: &gio::Settings) {
    let manager = adw::StyleManager::default();
    refresh_accent(settings, &manager);
    settings.connect_changed(
        Some(keys::ARTICLE_THEME),
        glib::clone!(
            #[weak]
            manager,
            move |s, _| refresh_accent(s, &manager)
        ),
    );
    // Dark-mode toggles can flip the chosen theme in "auto" mode, so
    // re-apply the accent whenever the color scheme actually flips.
    manager.connect_dark_notify(glib::clone!(
        #[weak]
        settings,
        move |m| refresh_accent(&settings, m)
    ));
}

fn refresh_accent(settings: &gio::Settings, manager: &adw::StyleManager) {
    let theme = resolve_article_theme(settings, manager.is_dark());
    tracing::debug!(
        nick = %article_theme_nick(settings),
        resolved_id = theme.id,
        accent = theme.accent_hex,
        is_dark = manager.is_dark(),
        "preferences: refresh_accent"
    );
    crate::ui::article_renderer::apply_app_accent(theme.accent_hex);
}
