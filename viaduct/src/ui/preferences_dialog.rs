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
use crate::ui::window::ViaductWindow;

/// Build and present the preferences dialog. When the schema isn't installed
/// (dev environment without `glib-compile-schemas` having run), the dialog
/// renders a single explanatory row and the toggles remain inert.
pub fn present(parent: &ViaductWindow) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .build();
    let page = adw::PreferencesPage::new();
    page.set_title("General");
    page.set_icon_name(Some("preferences-system-symbolic"));

    let appearance = adw::PreferencesGroup::new();
    appearance.set_title("Appearance");
    page.add(&appearance);

    let sync = adw::PreferencesGroup::new();
    sync.set_title("Sync");
    sync.set_description(Some(
        "Automatic refresh on startup, on a periodic schedule, and optionally while the window is closed.",
    ));
    page.add(&sync);

    let notifications = adw::PreferencesGroup::new();
    notifications.set_title("Notifications");
    page.add(&notifications);

    let playback = adw::PreferencesGroup::new();
    playback.set_title("Video playback");
    page.add(&playback);

    if let Some(settings) = crate::preferences::settings() {
        appearance.add(&color_scheme_row(&settings));
        appearance.add(&article_theme_row(&settings));
        sync.add(&refresh_on_startup_row(&settings));
        sync.add(&refresh_interval_row(&settings));
        sync.add(&run_in_background_row(&settings, parent));
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

fn refresh_on_startup_row(settings: &gio::Settings) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title("Sync feeds when viaduct opens")
        .subtitle("Run a refresh cycle automatically when the main window is shown.")
        .build();
    settings
        .bind(keys::REFRESH_ON_STARTUP, &row, "active")
        .build();
    row
}

/// Periodic-refresh interval picker. Discrete options map to integer
/// minute values stored in `refresh-interval-minutes` (0 = disabled).
const REFRESH_INTERVAL_OPTIONS: &[(i32, &str)] = &[
    (0, "Never"),
    (15, "Every 15 minutes"),
    (30, "Every 30 minutes"),
    (60, "Every hour"),
    (120, "Every 2 hours"),
    (360, "Every 6 hours"),
    (1440, "Once a day"),
];

fn refresh_interval_row(settings: &gio::Settings) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title("Sync feeds periodically")
        .subtitle("How often viaduct refreshes while running.")
        .build();

    let labels: Vec<&str> = REFRESH_INTERVAL_OPTIONS.iter().map(|(_, l)| *l).collect();
    let model = gtk::StringList::new(&labels);
    row.set_model(Some(&model));
    row.set_selected(refresh_interval_to_index(
        settings.int(keys::REFRESH_INTERVAL_MINUTES),
    ));

    let settings_for_row = settings.clone();
    row.connect_selected_notify(move |row| {
        let value = REFRESH_INTERVAL_OPTIONS
            .get(row.selected() as usize)
            .map(|(v, _)| *v)
            .unwrap_or(0);
        if settings_for_row.int(keys::REFRESH_INTERVAL_MINUTES) != value
            && let Err(e) = settings_for_row.set_int(keys::REFRESH_INTERVAL_MINUTES, value)
        {
            tracing::warn!(?e, "failed to write refresh-interval-minutes");
        }
    });

    settings.connect_changed(
        Some(keys::REFRESH_INTERVAL_MINUTES),
        glib::clone!(
            #[weak]
            row,
            move |s, _| {
                row.set_selected(refresh_interval_to_index(
                    s.int(keys::REFRESH_INTERVAL_MINUTES),
                ));
            }
        ),
    );

    row
}

/// Switch row for `run-in-background`. The bind itself is straightforward;
/// the wrinkle is that on flip-to-true we need to ask the desktop for the
/// `org.freedesktop.portal.Background` grant. On portal denial we flip the
/// switch back off and toast the parent window so the user understands why.
/// On non-Flatpak installs the portal call is a no-op and always returns
/// `true`, so the toggle just works.
fn run_in_background_row(settings: &gio::Settings, parent: &ViaductWindow) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title("Keep refreshing after the window is closed")
        .subtitle(
            "viaduct hides the window on close instead of quitting, so periodic refresh keeps running. The first time you enable this, the desktop will ask for permission.",
        )
        .build();
    settings
        .bind(keys::RUN_IN_BACKGROUND, &row, "active")
        .build();

    // Fire the portal request when the GSetting flips to true, regardless
    // of whether the change came from this switch or from elsewhere
    // (dconf-editor, another viaduct window). On denial we set the
    // GSetting back to false; the bind syncs the switch off automatically
    // and connect_changed re-fires with `false` (no-op for us).
    settings.connect_changed(
        Some(keys::RUN_IN_BACKGROUND),
        glib::clone!(
            #[weak]
            parent,
            move |s, _| {
                if !s.boolean(keys::RUN_IN_BACKGROUND) {
                    return;
                }
                let settings_for_response = s.clone();
                let (tx, rx) = tokio::sync::oneshot::channel();
                crate::spawn_on_runtime(async move {
                    let result =
                        crate::network::background::request_background_permission().await;
                    let _ = tx.send(result);
                });
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    parent,
                    async move {
                        let response = rx.await;
                        let granted = matches!(response, Ok(Ok(true)));
                        if granted {
                            return;
                        }
                        if let Err(e) = settings_for_response.set_boolean(
                            keys::RUN_IN_BACKGROUND,
                            false,
                        ) {
                            tracing::warn!(?e, "failed to clear run-in-background after portal denial");
                        }
                        let message = match response {
                            Ok(Ok(false)) => {
                                "Background permission denied. Enable it in your system settings to run viaduct in the background."
                            }
                            _ => {
                                "Could not request background permission. Disabled."
                            }
                        };
                        parent.show_toast_public(message);
                    }
                ));
            }
        ),
    );

    row
}

/// Pick the closest discrete dropdown index for an arbitrary minute
/// value. Values that don't exactly match a preset (e.g. someone set
/// it to `45` via `dconf-editor`) snap to the nearest representable
/// option so the dropdown still shows something coherent.
fn refresh_interval_to_index(minutes: i32) -> u32 {
    if let Some(idx) = REFRESH_INTERVAL_OPTIONS
        .iter()
        .position(|(v, _)| *v == minutes)
    {
        return idx as u32;
    }
    // No exact match — find the option whose value is closest.
    REFRESH_INTERVAL_OPTIONS
        .iter()
        .enumerate()
        .min_by_key(|(_, (v, _))| (v - minutes).abs())
        .map(|(i, _)| i as u32)
        .unwrap_or(0)
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
