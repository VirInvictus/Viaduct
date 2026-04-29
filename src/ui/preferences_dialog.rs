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

    let playback = adw::PreferencesGroup::new();
    playback.set_title("Video playback");
    page.add(&playback);

    if let Some(settings) = crate::preferences::settings() {
        appearance.add(&color_scheme_row(&settings));
        appearance.add(&article_theme_row(&settings));
        notifications.add(&notifications_row(&settings));
        playback.add(&video_playback_row(&settings));
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

/// Theme picker for the article reading pane. The dropdown lists "Follow
/// color scheme" plus all 8 NNW-ported themes; the selected theme's
/// accent color also propagates app-wide via
/// `preferences::apply_article_theme_accent`.
fn article_theme_row(settings: &gio::Settings) -> adw::ComboRow {
    use crate::ui::article_renderer::THEMES;

    let row = adw::ComboRow::builder()
        .title("Article theme")
        .subtitle("Reading pane typography. Accent color propagates app-wide.")
        .build();

    // Index 0 = Auto; subsequent indices match THEMES[i-1].
    let mut labels = vec!["Follow color scheme".to_string()];
    labels.extend(THEMES.iter().map(|t| t.display_name.to_string()));
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let model = gtk::StringList::new(&label_refs);
    row.set_model(Some(&model));
    row.set_selected(theme_nick_to_index(&settings.string(keys::ARTICLE_THEME)));

    let settings_for_row = settings.clone();
    row.connect_selected_notify(move |row| {
        let nick = theme_index_to_nick(row.selected());
        if settings_for_row.string(keys::ARTICLE_THEME).as_str() != nick
            && let Err(e) = settings_for_row.set_string(keys::ARTICLE_THEME, nick)
        {
            tracing::warn!(?e, "failed to write article-theme setting");
        }
    });

    // External flips (e.g. dconf-editor) sync back to the dropdown.
    settings.connect_changed(
        Some(keys::ARTICLE_THEME),
        glib::clone!(
            #[weak]
            row,
            move |s, _| {
                let nick = s.string(keys::ARTICLE_THEME);
                row.set_selected(theme_nick_to_index(&nick));
            }
        ),
    );

    row
}

fn theme_nick_to_index(nick: &str) -> u32 {
    use crate::ui::article_renderer::THEMES;
    if nick == "auto" || nick.is_empty() {
        return 0;
    }
    let id_form = nick.replace('-', "_");
    match THEMES.iter().position(|t| t.id == id_form) {
        Some(i) => (i as u32) + 1,
        None => 0,
    }
}

fn theme_index_to_nick(index: u32) -> &'static str {
    use crate::ui::article_renderer::THEMES;
    if index == 0 {
        return "auto";
    }
    let i = (index as usize).saturating_sub(1);
    if i >= THEMES.len() {
        return "auto";
    }
    // Map the underscore form back to the schema's dash form.
    match THEMES[i].id {
        "adwaita" => "adwaita",
        "sepia" => "sepia",
        "appanoose" => "appanoose",
        "biblioteca" => "biblioteca",
        "hyperlegible" => "hyperlegible",
        "newsfax" => "newsfax",
        "promenade" => "promenade",
        "tiqoe_dark" => "tiqoe-dark",
        "verdana_revival" => "verdana-revival",
        _ => "auto",
    }
}

/// Picker for how YouTube / Vimeo videos play when detected on an article.
/// In-pane spawns a transient WebKit dialog; External hands off to the
/// system handler (xdg-open / mpv / browser); Disabled hides the play
/// button entirely.
fn video_playback_row(settings: &gio::Settings) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title("Video playback")
        .subtitle("How YouTube / Vimeo videos play when one is detected on an article.")
        .build();

    let model = gtk::StringList::new(&[
        "Play in app (sandboxed)",
        "Open in default handler",
        "Don’t show play button",
    ]);
    row.set_model(Some(&model));
    row.set_selected(video_mode_nick_to_index(
        &settings.string(keys::VIDEO_PLAYBACK_MODE),
    ));

    let settings_for_row = settings.clone();
    row.connect_selected_notify(move |row| {
        let nick = video_mode_index_to_nick(row.selected());
        if settings_for_row.string(keys::VIDEO_PLAYBACK_MODE).as_str() != nick
            && let Err(e) = settings_for_row.set_string(keys::VIDEO_PLAYBACK_MODE, nick)
        {
            tracing::warn!(?e, "failed to write video-playback-mode setting");
        }
    });

    settings.connect_changed(
        Some(keys::VIDEO_PLAYBACK_MODE),
        glib::clone!(
            #[weak]
            row,
            move |s, _| {
                let nick = s.string(keys::VIDEO_PLAYBACK_MODE);
                row.set_selected(video_mode_nick_to_index(&nick));
            }
        ),
    );

    row
}

fn video_mode_nick_to_index(nick: &str) -> u32 {
    match nick {
        "external" => 1,
        "disabled" => 2,
        _ => 0,
    }
}

fn video_mode_index_to_nick(index: u32) -> &'static str {
    match index {
        1 => "external",
        2 => "disabled",
        _ => "in-pane",
    }
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
