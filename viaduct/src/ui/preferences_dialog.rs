// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Preferences dialog. Port of NNW's `AppearancePreferencesView` and the
//! "General" tab of NSPreferencesWindow.
//!
//! Phase 20c: a plain modal `gtk::Window` over `ui::rows`, replacing the
//! `AdwPreferencesDialog` / `Page` / `Group` / `ComboRow` / `SwitchRow` /
//! `EntryRow` set. Escape-to-close is explicit (`ui::close_on_escape`);
//! the adw sheet gave it for free.
//!
//! For v0.8.0 we expose color-scheme follow + new-article notifications.
//! Other schema keys (refresh interval, retention days, font overrides) are
//! reserved for future phases that wire them into behavior.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::preferences::keys;
use crate::ui::rows;
use crate::ui::window::ViaductWindow;

/// Build and present the preferences dialog. When the schema isn't installed
/// (dev environment without `glib-compile-schemas` having run), the dialog
/// renders a single explanatory row and the toggles remain inert.
pub fn present(parent: &ViaductWindow) {
    let (appearance, appearance_list) = rows::group(Some("Appearance"), None);
    let (typography, typography_list) = rows::group(
        Some("Typography"),
        Some(
            "Override the font family for each surface. Empty = use system default. Type a family name (e.g. \"Atkinson Hyperlegible\") exactly as installed.",
        ),
    );
    let (sync, sync_list) = rows::group(
        Some("Sync"),
        Some(
            "Automatic refresh on startup, on a periodic schedule, and optionally while the window is closed.",
        ),
    );
    let (notifications, notifications_list) = rows::group(Some("Notifications"), None);
    let (playback, playback_list) = rows::group(Some("Video playback"), None);

    if let Some(settings) = crate::preferences::settings() {
        appearance_list.append(&color_scheme_row(&settings));
        appearance_list.append(&article_theme_row(&settings));
        typography_list.append(&font_row(
            &settings,
            keys::FONT_UI,
            "App font",
            "Sidebar, timeline, header bars, dialogs.",
        ));
        typography_list.append(&font_row(
            &settings,
            keys::FONT_SERIF,
            "Reading font",
            "Article body in the reading pane. Layered after the article theme.",
        ));
        typography_list.append(&font_row(
            &settings,
            keys::FONT_MONOSPACE,
            "Monospace font",
            "Code and pre blocks (article pane + chrome).",
        ));
        sync_list.append(&refresh_on_startup_row(&settings));
        sync_list.append(&refresh_interval_row(&settings));
        sync_list.append(&run_in_background_row(&settings, parent));
        notifications_list.append(&notifications_row(&settings));
        playback_list.append(&video_playback_row(&settings));
    } else {
        appearance_list.append(&rows::row(
            "Settings unavailable",
            Some("GSettings schema isn’t installed. Run `glib-compile-schemas data/` and retry."),
            None,
        ));
    }

    // Phase 20c: a plain modal window rather than an in-window
    // `adw::PreferencesDialog` sheet. `AdwPreferencesPage` scrolled and
    // clamped its groups for us, so both are explicit here.
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    for group in [&appearance, &typography, &sync, &notifications, &playback] {
        content.append(group);
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    outer.append(&gtk::HeaderBar::new());
    outer.append(&scroller);

    let window = gtk::Window::builder()
        .title("Preferences")
        .transient_for(parent)
        .modal(true)
        .default_width(600)
        .default_height(700)
        .child(&outer)
        .build();
    crate::ui::close_on_escape(&window);
    window.present();
}

fn color_scheme_row(settings: &gio::Settings) -> gtk::ListBoxRow {
    let (row, drop_down) = rows::combo_row(
        "Color scheme",
        Some("Follow the system theme or force light/dark."),
        &["Follow system", "Force light", "Force dark"],
    );
    drop_down.set_selected(nick_to_index(&settings.string(keys::COLOR_SCHEME)));

    let settings_for_row = settings.clone();
    drop_down.connect_selected_notify(move |drop_down| {
        let nick = index_to_nick(drop_down.selected());
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
            drop_down,
            move |s, _| {
                let nick = s.string(keys::COLOR_SCHEME);
                drop_down.set_selected(nick_to_index(&nick));
            }
        ),
    );

    row
}

/// Theme picker for the article reading pane. The dropdown lists "Follow
/// color scheme" plus all 8 NNW-ported themes; the selected theme's
/// accent color also propagates app-wide via
/// `preferences::apply_article_theme_accent`.
fn article_theme_row(settings: &gio::Settings) -> gtk::ListBoxRow {
    use crate::ui::article_renderer::THEMES;

    // Index 0 = Auto; subsequent indices match THEMES[i-1].
    let mut labels = vec!["Follow color scheme".to_string()];
    labels.extend(THEMES.iter().map(|t| t.display_name.to_string()));
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

    let (row, drop_down) = rows::combo_row(
        "Article theme",
        Some("Reading pane typography. Accent color propagates app-wide."),
        &label_refs,
    );
    drop_down.set_selected(theme_nick_to_index(&settings.string(keys::ARTICLE_THEME)));

    let settings_for_row = settings.clone();
    drop_down.connect_selected_notify(move |drop_down| {
        let nick = theme_index_to_nick(drop_down.selected());
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
            drop_down,
            move |s, _| {
                let nick = s.string(keys::ARTICLE_THEME);
                drop_down.set_selected(theme_nick_to_index(&nick));
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
fn video_playback_row(settings: &gio::Settings) -> gtk::ListBoxRow {
    let (row, drop_down) = rows::combo_row(
        "Video playback",
        Some("How YouTube / Vimeo videos play when one is detected on an article."),
        &[
            "Play in app (sandboxed)",
            "Open in default handler",
            "Don’t show play button",
        ],
    );
    drop_down.set_selected(video_mode_nick_to_index(
        &settings.string(keys::VIDEO_PLAYBACK_MODE),
    ));

    let settings_for_row = settings.clone();
    drop_down.connect_selected_notify(move |drop_down| {
        let nick = video_mode_index_to_nick(drop_down.selected());
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
            drop_down,
            move |s, _| {
                let nick = s.string(keys::VIDEO_PLAYBACK_MODE);
                drop_down.set_selected(video_mode_nick_to_index(&nick));
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

// v2.6.21: free-form font-family entry row. Each of the three
// font-override GSettings (font-ui, font-serif for the reading pane,
// font-monospace) wires through this same helper. The text is bound
// bidirectionally to the GSetting; empty string means "use default",
// which the CSS pipeline in preferences::apply_fonts and
// article_renderer::render_themed reads as "skip the override rule
// entirely". A free-text row rather than gtk::FontDialogButton because
// the font picker yields a full pango::FontDescription (family + size
// + weight + style) and we only persist the family name; round-tripping
// through the picker would be lossy and awkward.
fn font_row(
    settings: &gio::Settings,
    key: &'static str,
    title: &str,
    subtitle: &str,
) -> gtk::ListBoxRow {
    // `adw::EntryRow` floated the title inside the entry; a plain entry
    // cannot, so the title sits left and the subtitle stays a subtitle.
    let (row, entry) = rows::entry_row(title, Some(subtitle), Some("empty = default"));
    settings.bind(key, &entry, "text").build();
    row
}

fn notifications_row(settings: &gio::Settings) -> gtk::ListBoxRow {
    let (row, switch) = rows::switch_row(
        "New article notifications",
        Some("Show a desktop notification after each refresh that fetches new articles."),
    );
    settings
        .bind(keys::NOTIFICATIONS_ON_REFRESH, &switch, "active")
        .build();
    row
}

fn refresh_on_startup_row(settings: &gio::Settings) -> gtk::ListBoxRow {
    let (row, switch) = rows::switch_row(
        "Sync feeds when viaduct opens",
        Some("Run a refresh cycle automatically when the main window is shown."),
    );
    settings
        .bind(keys::REFRESH_ON_STARTUP, &switch, "active")
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

fn refresh_interval_row(settings: &gio::Settings) -> gtk::ListBoxRow {
    let labels: Vec<&str> = REFRESH_INTERVAL_OPTIONS.iter().map(|(_, l)| *l).collect();
    let (row, drop_down) = rows::combo_row(
        "Sync feeds periodically",
        Some("How often viaduct refreshes while running."),
        &labels,
    );
    drop_down.set_selected(refresh_interval_to_index(
        settings.int(keys::REFRESH_INTERVAL_MINUTES),
    ));

    let settings_for_row = settings.clone();
    drop_down.connect_selected_notify(move |drop_down| {
        let value = REFRESH_INTERVAL_OPTIONS
            .get(drop_down.selected() as usize)
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
            drop_down,
            move |s, _| {
                drop_down.set_selected(refresh_interval_to_index(
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
fn run_in_background_row(settings: &gio::Settings, parent: &ViaductWindow) -> gtk::ListBoxRow {
    let (row, switch) = rows::switch_row(
        "Keep refreshing after the window is closed",
        Some(
            "viaduct hides the window on close instead of quitting, so periodic refresh keeps running. The first time you enable this, the desktop will ask for permission.",
        ),
    );
    settings
        .bind(keys::RUN_IN_BACKGROUND, &switch, "active")
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
