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
//! tag so WebKit doesn't need disk access for rendering.
//!
//! ### Network sandbox: `viaduct-img://` + CSP
//!
//! WebKit gets ZERO direct internet access. The page wrapper carries a
//! strict Content-Security-Policy (`default-src 'none'`) that whitelists
//! only inline styles and images from a custom URI scheme — `viaduct-img:`.
//! Every `<img src="https://…">` in incoming feed HTML is rewritten to
//! `viaduct-img://i/<percent-encoded-original>`; the registered scheme
//! handler routes the lookup through our `ImageCache` (memory LRU → disk
//! cache → network). End result: WebKit can render images, but every byte
//! travels through our cache, and no other origin (script, font, frame,
//! analytics beacon) can load anything.
//!
//! NNW deviation: NNW uses native `WKWebView` (macOS/iOS) with a similar
//! lockdown profile in `WebViewController.applyConfiguration` and registers
//! `nnwImageIcon://` for article-icon URLs. We translate the same intent
//! to WebKitGTK 6.0 via `webkit6::Settings` + `WebContext::register_uri_scheme`.

use crate::network::ImageCache;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use webkit6::prelude::*;

/// Outer wrapper template — port of `.netnewswire/Mac/MainWindow/Detail/page.html`.
/// Substitutions: `[[title]]`, `[[style]]`, `[[baseURL]]`, `[[body]]`.
const PAGE_HTML: &str = include_str!("../../../data/themes/page.html");

/// CSS appended after every theme stylesheet so the WebKitWebView in the
/// article pane behaves correctly inside the GtkOverlay container that
/// hosts it. Each NNW `.nnwtheme` ships `html { overflow: hidden;
/// ::-webkit-scrollbar { display: none; }` because NNW's `WKWebView` is
/// wrapped in a parent `NSScrollView` that owns scrolling. We removed
/// the equivalent `GtkScrolledWindow` in v1.1.0-pre1.6 (it was
/// silently clipping long articles), so WebKit needs to scroll itself.
/// This override re-enables overflow + restores the scrollbar so long
/// articles are reachable. Last in the cascade, so theme stylesheets
/// can't unset it accidentally.
const VIADUCT_PANE_OVERRIDE_CSS: &str = "\
html, body {\n\
  overflow: auto !important;\n\
  height: auto !important;\n\
}\n\
/* v2.0.0-pre6: thinner scrollbar driven by `currentColor` so the\n\
 * thumb adopts the page's text color (which respects\n\
 * `prefers-color-scheme` already) — closer to libadwaita's overlay\n\
 * scrollbar feel than the v1.x hard-coded gray. The `width` jumps\n\
 * from 6 px to 10 px on hover so the target stays grabbable. */\n\
::-webkit-scrollbar {\n\
  display: initial !important;\n\
  width: 6px;\n\
  height: 6px;\n\
  transition: width 120ms ease, height 120ms ease;\n\
}\n\
::-webkit-scrollbar:hover {\n\
  width: 10px;\n\
  height: 10px;\n\
}\n\
::-webkit-scrollbar-thumb {\n\
  background-color: color-mix(in srgb, currentColor 30%, transparent);\n\
  border-radius: 999px;\n\
  transition: background-color 120ms ease;\n\
}\n\
::-webkit-scrollbar-thumb:hover {\n\
  background-color: color-mix(in srgb, currentColor 55%, transparent);\n\
}\n\
::-webkit-scrollbar-track {\n\
  background: transparent;\n\
}\n\
/* v2.0.0-pre6: 150 ms article-to-article fade-in. Matches the\n\
 * outer `article_stack` content/empty crossfade so within-content\n\
 * swaps no longer feel like an instant jump-cut. The animation\n\
 * runs on every `WebKitWebView::load_html` since each load is a\n\
 * fresh page render. */\n\
@keyframes viaduct-fade-in {\n\
  from { opacity: 0; }\n\
  to   { opacity: 1; }\n\
}\n\
body {\n\
  animation: viaduct-fade-in 150ms ease-out;\n\
}\n\
/* v2.3.0: user-controlled font scale + line height. The font\n\
 * scale ratio is multiplied against the body's base font-size so\n\
 * theme rules expressed in em/rem scale proportionally. The\n\
 * line-height variable is unitless and used directly — the\n\
 * GSettings default of 1.5 matches most NNW themes' native\n\
 * value, and `inherit` covers descendants automatically. */\n\
body {\n\
  font-size: calc(1em * var(--article-font-scale, 1));\n\
  line-height: var(--article-line-height, 1.5);\n\
}\n\
";

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
    /// Hex color (`#rrggbb`) applied app-wide as `@define-color
    /// accent_bg_color` / `accent_color` so the GTK chrome (sidebar
    /// selection, focus rings, switches, buttons) visually echoes the
    /// theme of the article pane. Pulled from each NNW stylesheet's
    /// most distinctive accent. `None` means "don't override" — the
    /// Adwaita theme uses this so GNOME's system accent surfaces
    /// unchanged through the chrome.
    pub accent_hex: Option<&'static str>,
    /// Hand-tuned `@media (prefers-color-scheme: dark)` overlay that
    /// sits AFTER `stylesheet` in the cascade — the original NNW
    /// stylesheet stays byte-perfect, our dark adaptation adds on top.
    /// `None` for themes that handle dark mode internally (Adwaita
    /// has prefers-color-scheme baked in) or are already dark-only
    /// (Tiqoe Dark).
    pub dark_overlay: Option<&'static str>,
}

/// All eight NNW themes ported in v1.1.0. Name and identifier match NNW's
/// `Info.plist` `Name` field; the bundle on disk lives at
/// `data/themes/<id>/`.
pub const THEMES: &[Theme] = &[
    Theme {
        id: "adwaita",
        display_name: "Adwaita",
        template: include_str!("../../../data/themes/adwaita/template.html"),
        stylesheet: include_str!("../../../data/themes/adwaita/stylesheet.css"),
        dark: false,
        accent_hex: None,
        dark_overlay: None, // Adwaita stylesheet has prefers-color-scheme baked in
    },
    Theme {
        id: "sepia",
        display_name: "Sepia",
        template: include_str!("../../../data/themes/sepia/template.html"),
        stylesheet: include_str!("../../../data/themes/sepia/stylesheet.css"),
        dark: false,
        accent_hex: Some("#7a4d1f"),
        dark_overlay: Some(include_str!("../../../data/themes/sepia/dark.css")),
    },
    Theme {
        id: "appanoose",
        display_name: "Appanoose",
        template: include_str!("../../../data/themes/appanoose/template.html"),
        stylesheet: include_str!("../../../data/themes/appanoose/stylesheet.css"),
        dark: false,
        accent_hex: Some("#086aee"),
        dark_overlay: Some(include_str!("../../../data/themes/appanoose/dark.css")),
    },
    Theme {
        id: "biblioteca",
        display_name: "Biblioteca",
        template: include_str!("../../../data/themes/biblioteca/template.html"),
        stylesheet: include_str!("../../../data/themes/biblioteca/stylesheet.css"),
        dark: false,
        accent_hex: Some("#1145a5"),
        dark_overlay: Some(include_str!("../../../data/themes/biblioteca/dark.css")),
    },
    Theme {
        id: "hyperlegible",
        display_name: "Hyperlegible",
        template: include_str!("../../../data/themes/hyperlegible/template.html"),
        stylesheet: include_str!("../../../data/themes/hyperlegible/stylesheet.css"),
        dark: false,
        accent_hex: Some("#086aee"),
        dark_overlay: Some(include_str!("../../../data/themes/hyperlegible/dark.css")),
    },
    Theme {
        id: "newsfax",
        display_name: "NewsFax",
        template: include_str!("../../../data/themes/newsfax/template.html"),
        stylesheet: include_str!("../../../data/themes/newsfax/stylesheet.css"),
        dark: false,
        accent_hex: Some("#3a3a3a"),
        dark_overlay: Some(include_str!("../../../data/themes/newsfax/dark.css")),
    },
    Theme {
        id: "promenade",
        display_name: "Promenade",
        template: include_str!("../../../data/themes/promenade/template.html"),
        stylesheet: include_str!("../../../data/themes/promenade/stylesheet.css"),
        dark: false,
        accent_hex: Some("#086aee"),
        dark_overlay: Some(include_str!("../../../data/themes/promenade/dark.css")),
    },
    Theme {
        id: "tiqoe_dark",
        display_name: "Tiqoe Dark",
        template: include_str!("../../../data/themes/tiqoe_dark/template.html"),
        stylesheet: include_str!("../../../data/themes/tiqoe_dark/stylesheet.css"),
        dark: true,
        accent_hex: Some("#b08660"),
        dark_overlay: None, // already dark
    },
    Theme {
        id: "verdana_revival",
        display_name: "Verdana Revival",
        template: include_str!("../../../data/themes/verdana_revival/template.html"),
        stylesheet: include_str!("../../../data/themes/verdana_revival/stylesheet.css"),
        dark: false,
        accent_hex: Some("#2670c4"),
        dark_overlay: Some(include_str!(
            "../../../data/themes/verdana_revival/dark.css"
        )),
    },
];

/// Install a CSS provider on the default `gdk::Display` overriding
/// libadwaita's accent throughout the GTK chrome to match the chosen
/// article theme. Replaces any prior provider so theme switches are
/// instant (no app restart). `None` removes the active override
/// entirely so GNOME's system accent surfaces unchanged — used by
/// the Adwaita theme to feel like stock GNOME.
///
/// libadwaita 1.7 propagates GNOME's system accent (e.g. the
/// `org.gnome.desktop.interface accent-color` GSetting) via internal
/// style channels that win against generic `@define-color` overrides at
/// any priority. To beat that we register at
/// `STYLE_PROVIDER_PRIORITY_USER + 100` AND target the highest-traffic
/// accent-coloured widgets by selector — sidebar selection, focus
/// rings, switches, checks, suggested-action buttons, links.
///
/// `@define-color` redirects are kept for stylesheets that consult the
/// named-color cascade directly (rare in libadwaita 1.7+ but free
/// insurance).
pub fn apply_app_accent(hex: Option<&str>) {
    use std::cell::RefCell;
    thread_local! {
        static ACTIVE_PROVIDER: RefCell<Option<gtk::CssProvider>> = const {
            RefCell::new(None)
        };
    }
    let Some(display) = gtk::gdk::Display::default() else {
        tracing::warn!("article_renderer: no default Gdk display — accent skipped");
        return;
    };
    // No-override path: pull our active provider so GNOME's system
    // accent (or whatever the user has set) re-takes the cascade.
    let Some(hex) = hex else {
        ACTIVE_PROVIDER.with(|cell| {
            if let Some(old) = cell.borrow_mut().take() {
                gtk::style_context_remove_provider_for_display(&display, &old);
            }
        });
        return;
    };
    // Beat libadwaita's system-accent integration (which sits at USER
    // priority on GNOME 47+).
    const ACCENT_PRIORITY: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER + 100;

    // Pick the accent's contrasting foreground by WCAG contrast ratio.
    // White is correct for most of our shipped themes (Sepia, the blue
    // family, NewsFax — all dark accents). Tiqoe Dark's warm tan
    // `#b08660` is the outlier: white on it gives ~3.4:1 (fails AA),
    // near-black gives ~4.9:1 (passes AA). Without this picker, the
    // selected timeline row would be unreadable on Tiqoe Dark in dark
    // mode — same class of bug Brandon reported in Sepia/v1.5.4.
    let fg = pick_accent_fg(hex);
    let css = format!(
        // 1. Legacy @define-color cascade — read by older stylesheets
        //    and any custom widget that asks for the named colors.
        "@define-color accent_bg_color {hex};\n\
         @define-color accent_color {hex};\n\
         @define-color accent_fg_color {fg};\n\
         \n\
         /* 2. CSS custom properties — libadwaita 1.7's primary path. */\n\
         :root {{\n\
           --accent-bg-color: {hex};\n\
           --accent-color: {hex};\n\
           --accent-fg-color: {fg};\n\
         }}\n\
         \n\
         /* 3. Selector-targeted overrides for the highest-visibility\n\
          *    accent surfaces. These win unconditionally because GTK\n\
          *    matches them on widget paths and we sit at higher\n\
          *    priority than libadwaita's accent integration. */\n\
         \n\
         /* Suggested-action / accent buttons. */\n\
         button.suggested-action,\n\
         button.accent {{\n\
           background-color: {hex};\n\
           color: {fg};\n\
         }}\n\
         \n\
         /* Toggles and check states. */\n\
         switch:checked > slider {{ background-color: {fg}; }}\n\
         switch:checked {{ background-color: {hex}; }}\n\
         checkbutton check:checked,\n\
         checkbutton radio:checked,\n\
         menu check:checked,\n\
         menu radio:checked {{ background-color: {hex}; color: {fg}; }}\n\
         \n\
         /* List & sidebar selection — the single biggest visual cue.\n\
          * Use FULL-saturation accent for the background and white for\n\
          * the foreground, mirroring GNOME's stock convention. The\n\
          * previous 20%-alpha background + accent foreground produced\n\
          * accent-on-accent-tinted-dark in dark mode (Sepia + dark mode\n\
          * was the worst offender, where `#7a4d1f` text on a 20% Sepia\n\
          * tinted dark base gave ~1.6:1 contrast — illegible). */\n\
         listview > row:selected,\n\
         listview > row:selected:focus,\n\
         listview > row:selected:hover,\n\
         row.activatable:selected,\n\
         row.activatable:selected:focus,\n\
         row.activatable:selected:hover {{\n\
           background-color: {hex};\n\
           color: {fg};\n\
         }}\n\
         /* Override label-level styling inside a selected row so every\n\
          * child label paints against the accent background. The .heading\n\
          * class on titles inherits color naturally; .dim-label and our\n\
          * .viaduct-row-read class both apply opacity that needs to be\n\
          * partially neutralised so the user can read a selected item\n\
          * clearly even when it's marked read. */\n\
         listview > row:selected label,\n\
         row.activatable:selected label {{\n\
           color: {fg};\n\
         }}\n\
         /* Bump dim-label's default opacity (0.55) up to 0.85 inside a\n\
          * selected row — preserves hierarchy (preview vs. title) but\n\
          * stays solidly legible on the accent fill. */\n\
         listview > row:selected .dim-label,\n\
         row.activatable:selected .dim-label {{\n\
           opacity: 0.85;\n\
         }}\n\
         /* Read-row dimming (`viaduct-row-read`, opacity 0.55) is\n\
          * actively unhelpful on a selected row — the user is looking\n\
          * AT the selected item, so render it at full strength. */\n\
         listview > row:selected .viaduct-row-read,\n\
         row.activatable:selected .viaduct-row-read {{\n\
           opacity: 1;\n\
         }}\n\
         \n\
         /* Text selection. */\n\
         selection {{ background-color: alpha({hex}, 0.35); color: inherit; }}\n\
         \n\
         /* Focus rings. */\n\
         :focus-visible {{ outline-color: {hex}; }}\n\
         \n\
         /* Link buttons / inline links. */\n\
         button.link, button.link:hover {{ color: {hex}; }}\n\
         \n\
         /* AdwAvatar accent fallback when no custom-image is set. */\n\
         avatar.accent {{ background-color: alpha({hex}, 0.30); color: {hex}; }}\n",
    );

    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    ACTIVE_PROVIDER.with(|cell| {
        if let Some(old) = cell.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        gtk::style_context_add_provider_for_display(&display, &provider, ACCENT_PRIORITY);
        *cell.borrow_mut() = Some(provider);
    });
}

/// Pick a high-contrast foreground (white or near-black) for an accent
/// background. Compares WCAG contrast against both candidates and
/// returns whichever wins. Falls back to white on parse failure so a
/// malformed hex never produces invisible text.
///
/// Test vectors:
/// - `#7a4d1f` (Sepia, dark cinnamon) → `#ffffff` (8.4:1 vs 1.8:1)
/// - `#086aee` (the blue-family accents) → `#ffffff` (~7:1)
/// - `#b08660` (Tiqoe Dark's warm tan) → `#1d1d1d` (~4.9:1 vs 3.4:1)
/// - `#3a3a3a` (NewsFax) → `#ffffff` (~12:1)
fn pick_accent_fg(hex: &str) -> &'static str {
    const WHITE: &str = "#ffffff";
    const DARK: &str = "#1d1d1d";
    let Some(lum) = relative_luminance(hex) else {
        return WHITE;
    };
    // WCAG contrast: (L_lighter + 0.05) / (L_darker + 0.05)
    let contrast_with_white = 1.05 / (lum + 0.05);
    // Near-black #1d1d1d has linear-luminance ≈ 0.0119; using 0.012
    // matches WCAG enough for the picker. Hardcoded so we don't need
    // a second relative_luminance call on the constant.
    let contrast_with_dark = (lum + 0.05) / (0.012 + 0.05);
    if contrast_with_white >= contrast_with_dark {
        WHITE
    } else {
        DARK
    }
}

/// WCAG relative luminance of a `#rrggbb` hex string. Returns None for
/// any unparseable input. The formula applies the sRGB gamma decoding
/// per channel (the `<= 0.03928` branch of the WCAG 2.x spec) before
/// the `0.2126·R + 0.7152·G + 0.0722·B` weighting.
fn relative_luminance(hex: &str) -> Option<f64> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;

    fn channel(c: u8) -> f64 {
        let s = c as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }

    Some(0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b))
}

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

/// Wires `mouse-target-changed` so the hovered link's URL surfaces in the
/// supplied label widget. Hide the label when the cursor isn't over a
/// link. Idempotent; safe to call once per WebView. Port of the same
/// behavior that NewsFlash's `UrlOverlay` provides — implemented from
/// scratch with a `gtk::Label` overlay child rather than reusing their
/// custom widget.
pub fn install_hover_url_overlay(view: &webkit6::WebView, overlay_label: &gtk::Label) {
    let label = overlay_label.clone();
    view.connect_mouse_target_changed(move |_view, hit_test, _modifiers| {
        if hit_test.context_is_link()
            && let Some(uri) = hit_test.link_uri()
        {
            label.set_text(uri.as_str());
            label.set_visible(true);
        } else {
            label.set_visible(false);
        }
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

/// Custom URI scheme served by our `ImageCache`. Article HTML's
/// `<img src="https://…">` attributes are rewritten to
/// `viaduct-img://i/<percent-encoded-original>` before WebKit sees them;
/// the scheme handler decodes and routes through cache → disk → network.
pub const VIADUCT_IMG_SCHEME: &str = "viaduct-img";
const VIADUCT_IMG_PREFIX: &str = "viaduct-img://i/";

/// Custom URI scheme served by the bundled font registry below. Each
/// `@font-face` rule in the article CSS references a `viaduct-font://`
/// URL; the scheme handler returns the embedded TTF bytes.
pub const VIADUCT_FONT_SCHEME: &str = "viaduct-font";

/// Hardcoded registry of fonts bundled into the binary via `include_bytes!`.
/// Keyed by `(family, variant)` where variant is one of `regular` / `bold` /
/// `italic` / `bolditalic`. Adding a new font: drop the TTF in
/// `data/fonts/<family>/`, add four `include_bytes!` lines below, and
/// declare the four `@font-face` rules in `font_face_css()`.
struct FontEntry {
    family: &'static str,
    variant: &'static str,
    bytes: &'static [u8],
}

const BUNDLED_FONTS: &[FontEntry] = &[
    FontEntry {
        family: "atkinson",
        variant: "regular",
        bytes: include_bytes!("../../../data/fonts/atkinson/AtkinsonHyperlegibleNext-Regular.ttf"),
    },
    FontEntry {
        family: "atkinson",
        variant: "bold",
        bytes: include_bytes!("../../../data/fonts/atkinson/AtkinsonHyperlegibleNext-Bold.ttf"),
    },
    FontEntry {
        family: "atkinson",
        variant: "italic",
        bytes: include_bytes!("../../../data/fonts/atkinson/AtkinsonHyperlegibleNext-Italic.ttf"),
    },
    FontEntry {
        family: "atkinson",
        variant: "bolditalic",
        bytes: include_bytes!(
            "../../../data/fonts/atkinson/AtkinsonHyperlegibleNext-BoldItalic.ttf"
        ),
    },
];

/// `@font-face` declarations prepended to every theme stylesheet so
/// `font-family: 'Atkinson Hyperlegible'` and friends resolve through
/// our bundled TTFs even when the system doesn't have them installed.
/// Themes that don't reference these family names simply ignore the
/// declarations — `@font-face` only loads on demand.
fn font_face_css() -> &'static str {
    "\
@font-face {\n\
  font-family: 'Atkinson Hyperlegible';\n\
  src: url('viaduct-font://atkinson/regular') format('truetype');\n\
  font-weight: normal;\n\
  font-style: normal;\n\
  font-display: swap;\n\
}\n\
@font-face {\n\
  font-family: 'Atkinson Hyperlegible';\n\
  src: url('viaduct-font://atkinson/bold') format('truetype');\n\
  font-weight: bold;\n\
  font-style: normal;\n\
  font-display: swap;\n\
}\n\
@font-face {\n\
  font-family: 'Atkinson Hyperlegible';\n\
  src: url('viaduct-font://atkinson/italic') format('truetype');\n\
  font-weight: normal;\n\
  font-style: italic;\n\
  font-display: swap;\n\
}\n\
@font-face {\n\
  font-family: 'Atkinson Hyperlegible';\n\
  src: url('viaduct-font://atkinson/bolditalic') format('truetype');\n\
  font-weight: bold;\n\
  font-style: italic;\n\
  font-display: swap;\n\
}\n\
"
}

fn lookup_bundled_font(uri: &str) -> Option<&'static [u8]> {
    let rest = uri.strip_prefix("viaduct-font://")?;
    // rest looks like "atkinson/regular"
    let (family, variant) = rest.split_once('/')?;
    BUNDLED_FONTS
        .iter()
        .find(|f| f.family == family && f.variant == variant)
        .map(|f| f.bytes)
}

/// Percent-encode every byte that isn't an unreserved RFC 3986 character.
/// Used for stuffing arbitrary URLs into our scheme's path component.
pub(crate) fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for &b in input.as_bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            let high = b >> 4;
            let low = b & 0x0F;
            out.push(hex_digit(high));
            out.push(hex_digit(low));
        }
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => '0',
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let high = decode_hex(bytes[i + 1])?;
            let low = decode_hex(bytes[i + 2])?;
            out.push((high << 4) | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn decode_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn encode_image_url(original: &str) -> String {
    format!("{VIADUCT_IMG_PREFIX}{}", percent_encode(original))
}

pub fn decode_image_url(viaduct_url: &str) -> Option<String> {
    viaduct_url
        .strip_prefix(VIADUCT_IMG_PREFIX)
        .and_then(percent_decode)
}

/// Sniff a content-type from the first few bytes of an image. Sufficient
/// for the formats WebKit cares about; falls back to
/// `application/octet-stream` so WebKit's own MIME detection takes over.
fn sniff_image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        return "image/svg+xml";
    }
    "application/octet-stream"
}

/// Run sanitization + img-src rewrite on a raw article body. Built every
/// call (cheap) instead of lazily so the closure capturing
/// `image_cache` stays scoped to the active window. The ammonia builder
/// adds `viaduct-img` to the URL allowlist so our rewritten attribute
/// survives the second-pass URL validation.
pub fn sanitize_and_rewrite_image_srcs(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    builder.add_url_schemes(&[VIADUCT_IMG_SCHEME]);
    builder.attribute_filter(|element, attribute, value| {
        if element.eq_ignore_ascii_case("img")
            && attribute.eq_ignore_ascii_case("src")
            && (value.starts_with("http://") || value.starts_with("https://"))
        {
            return Some(Cow::Owned(encode_image_url(value)));
        }
        Some(Cow::Borrowed(value))
    });
    builder.clean(html).to_string()
}

/// Register the `viaduct-img://` URI scheme on the default `WebContext`
/// so every WebView in the process resolves image URLs through our
/// `ImageCache` instead of the network. Idempotent within a process; the
/// last registration wins on a re-call. Must be invoked on the GTK main
/// thread.
/// Register `viaduct-font://` so the `@font-face` rules in
/// `font_face_css()` resolve through our bundled TTFs. Sync handler
/// (font bytes are baked into the binary via `include_bytes!` so no
/// async work is needed). Idempotent.
/// Register the `viaduct-font://` scheme on the supplied `WebContext`.
/// v2.0.0-pre4: was previously hard-wired to `WebContext::default()`;
/// the new `ArticleRenderer` GObject creates its own per-renderer context
/// so the scheme can't leak between hypothetical future windows.
pub fn install_font_uri_scheme(ctx: &webkit6::WebContext) {
    ctx.register_uri_scheme(VIADUCT_FONT_SCHEME, |request| {
        let uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
        match lookup_bundled_font(&uri) {
            Some(bytes) => {
                let len = bytes.len() as i64;
                // Bytes::from_static — no allocation, the static slice
                // lives forever.
                let g_bytes = glib::Bytes::from_static(bytes);
                let stream = gio::MemoryInputStream::from_bytes(&g_bytes);
                request.finish(&stream, len, Some("font/ttf"));
            }
            None => {
                tracing::warn!(%uri, "viaduct-font: not in bundled registry");
                let mut err = glib::Error::new(gio::IOErrorEnum::NotFound, "font not bundled");
                request.finish_error(&mut err);
            }
        }
    });
}

/// Register the `viaduct-img://` scheme on the supplied `WebContext`.
/// Same per-renderer rationale as `install_font_uri_scheme`.
pub fn install_image_uri_scheme(ctx: &webkit6::WebContext, image_cache: Arc<ImageCache>) {
    ctx.register_uri_scheme(VIADUCT_IMG_SCHEME, move |request| {
        // The URISchemeRequest is a !Send GObject; clone the Rc-style
        // reference so we can keep it alive across the async fetch and
        // call finish() on the GTK main thread when bytes arrive.
        let request = request.clone();
        let cache = image_cache.clone();
        let uri = request.uri().map(|s| s.to_string()).unwrap_or_default();
        tracing::debug!(%uri, "viaduct-img: scheme request");

        glib::spawn_future_local(async move {
            let Some(original_url) = decode_image_url(&uri) else {
                tracing::warn!(%uri, "viaduct-img: malformed scheme URI");
                let mut err = glib::Error::new(
                    gio::IOErrorEnum::InvalidArgument,
                    "malformed viaduct-img URI",
                );
                request.finish_error(&mut err);
                return;
            };

            // Hop to tokio for the cache lookup (memory → disk → network).
            // Image cache traffic must NOT block the GTK main loop.
            let (tx, rx) = tokio::sync::oneshot::channel::<Option<Vec<u8>>>();
            let cache_for_task = cache.clone();
            let url_for_task = original_url.clone();
            crate::spawn_on_runtime(async move {
                let bytes = cache_for_task.image(&url_for_task).await;
                let _ = tx.send(bytes);
            });

            match rx.await {
                Ok(Some(bytes)) => {
                    let mime = sniff_image_mime(&bytes);
                    let len = bytes.len() as i64;
                    let g_bytes = glib::Bytes::from_owned(bytes);
                    let stream = gio::MemoryInputStream::from_bytes(&g_bytes);
                    request.finish(&stream, len, Some(mime));
                }
                _ => {
                    tracing::debug!(%original_url, "viaduct-img: cache miss");
                    let mut err =
                        glib::Error::new(gio::IOErrorEnum::NotFound, "image not in viaduct cache");
                    request.finish_error(&mut err);
                }
            }
        });
    });
}

/// Render an article with the NNW page-wrapper + theme. Two-pass macro
/// substitution (matches NNW): inner pass fills the article fields into
/// the theme template; outer pass fills the result, the theme stylesheet,
/// and the article's title and base URL into `page.html`. The body is
/// run through `sanitize_and_rewrite_image_srcs` first so external `img`
/// URLs become `viaduct-img://` references and CSP can lock the pane
/// down to our scheme alone.
pub fn render_themed(
    view: &webkit6::WebView,
    theme: Theme,
    mut subs: ArticleSubstitutions,
    base_uri: Option<&str>,
) {
    subs.body = sanitize_and_rewrite_image_srcs(&subs.body);
    let title_for_outer = escape_html(&subs.title);
    let inner_subs = subs.into_map();
    let inner_html = render_with_macros(theme.template, &inner_subs);

    let mut outer_subs: HashMap<&'static str, String> = HashMap::with_capacity(4);
    outer_subs.insert("title", title_for_outer);

    // v2.0.0-pre6: effective accent — themes with a hard-coded
    // `accent_hex` (Sepia / Biblioteca / etc.) keep theirs; themes
    // with `accent_hex: None` (Adwaita) pick up the libadwaita
    // system accent (GNOME 47+ gnome-control-center setting).
    //
    // v2.3.0: also inject the user's article-font-scale and
    // article-line-height multipliers (read from GSettings, clamped
    // to schema range, expressed as fractions). The override stylesheet
    // applies them via `font-size: calc(1em * var(--article-font-scale))`
    // and `line-height: var(--article-line-height)` so theme rules
    // expressed in `em` / `rem` scale proportionally.
    // Phase 20c: no libadwaita system accent to fall back on. A theme with
    // `accent_hex: None` (Adwaita) now contributes no accent override; the
    // theme's own stylesheet colours apply. The Adwaita-theme accent is a
    // 20d decision (spec.md §12.3).
    let effective_accent = theme.accent_hex.map(|s| s.to_string());
    let (font_scale, line_height, reading_font, mono_font) = match crate::preferences::settings() {
        Some(s) => (
            crate::preferences::article_font_scale(&s),
            crate::preferences::article_line_height(&s),
            crate::preferences::font_serif(&s),
            crate::preferences::font_monospace(&s),
        ),
        None => (1.0, 1.0, String::new(), String::new()),
    };
    let mut root_decls = String::new();
    if let Some(hex) = effective_accent {
        root_decls.push_str(&format!("--accent-color: {};", hex));
    }
    root_decls.push_str(&format!(" --article-font-scale: {:.3};", font_scale));
    root_decls.push_str(&format!(" --article-line-height: {:.3};", line_height));
    let accent_root_css = format!(":root {{{} }}\n", root_decls);

    // v2.6.21 user font overrides for the reading pane. Built per-
    // render so a Preferences flip takes effect on the next theme
    // refresh. Layered after `VIADUCT_PANE_OVERRIDE_CSS` so the
    // user's choice wins over both theme styles and our scrollbar /
    // accent overrides. `!important` is necessary because byte-
    // perfect NNW theme stylesheets specify `body { font-family: … }`
    // with high specificity; without `!important` our later rule
    // doesn't override the theme's pinned family. Each rule is
    // emitted only when the corresponding GSetting is non-empty —
    // empty means "let the theme decide", which is the default.
    let mut user_font_css = String::new();
    if !reading_font.is_empty() {
        user_font_css.push_str(&format!(
            "body {{ font-family: \"{}\", inherit !important; }}\n",
            crate::preferences::css_escape(&reading_font)
        ));
    }
    if !mono_font.is_empty() {
        user_font_css.push_str(&format!(
            "code, pre, kbd, samp, tt {{ font-family: \"{}\", monospace !important; }}\n",
            crate::preferences::css_escape(&mono_font)
        ));
    }

    // Style cascade: accent custom property → bundled @font-face
    // rules (so themes can reference 'Atkinson Hyperlegible' etc.) →
    // byte-perfect NNW theme stylesheet → optional dark-mode
    // adaptation overlay (per-theme, hand-tuned prefers-color-scheme
    // block that activates when the system color scheme is dark) →
    // viaduct GTK-pane override → v2.6.21 user font overrides (last
    // so they win). Themes that adapt to dark mode internally
    // (Adwaita) or are dark-only (Tiqoe Dark) carry None for the
    // overlay slot.
    let style = match theme.dark_overlay {
        Some(overlay) => format!(
            "{}{}\n{}\n{}\n{}\n{}",
            accent_root_css,
            font_face_css(),
            theme.stylesheet,
            overlay,
            VIADUCT_PANE_OVERRIDE_CSS,
            user_font_css,
        ),
        None => format!(
            "{}{}\n{}\n{}\n{}",
            accent_root_css,
            font_face_css(),
            theme.stylesheet,
            VIADUCT_PANE_OVERRIDE_CSS,
            user_font_css,
        ),
    };
    outer_subs.insert("style", style);
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
    fn all_themes_load() {
        // Compile-time check — if any include_str! fails the build breaks.
        // This just asserts the registry shape didn't drift.
        assert_eq!(THEMES.len(), 9, "Adwaita + 8 NNW themes");
        for t in THEMES {
            assert!(!t.template.is_empty(), "{}: template empty", t.id);
            assert!(!t.stylesheet.is_empty(), "{}: stylesheet empty", t.id);
        }
        assert_eq!(theme_by_id("sepia").id, "sepia");
        assert_eq!(theme_by_id("adwaita").id, "adwaita");
        // Adwaita opts out of accent override.
        assert!(theme_by_id("adwaita").accent_hex.is_none());
        // Sepia carries an accent.
        assert!(theme_by_id("sepia").accent_hex.is_some());
        // Unknown id falls back to first registered theme (Adwaita).
        assert_eq!(theme_by_id("nope").id, "adwaita");
    }

    #[test]
    fn relative_luminance_handles_endpoints() {
        // Pure white should give 1.0; pure black 0.0; near-black our
        // hardcoded approximation. Allow generous epsilon for sRGB
        // gamma roundtripping.
        let white = relative_luminance("#ffffff").unwrap();
        let black = relative_luminance("#000000").unwrap();
        assert!((white - 1.0).abs() < 1e-6, "white luminance = {white}");
        assert!(black < 1e-6, "black luminance = {black}");
    }

    #[test]
    fn relative_luminance_rejects_malformed() {
        assert!(relative_luminance("").is_none());
        assert!(relative_luminance("nope").is_none());
        assert!(relative_luminance("#xyz").is_none());
        assert!(relative_luminance("#fff").is_none()); // 3-char shorthand not supported
    }

    #[test]
    fn pick_accent_fg_picks_white_for_dark_accents() {
        // Sepia, the blue family, NewsFax — every shipped theme except
        // Tiqoe Dark — should pair with white text.
        for hex in [
            "#7a4d1f", // Sepia
            "#086aee", // Appanoose / Hyperlegible / Promenade
            "#1145a5", // Biblioteca
            "#3a3a3a", // NewsFax
            "#2670c4", // Verdana Revival
        ] {
            assert_eq!(
                pick_accent_fg(hex),
                "#ffffff",
                "expected white fg for accent {hex}"
            );
        }
    }

    #[test]
    fn pick_accent_fg_picks_dark_for_warm_tan() {
        // Tiqoe Dark — the regression case from v1.5.5. Warm tan #b08660
        // gives ~3.4:1 with white but ~4.9:1 with near-black, so the
        // picker should pick the dark fg to preserve readability.
        assert_eq!(pick_accent_fg("#b08660"), "#1d1d1d");
    }

    #[test]
    fn pick_accent_fg_falls_back_safely_on_garbage() {
        // Malformed hex shouldn't produce invisible text — fall back
        // to white, which works against most realistic backgrounds.
        assert_eq!(pick_accent_fg("not-a-hex"), "#ffffff");
        assert_eq!(pick_accent_fg("#zzz"), "#ffffff");
    }
}
