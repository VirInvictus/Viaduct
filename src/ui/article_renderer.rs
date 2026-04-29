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
//! v1.1.0-pre1 ships the bare-bones `render(view, html, base_uri)` that just
//! calls `WebView::load_html`. CSP, theme, image-URI-scheme, and link
//! interception land in subsequent commits.
//!
//! NNW deviation: NNW uses native `WKWebView` (macOS/iOS) with a similar
//! lockdown profile in `WebViewController.applyConfiguration`. We translate
//! the same intent to WebKitGTK 6.0 via `webkit6::Settings`.

use webkit6::prelude::*;

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

/// Render the supplied HTML into the article pane. Replaces any prior
/// content. `base_uri` is used by WebKit to resolve relative URLs in the
/// payload — pass the article's permalink when known so relative `<a href>`
/// and `<img src>` resolve correctly. `None` falls back to about:blank.
pub fn render(view: &webkit6::WebView, html: &str, base_uri: Option<&str>) {
    view.load_html(html, base_uri);
}
