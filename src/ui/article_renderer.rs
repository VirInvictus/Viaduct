// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Neutered WebKit article renderer (Phase 6).
//!
//! Single `WebKitWebView` instance drives all article rendering. The settings
//! lockdown below is the ENTIRE security/memory story for the reader pane:
//!
//! - JavaScript: off (both runtime and HTML5 inline `<script>` markup).
//! - WebGL / WebRTC / plugins / DevTools: off.
//! - HTML5 LocalStorage / IndexedDB / app cache: off.
//! - Media playback: requires user gesture, no autoplay.
//! - Window-open from JS: off (belt-and-braces, since JS is already off).
//!
//! Themes are NetNewsWire `.nnwtheme` bundles ported byte-for-byte from
//! `.netnewswire/Shared/Resources/`. Each theme is a (`template.html`,
//! `stylesheet.css`) pair embedded at compile time via `include_str!`. The
//! outer page wrapper (`page.html`) inlines the theme CSS into a `<style>`
//! tag so WebKit doesn't need disk access for rendering — Phase 6 CSP will
//! then forbid all external resource loads except our `viaduct-img://`
//! scheme.
//!
//! NNW deviation: NNW uses native `WKWebView` (macOS/iOS) with a similar
//! lockdown profile in `WebViewController.applyConfiguration`. We translate
//! the same intent to WebKitGTK 6.0 via `webkit6::Settings`.

use std::collections::HashMap;
use webkit6::prelude::*;

/// Outer wrapper template — port of `.netnewswire/Mac/MainWindow/Detail/page.html`.
/// Substitutions: `[[title]]`, `[[style]]`, `[[baseURL]]`, `[[body]]`.
const PAGE_HTML: &str = include_str!("../../data/themes/page.html");

/// One bundled NetNewsWire theme. `template.html` is the inner article
/// shell (header / byline / body / footer); `stylesheet.css` is its CSS.
/// Both embed at compile time so production builds don't need disk access
/// to render an article.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Stable identifier used in GSettings and code. Lowercase, ASCII,
    /// underscores only — matches the `data/themes/<name>/` directory.
    pub id: &'static str,
    /// Display name shown to the user (matches NNW's Info.plist `Name`).
    pub display_name: &'static str,
    /// Inner template — the body that sits inside the page wrapper's
    /// `<body>[[body]]</body>`.
    pub template: &'static str,
    /// CSS to inline into the wrapper's `<style>[[style]]</style>`.
    pub stylesheet: &'static str,
    /// True for themes designed for dark backgrounds. `select_for_dark_mode`
    /// uses this to honor `adw::StyleManager::dark`.
    pub dark: bool,
}

/// All eight NNW themes ported in v1.1.0. Name and identifier match NNW's
/// `Info.plist` `Name` field; the bundle on disk lives at
/// `data/themes/<id>/`.
pub const THEMES: &[Theme] = &[
    Theme {
        id: "sepia",
        display_name: "Sepia",
        template: include_str!("../../data/themes/sepia/template.html"),
        stylesheet: include_str!("../../data/themes/sepia/stylesheet.css"),
        dark: false,
    },
    Theme {
        id: "appanoose",
        display_name: "Appanoose",
        template: include_str!("../../data/themes/appanoose/template.html"),
        stylesheet: include_str!("../../data/themes/appanoose/stylesheet.css"),
        dark: false,
    },
    Theme {
        id: "biblioteca",
        display_name: "Biblioteca",
        template: include_str!("../../data/themes/biblioteca/template.html"),
        stylesheet: include_str!("../../data/themes/biblioteca/stylesheet.css"),
        dark: false,
    },
    Theme {
        id: "hyperlegible",
        display_name: "Hyperlegible",
        template: include_str!("../../data/themes/hyperlegible/template.html"),
        stylesheet: include_str!("../../data/themes/hyperlegible/stylesheet.css"),
        dark: false,
    },
    Theme {
        id: "newsfax",
        display_name: "NewsFax",
        template: include_str!("../../data/themes/newsfax/template.html"),
        stylesheet: include_str!("../../data/themes/newsfax/stylesheet.css"),
        dark: false,
    },
    Theme {
        id: "promenade",
        display_name: "Promenade",
        template: include_str!("../../data/themes/promenade/template.html"),
        stylesheet: include_str!("../../data/themes/promenade/stylesheet.css"),
        dark: false,
    },
    Theme {
        id: "tiqoe_dark",
        display_name: "Tiqoe Dark",
        template: include_str!("../../data/themes/tiqoe_dark/template.html"),
        stylesheet: include_str!("../../data/themes/tiqoe_dark/stylesheet.css"),
        dark: true,
    },
    Theme {
        id: "verdana_revival",
        display_name: "Verdana Revival",
        template: include_str!("../../data/themes/verdana_revival/template.html"),
        stylesheet: include_str!("../../data/themes/verdana_revival/stylesheet.css"),
        dark: false,
    },
];

/// Look up a theme by id; falls back to Sepia when the id isn't recognized.
pub fn theme_by_id(id: &str) -> Theme {
    THEMES
        .iter()
        .find(|t| t.id == id)
        .copied()
        .unwrap_or(THEMES[0])
}

/// Choose a theme appropriate for the current libadwaita color scheme.
/// Sepia for light; Tiqoe Dark for dark. Phase 6 ships this hardcoded
/// pairing; v1.2.0 will expose a per-user theme picker via GSettings.
pub fn select_for_dark_mode(is_dark: bool) -> Theme {
    if is_dark {
        theme_by_id("tiqoe_dark")
    } else {
        theme_by_id("sepia")
    }
}

/// Article fields needed to render the NNW-shape inner template. All
/// strings are HTML-escaped by the caller before insertion (except `body`,
/// which is already-rendered HTML).
#[derive(Default, Debug, Clone)]
pub struct ArticleSubstitutions {
    pub title: String,
    pub body: String,
    pub preferred_link: String,
    pub feed_link: String,
    pub feed_link_title: String,
    pub byline: String,
    pub datetime_long: String,
    pub datetime_medium: String,
    pub datetime_short: String,
    pub date_long: String,
    pub date_medium: String,
    pub date_short: String,
    pub time_long: String,
    pub time_medium: String,
    pub time_short: String,
    /// `nnwImageIcon://<articleID>` in NNW; empty until Phase 6 wires the
    /// `viaduct-img://` URI scheme handler.
    pub avatar_src: String,
    pub external_link: String,
    pub external_link_label: String,
    pub external_link_stripped: String,
}

impl ArticleSubstitutions {
    /// Build the dictionary the macro processor expects. The keys match
    /// NNW's `articleSubstitutions()` exactly so the bundled themes work
    /// without modification.
    fn into_map(self) -> HashMap<&'static str, String> {
        let dateline_style = if self.title.is_empty() {
            "articleDatelineTitle"
        } else {
            "articleDateline"
        };
        let mut m = HashMap::with_capacity(20);
        m.insert("title", self.title);
        m.insert("body", self.body);
        m.insert("preferred_link", self.preferred_link);
        m.insert("feed_link", self.feed_link);
        m.insert("feed_link_title", self.feed_link_title);
        m.insert("byline", self.byline);
        m.insert("datetime_long", self.datetime_long);
        m.insert("datetime_medium", self.datetime_medium);
        m.insert("datetime_short", self.datetime_short);
        m.insert("date_long", self.date_long);
        m.insert("date_medium", self.date_medium);
        m.insert("date_short", self.date_short);
        m.insert("time_long", self.time_long);
        m.insert("time_medium", self.time_medium);
        m.insert("time_short", self.time_short);
        m.insert("avatar_src", self.avatar_src);
        m.insert("external_link", self.external_link);
        m.insert("external_link_label", self.external_link_label);
        m.insert("external_link_stripped", self.external_link_stripped);
        m.insert("dateline_style", dateline_style.to_string());
        // macOS-only in NNW; kept here as an empty class so the inner
        // template's `class="articleBody [[text_size_class]]"` doesn't
        // produce a literal `[[text_size_class]]` in the output.
        m.insert("text_size_class", String::new());
        m
    }
}

/// Linear `[[key]]` macro processor — port of NNW
/// `RSCore.MacroProcessor.processMacros()`. Unknown keys are left as
/// literal `[[key]]` per NNW behavior; the substitution map produced by
/// `ArticleSubstitutions::into_map` covers every key the bundled themes
/// reference, so unknowns only appear if a future theme introduces a new
/// macro that we haven't wired.
pub fn render_with_macros(template: &str, subs: &HashMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(template.len() + 256);
    let bytes = template.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 2 <= bytes.len() && &bytes[i..i + 2] == b"[[" {
            // Find matching ]] from i+2
            let rest = &bytes[i + 2..];
            if let Some(end_rel) = find_double_bracket_close(rest) {
                let key_bytes = &rest[..end_rel];
                if let Ok(key) = std::str::from_utf8(key_bytes)
                    && let Some(val) = subs.get(key)
                {
                    out.push_str(val);
                    i = i + 2 + end_rel + 2;
                    continue;
                }
                // Unknown key: emit literal `[[key]]` and skip past it.
                out.push_str("[[");
                out.push_str(std::str::from_utf8(key_bytes).unwrap_or(""));
                out.push_str("]]");
                i = i + 2 + end_rel + 2;
                continue;
            }
            // No closing `]]` — flush the rest verbatim and stop scanning.
            out.push_str(std::str::from_utf8(&bytes[i..]).unwrap_or(""));
            break;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn find_double_bracket_close(buf: &[u8]) -> Option<usize> {
    let mut j = 0;
    while j + 1 < buf.len() {
        if &buf[j..j + 2] == b"]]" {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// HTML-escape a string for use in macro substitution values. Used for
/// every field except `body` (which is already-rendered HTML) and URI
/// fields where attribute-context escaping is sufficient.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Wires the navigation-policy interceptor: every link click in the
/// article body routes to `xdg-open` instead of navigating the WebView
/// away from our rendered HTML. Without this hook the user can click the
/// feed link in the article header, lose the rendered article, and have
/// no way to return — there's no back/forward chrome on the pane.
///
/// Blocks `LinkClicked` and `FormSubmitted` navigations; allows `Other`
/// (the synthetic about:blank that backs `load_html`) and `Reload` /
/// `BackForward` (no-ops in our case since we never push history).
/// `NewWindowAction` and `NavigationAction` are handled the same way:
/// extract URL → `gio::AppInfo::launch_default_for_uri`.
///
/// Idempotent; safe to call once during window construction.
pub fn install_link_interceptor(view: &webkit6::WebView) {
    use webkit6::{NavigationType, PolicyDecisionType};

    view.connect_decide_policy(|_view, decision, decision_type| {
        // We only care about navigations and new-window requests. Response
        // policies (MIME-type display) hit our CSP; let those pass through.
        if !matches!(
            decision_type,
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
        ) {
            return false;
        }
        let Some(nav) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() else {
            return false;
        };
        let Some(mut action) = nav.navigation_action() else {
            return false;
        };
        let nav_type = action.navigation_type();

        // Allow the synthetic about:blank load that backs load_html().
        if matches!(
            nav_type,
            NavigationType::Other | NavigationType::Reload | NavigationType::BackForward
        ) {
            return false;
        }

        // Link clicks, form submits, NewWindow → system browser.
        if let Some(req) = action.request()
            && let Some(uri) = req.uri()
        {
            let uri_str = uri.to_string();
            // about:blank is what about:blank looks like to us — never
            // a real link, never worth shelling out for.
            if !uri_str.starts_with("about:") {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(
                    &uri_str,
                    gtk::gio::AppLaunchContext::NONE,
                );
            }
        }
        webkit6::prelude::PolicyDecisionExt::ignore(decision);
        true
    });
}

/// Applies the locked-down `WebKitSettings` profile to the supplied view.
/// Idempotent; safe to call repeatedly. Should be called once during window
/// construction before the first `render`.
pub fn apply_locked_down_settings(view: &webkit6::WebView) {
    // Disambiguate: GtkWidget also exposes a `settings()` (returns gtk::Settings).
    let Some(s) = webkit6::prelude::WebViewExt::settings(view) else {
        // WebKitGTK ships a default Settings on every WebView; this branch
        // is unreachable in practice. Bail rather than crashing.
        tracing::warn!("article_renderer: WebView has no Settings — leaving defaults");
        return;
    };
    s.set_enable_javascript(false);
    s.set_enable_javascript_markup(false);
    s.set_enable_webgl(false);
    s.set_enable_webrtc(false);
    s.set_enable_html5_local_storage(false);
    s.set_enable_html5_database(false);
    s.set_enable_offline_web_application_cache(false);
    s.set_enable_developer_extras(false);
    s.set_enable_back_forward_navigation_gestures(false);
    s.set_javascript_can_open_windows_automatically(false);
    s.set_media_playback_requires_user_gesture(true);
    s.set_enable_fullscreen(false);
    s.set_auto_load_images(true);
}

/// Render an article with the NNW page-wrapper + theme. Two-pass macro
/// substitution (matches NNW): inner pass fills the article fields into
/// the theme template; outer pass fills the result + the theme stylesheet
/// + the article's title and base URL into `page.html`.
pub fn render_themed(
    view: &webkit6::WebView,
    theme: Theme,
    subs: ArticleSubstitutions,
    base_uri: Option<&str>,
) {
    let title_for_outer = escape_html(&subs.title);
    let inner_subs = subs.into_map();
    let inner_html = render_with_macros(theme.template, &inner_subs);

    let mut outer_subs: HashMap<&'static str, String> = HashMap::with_capacity(4);
    outer_subs.insert("title", title_for_outer);
    outer_subs.insert("style", theme.stylesheet.to_string());
    outer_subs.insert(
        "baseURL",
        base_uri.map(|s| s.to_string()).unwrap_or_default(),
    );
    outer_subs.insert("body", inner_html);
    let final_html = render_with_macros(PAGE_HTML, &outer_subs);

    view.load_html(&final_html, base_uri);
}

/// Render plain HTML without a theme — used for the loading / no-selection
/// placeholder. Kept around in case future state needs a chrome-free pane.
#[allow(dead_code)]
pub fn render(view: &webkit6::WebView, html: &str, base_uri: Option<&str>) {
    view.load_html(html, base_uri);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_substitutes_known_keys() {
        let mut m = HashMap::new();
        m.insert("name", "Brandon".to_string());
        m.insert("place", "Toronto".to_string());
        let out = render_with_macros("Hello [[name]] from [[place]]!", &m);
        assert_eq!(out, "Hello Brandon from Toronto!");
    }

    #[test]
    fn macro_leaves_unknown_keys_as_literal() {
        let m = HashMap::new();
        let out = render_with_macros("Hello [[name]]!", &m);
        assert_eq!(out, "Hello [[name]]!");
    }

    #[test]
    fn macro_handles_unmatched_open() {
        let m = HashMap::new();
        let out = render_with_macros("Hello [[ no close", &m);
        assert_eq!(out, "Hello [[ no close");
    }

    #[test]
    fn macro_substitutes_adjacent_keys() {
        let mut m = HashMap::new();
        m.insert("a", "X".to_string());
        m.insert("b", "Y".to_string());
        assert_eq!(render_with_macros("[[a]][[b]]", &m), "XY");
    }

    #[test]
    fn html_escape_covers_amp_lt_gt_quotes() {
        assert_eq!(
            escape_html("a & b < c > d \"e\" 'f'"),
            "a &amp; b &lt; c &gt; d &quot;e&quot; &#39;f&#39;"
        );
    }

    #[test]
    fn all_eight_themes_load() {
        // Compile-time check — if any include_str! fails the build breaks.
        // This just asserts the registry shape didn't drift.
        assert_eq!(THEMES.len(), 8);
        for t in THEMES {
            assert!(!t.template.is_empty(), "{}: template empty", t.id);
            assert!(!t.stylesheet.is_empty(), "{}: stylesheet empty", t.id);
        }
        assert_eq!(theme_by_id("sepia").id, "sepia");
        assert_eq!(theme_by_id("nope").id, "sepia"); // fallback
    }
}
