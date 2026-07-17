// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Phase 18 / v2.0.0-pre1 — `ViaductArticlePaneView`. Owns the locked-down
//! `WebKitWebView`, the reader-view + play-video buttons in the article
//! pane's `AdwHeaderBar`, and the per-article display state. Lifted out of
//! `ViaductWindow` so the god-object shrinks one pane at a time. Window-side
//! callers interact through `set_article` / `set_auto_reader` / `clear` /
//! `idle_for_background` plus a few accessor helpers; everything else
//! (WebKit lockdown, link interceptor, `viaduct-img://` scheme, hover URL
//! overlay, theme + macro substitution, reader-view extraction kick-off,
//! video detection, in-pane embed dialog) lives here.
//!
//! NetNewsWire counterpart: `Mac/MainWindow/Detail/DetailViewController.swift`
//! plus the `ArticleRenderer` it uses. We bundle both for now; the further
//! `ArticleRenderer` GObject promotion is its own Phase 18 sub-step.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::sync::Arc;

use crate::network::ImageCache;
use crate::network::video_thumbs::VideoSource;
use crate::ui::article_renderer;

/// User preference for how to play YouTube / Vimeo videos detected in
/// articles. Mirrored from the `video-playback-mode` GSetting. Lives here
/// alongside `play_video` because the article pane is the only consumer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VideoPlaybackMode {
    InPane,
    External,
    Disabled,
}

/// v2.3.0: build the article-appearance popover. Two `AdwSpinRow`s
/// (Text size, Line spacing) bound bidirectionally to the GSettings,
/// plus a Reset button that puts both back to schema defaults. Live
/// edits trigger a re-render via the `notify::value-changed` chain
/// from GSettings → `connect_changed` listener (wired in `bootstrap`).
fn build_appearance_popover() -> gtk::Popover {
    let popover = gtk::Popover::builder().has_arrow(true).build();
    popover.add_css_class("menu");

    let group = adw::PreferencesGroup::builder()
        .title("Article Appearance")
        .build();

    let font_row = adw::SpinRow::with_range(75.0, 200.0, 5.0);
    font_row.set_title("Text Size");
    font_row.set_subtitle("Percentage of theme default");

    let line_row = adw::SpinRow::with_range(100.0, 250.0, 5.0);
    line_row.set_title("Line Spacing");
    line_row.set_subtitle("100 = single, 150 = 1.5×, 200 = double");

    if let Some(settings) = crate::preferences::settings() {
        settings
            .bind(
                crate::preferences::keys::ARTICLE_FONT_SCALE,
                &font_row,
                "value",
            )
            .build();
        settings
            .bind(
                crate::preferences::keys::ARTICLE_LINE_HEIGHT,
                &line_row,
                "value",
            )
            .build();
    }

    group.add(&font_row);
    group.add(&line_row);

    let reset_row = adw::ActionRow::builder()
        .title("Reset to Defaults")
        .activatable(true)
        .build();
    let reset_btn = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .valign(gtk::Align::Center)
        .build();
    reset_btn.add_css_class("flat");
    reset_row.add_suffix(&reset_btn);
    reset_row.set_activatable_widget(Some(&reset_btn));

    let font_for_reset = font_row.clone();
    let line_for_reset = line_row.clone();
    reset_btn.connect_clicked(move |_| {
        font_for_reset.set_value(100.0);
        line_for_reset.set_value(150.0);
    });

    group.add(&reset_row);

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    outer.append(&group);
    popover.set_child(Some(&outer));

    popover
}

fn current_video_playback_mode() -> VideoPlaybackMode {
    let Some(settings) = crate::preferences::settings() else {
        return VideoPlaybackMode::InPane;
    };
    match settings
        .string(crate::preferences::keys::VIDEO_PLAYBACK_MODE)
        .as_str()
    {
        "external" => VideoPlaybackMode::External,
        "disabled" => VideoPlaybackMode::Disabled,
        _ => VideoPlaybackMode::InPane,
    }
}

/// HTML-escape an embed URL for safe insertion into an `<iframe src="…">`
/// attribute. The embed URLs we generate carry query strings with `&`,
/// `=`, and the four other characters that need escaping in HTML attribute
/// context. Without this, `&rel=0&modestbranding=1` is interpreted by the
/// HTML parser as `&rel`-something + `&mod`-something entity references,
/// silently corrupting the URL the iframe actually loads.
fn embed_url_for_iframe(url: &str) -> String {
    let mut out = String::with_capacity(url.len() + 8);
    for c in url.chars() {
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

/// All the metadata `render_article_body` needs to drive the NNW theme
/// macros. The article body is whichever of `content_html` / `content_text`
/// / `summary` was non-empty (or a synthesized stub for sparse feeds).
/// Built by the timeline-selection handler on `ViaductWindow` — the pane
/// just consumes it.
#[derive(Default)]
pub struct ArticleRenderContext {
    pub raw_html: String,
    pub article_url: Option<String>,
    pub title: String,
    pub byline: String,
    pub feed_link: String,
    pub feed_link_title: String,
    pub date_published: Option<chrono::DateTime<chrono::Utc>>,
    /// Detected video source (if any) used to drive `play_video_btn`.
    pub video: Option<VideoSource>,
}

/// Per-article state. `raw_html` is the feed body, `extracted_html` caches
/// the readability extraction so the toggle is cheap. `auto_reader` mirrors
/// the per-feed `reader_view_always_enabled` setting; the timeline handler
/// pushes it in async via `set_auto_reader` once the DB lookup completes.
#[derive(Default)]
pub struct ArticleDisplayState {
    pub raw_html: Option<String>,
    pub extracted_html: Option<String>,
    pub article_url: Option<String>,
    pub auto_reader: bool,
    pub title: String,
    pub byline: String,
    pub feed_link: String,
    pub feed_link_title: String,
    pub date_published: Option<chrono::DateTime<chrono::Utc>>,
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "article_pane_view.ui")]
    pub struct ArticlePaneView {
        #[template_child]
        pub article_renderer: TemplateChild<crate::ui::article_renderer_widget::ArticleRenderer>,
        #[template_child]
        pub article_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub article_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub reader_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub play_video_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub appearance_btn: TemplateChild<gtk::MenuButton>,

        pub display: RefCell<ArticleDisplayState>,
        pub current_video: RefCell<Option<VideoSource>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArticlePaneView {
        const NAME: &'static str = "ViaductArticlePaneView";
        type Type = super::ArticlePaneView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            // ArticleRenderer's template is a child of this template, so
            // its GType must be registered before the GTK builder
            // resolves our template.
            crate::ui::article_renderer_widget::ArticleRenderer::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ArticlePaneView {}
    impl WidgetImpl for ArticlePaneView {}
    impl BinImpl for ArticlePaneView {}
}

glib::wrapper! {
    pub struct ArticlePaneView(ObjectSubclass<imp::ArticlePaneView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ArticlePaneView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ArticlePaneView {
    /// Bootstrap the inner `ArticleRenderer` (which owns the WebView, its
    /// per-renderer `WebContext`, the `viaduct-img://` + `viaduct-font://`
    /// scheme handlers, the link interceptor, and the hover URL overlay)
    /// then wire up the reader-view + play-video buttons.
    pub fn bootstrap(&self, image_cache: Arc<ImageCache>) {
        let imp = self.imp();

        imp.article_renderer.get().bootstrap(image_cache);

        // Reader-button toggle → re-render with extracted or raw body.
        let weak_for_reader = self.downgrade();
        imp.reader_btn.connect_toggled(move |_| {
            if let Some(view) = weak_for_reader.upgrade() {
                view.render_article_body();
            }
        });

        // Play-video click → present in-pane / external dispatch.
        let weak_for_video = self.downgrade();
        imp.play_video_btn.connect_clicked(move |_| {
            if let Some(view) = weak_for_video.upgrade() {
                view.play_video();
            }
        });

        // Track the video-playback-mode GSetting so flipping it from the
        // Preferences dialog refreshes the play button's visibility live.
        if let Some(settings) = crate::preferences::settings() {
            let weak_for_settings = self.downgrade();
            settings.connect_changed(
                Some(crate::preferences::keys::VIDEO_PLAYBACK_MODE),
                move |_, _| {
                    if let Some(view) = weak_for_settings.upgrade() {
                        view.refresh_video_button_visibility();
                    }
                },
            );

            // v2.3.0: re-render the article when the appearance settings
            // change. The font scale + line height ride into the page
            // wrapper as CSS custom properties on each render, so
            // invalidating + re-rendering picks up the new values
            // immediately without the user needing to re-select.
            let weak_for_font = self.downgrade();
            settings.connect_changed(
                Some(crate::preferences::keys::ARTICLE_FONT_SCALE),
                move |_, _| {
                    if let Some(view) = weak_for_font.upgrade() {
                        view.refresh_render();
                    }
                },
            );
            let weak_for_line = self.downgrade();
            settings.connect_changed(
                Some(crate::preferences::keys::ARTICLE_LINE_HEIGHT),
                move |_, _| {
                    if let Some(view) = weak_for_line.upgrade() {
                        view.refresh_render();
                    }
                },
            );
            // v2.6.21: re-render when the user-supplied reading-pane
            // font overrides change. Both ride into the cascade after
            // VIADUCT_PANE_OVERRIDE_CSS in render_themed; an explicit
            // re-render picks them up without waiting for the next
            // article selection.
            for key in [
                crate::preferences::keys::FONT_SERIF,
                crate::preferences::keys::FONT_MONOSPACE,
            ] {
                let weak_for_font = self.downgrade();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(view) = weak_for_font.upgrade() {
                        view.refresh_render();
                    }
                });
            }
        }

        // v2.3.0: build the appearance popover lazily and attach it to
        // the toolbar menu button.
        imp.appearance_btn
            .set_popover(Some(&build_appearance_popover()));
    }

    /// Set the article being displayed. Resets reader-view state — callers
    /// should follow this with `set_auto_reader` once the per-feed
    /// `reader_view_always_enabled` lookup completes.
    pub fn set_article(&self, ctx: ArticleRenderContext) {
        let imp = self.imp();
        {
            let mut state = imp.display.borrow_mut();
            state.raw_html = Some(ctx.raw_html);
            state.extracted_html = None;
            state.article_url = ctx.article_url;
            state.auto_reader = false;
            state.title = ctx.title;
            state.byline = ctx.byline;
            state.feed_link = ctx.feed_link;
            state.feed_link_title = ctx.feed_link_title;
            state.date_published = ctx.date_published;
        }
        // Keep the header bar oriented: article title up top, feed name as
        // subtitle. Fall back to the feed name as the title for the rare
        // titleless item so the bar is never blank.
        {
            let state = imp.display.borrow();
            if state.title.is_empty() {
                imp.article_title.set_title(&state.feed_link_title);
                imp.article_title.set_subtitle("");
            } else {
                imp.article_title.set_title(&state.title);
                imp.article_title.set_subtitle(&state.feed_link_title);
            }
        }
        // Untoggle reader without re-firing its handler — the async
        // auto_reader resolution below will set it true if appropriate.
        imp.reader_btn.set_active(false);
        *imp.current_video.borrow_mut() = ctx.video;
        self.refresh_video_button_visibility();
        self.render_article_body();
    }

    /// Push the per-feed `reader_view_always_enabled` flag in. Window
    /// resolves it asynchronously after the article is set; when true we
    /// flip the reader button on (which fires its toggled handler and
    /// kicks off the readability extraction).
    pub fn set_auto_reader(&self, auto: bool) {
        let imp = self.imp();
        imp.display.borrow_mut().auto_reader = auto;
        if auto {
            imp.reader_btn.set_active(true);
        }
    }

    /// Programmatically toggle the reader-view button. Bound to
    /// `win.toggle-reader` (Ctrl+Shift+R).
    pub fn toggle_reader(&self) {
        let btn = &self.imp().reader_btn;
        btn.set_active(!btn.is_active());
    }

    /// Programmatically activate the play-video action. Bound to the
    /// `win.play-video` accelerator path; also fires from the button click
    /// handler installed by `bootstrap`.
    pub fn play_video(&self) {
        let Some(source) = self.imp().current_video.borrow().clone() else {
            return;
        };
        match current_video_playback_mode() {
            VideoPlaybackMode::InPane => self.present_video_dialog(&source),
            VideoPlaybackMode::External => {
                let watch = source.watch_url();
                let _ = gio::AppInfo::launch_default_for_uri(&watch, gio::AppLaunchContext::NONE);
            }
            VideoPlaybackMode::Disabled => {
                // Button is hidden; nothing to do.
            }
        }
    }

    /// Clear the pane (no article selected). Called when the user closes
    /// the active article via `Esc` or when the timeline selection drops.
    pub fn clear(&self) {
        let imp = self.imp();
        imp.article_stack.set_visible_child_name("empty");
        imp.display.replace(ArticleDisplayState::default());
        *imp.current_video.borrow_mut() = None;
        imp.play_video_btn.set_visible(false);
        imp.article_title.set_title("");
        imp.article_title.set_subtitle("");
    }

    /// Drop the article body and idle the WebProcess. Called from
    /// `ViaductWindow::hide_for_background` when the user closes the window
    /// while run-in-background mode is on.
    pub fn idle_for_background(&self) {
        self.imp().article_renderer.get().idle();
        self.clear();
    }

    /// v2.2.0: present the print dialog for the current article.
    /// Delegates to `ArticleRenderer::print`. Caller (window) supplies
    /// itself as the parent so the dialog is modal-correct.
    pub fn print(&self, parent: Option<&gtk::Window>) {
        self.imp().article_renderer.get().print(parent);
    }

    /// The article `WebKitWebView`, for the window's Phase 19 keyboard
    /// plumbing: it installs capture-phase nav shortcuts here so `j`/`k`/
    /// `Down` keep navigating even while the body holds focus, and moves
    /// focus in so Space reaches WebKit's native paging.
    pub fn web_view(&self) -> Option<webkit6::WebView> {
        self.imp().article_renderer.get().web_view()
    }

    /// Move keyboard focus into the article body. Returns false when there
    /// is no WebView yet (bootstrap hasn't run) or it refuses focus, which
    /// the caller uses to leave focus where it was rather than stranding it.
    pub fn focus_article(&self) -> bool {
        self.web_view().is_some_and(|wv| wv.grab_focus())
    }

    /// Read the current article's preferred URL. Used by `act_copy_url` /
    /// `act_open_in_browser` on the window when the timeline isn't focused.
    pub fn current_article_url(&self) -> Option<String> {
        self.imp().display.borrow().article_url.clone()
    }

    /// Whether a reader-view extraction is currently active. Used by the
    /// window's keyboard-shortcut path to decide whether toggling reader
    /// is meaningful.
    pub fn reader_active(&self) -> bool {
        self.imp().reader_btn.is_active()
    }

    pub fn refresh_video_button_visibility(&self) {
        let imp = self.imp();
        let has_video = imp.current_video.borrow().is_some();
        let mode = current_video_playback_mode();
        let visible = has_video && mode != VideoPlaybackMode::Disabled;
        imp.play_video_btn.set_visible(visible);
    }

    /// Public re-render hook for callers that want to apply state changes
    /// outside the pane's control (theme GSetting flip, system dark-mode
    /// toggle). The window subscribes to those signals and calls this so
    /// "auto" mode swaps Sepia ↔ Tiqoe Dark live without re-selecting.
    pub fn refresh_render(&self) {
        self.render_article_body();
    }

    /// Re-render the article body from the current display state. Invoked
    /// on initial set, on reader-button toggle, and from the async
    /// readability extractor when its result lands.
    fn render_article_body(&self) {
        let imp = self.imp();
        let state = imp.display.borrow();
        let reader_mode = imp.reader_btn.is_active();
        let raw = state.raw_html.clone();
        let extracted = state.extracted_html.clone();
        let url = state.article_url.clone();

        let body_html = if reader_mode {
            extracted.clone().or_else(|| raw.clone())
        } else {
            raw.clone()
        };
        let Some(body_html) = body_html else {
            drop(state);
            imp.article_stack.set_visible_child_name("empty");
            return;
        };
        imp.article_stack.set_visible_child_name("content");

        let is_dark = adw::StyleManager::default().is_dark();
        let theme = match crate::preferences::settings() {
            Some(s) => crate::preferences::resolve_article_theme(&s, is_dark),
            None => article_renderer::select_for_dark_mode(is_dark),
        };

        let subs = article_renderer::ArticleSubstitutions {
            title: article_renderer::escape_html(&state.title),
            body: body_html,
            preferred_link: state.article_url.clone().unwrap_or_default(),
            feed_link: state.feed_link.clone(),
            feed_link_title: article_renderer::escape_html(&state.feed_link_title),
            byline: article_renderer::escape_html(&state.byline),
            datetime_long: state
                .date_published
                .map(|d| d.format("%A, %B %e, %Y at %l:%M:%S %p").to_string())
                .unwrap_or_default(),
            datetime_medium: state
                .date_published
                .map(|d| d.format("%b %e, %Y at %l:%M %p").to_string())
                .unwrap_or_default(),
            datetime_short: state
                .date_published
                .map(|d| d.format("%-m/%-d/%y, %l:%M %p").to_string())
                .unwrap_or_default(),
            date_long: state
                .date_published
                .map(|d| d.format("%A, %B %e, %Y").to_string())
                .unwrap_or_default(),
            date_medium: state
                .date_published
                .map(|d| d.format("%b %e, %Y").to_string())
                .unwrap_or_default(),
            date_short: state
                .date_published
                .map(|d| d.format("%-m/%-d/%y").to_string())
                .unwrap_or_default(),
            time_long: state
                .date_published
                .map(|d| d.format("%l:%M:%S %p").to_string())
                .unwrap_or_default(),
            time_medium: state
                .date_published
                .map(|d| d.format("%l:%M:%S %p").to_string())
                .unwrap_or_default(),
            time_short: state
                .date_published
                .map(|d| d.format("%l:%M %p").to_string())
                .unwrap_or_default(),
            avatar_src: String::new(),
            external_link: String::new(),
            external_link_label: String::new(),
            external_link_stripped: String::new(),
        };
        drop(state);

        imp.article_renderer
            .get()
            .render_themed(theme, subs, url.as_deref());

        // Reader-view extraction: when the user toggles reader on but we
        // haven't extracted yet, kick off the extractor. The fallback
        // render above already showed the raw body so the pane isn't
        // blank during the wait.
        if reader_mode && extracted.is_none() {
            let Some(url) = url else { return };
            let weak = self.downgrade();
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let result = crate::ui::reader_view::extract(&url, raw.as_deref()).await;
                let _ = tx.send(result);
            });
            glib::spawn_future_local(async move {
                match rx.await {
                    Ok(Ok(extracted)) => {
                        if let Some(view) = weak.upgrade() {
                            view.imp().display.borrow_mut().extracted_html = Some(extracted);
                            if view.imp().reader_btn.is_active() {
                                view.render_article_body();
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "reader view extraction failed");
                        if let Some(view) = weak.upgrade() {
                            view.imp().reader_btn.set_active(false);
                        }
                    }
                    Err(_) => {
                        tracing::warn!("reader view extraction task aborted");
                    }
                }
            });
        }
    }

    /// In-pane YouTube / Vimeo playback (v1.4.0). Spins up a transient
    /// `WebKitWebView` inside an `AdwDialog` parented on the nearest
    /// window, JS + LocalStorage on (the embed players need them); the
    /// dialog's close handler `try_close()`s the WebView so the
    /// WebProcess shuts down and audio stops cleanly. The article-pane
    /// WebView's lockdown profile is unchanged.
    fn present_video_dialog(&self, source: &VideoSource) {
        use webkit6::prelude::*;

        let dialog = adw::Dialog::new();
        dialog.set_title("Video");
        dialog.set_content_width(960);
        dialog.set_content_height(560);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let view = webkit6::WebView::new();
        view.set_vexpand(true);
        view.set_hexpand(true);

        if let Some(settings) = webkit6::prelude::WebViewExt::settings(&view) {
            settings.set_enable_javascript(true);
            settings.set_javascript_can_access_clipboard(false);
            settings.set_javascript_can_open_windows_automatically(false);
            settings.set_enable_webgl(true);
            settings.set_enable_html5_database(true);
            settings.set_enable_html5_local_storage(true);
            settings.set_enable_offline_web_application_cache(false);
            settings.set_enable_developer_extras(false);
            settings.set_enable_back_forward_navigation_gestures(false);
            settings.set_enable_fullscreen(true);
            settings.set_media_playback_requires_user_gesture(false);
        }

        let embed_url_attr = embed_url_for_iframe(&source.embed_url());
        let html = format!(
            "<!DOCTYPE html>\n\
             <html>\n\
             <head>\n\
             <meta charset=\"utf-8\">\n\
             <style>\n\
             * {{ box-sizing: border-box; margin: 0; padding: 0; }}\n\
             html, body {{ width: 100%; height: 100%; background: #000; }}\n\
             iframe {{ width: 100%; height: 100%; border: 0; display: block; }}\n\
             </style>\n\
             </head>\n\
             <body>\n\
             <iframe src=\"{}\" allow=\"autoplay; fullscreen; encrypted-media; picture-in-picture\" allowfullscreen referrerpolicy=\"no-referrer\"></iframe>\n\
             </body>\n\
             </html>",
            embed_url_attr,
        );
        view.load_html(&html, Some("https://viaduct.local/"));

        toolbar.set_content(Some(&view));
        dialog.set_child(Some(&toolbar));

        // Tear down the WebProcess on dialog close so audio actually stops.
        let view_for_close = view.clone();
        dialog.connect_closed(move |_| {
            view_for_close.load_uri("about:blank");
            view_for_close.try_close();
        });

        // Find the nearest window for parenting via the widget hierarchy.
        let parent_window = self
            .ancestor(gtk::Window::static_type())
            .and_downcast::<gtk::Window>();
        dialog.present(parent_window.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_url_for_iframe_escapes_query_separators() {
        let raw = "https://www.youtube-nocookie.com/embed/abc?autoplay=1&rel=0&modestbranding=1";
        let escaped = embed_url_for_iframe(raw);
        assert_eq!(
            escaped,
            "https://www.youtube-nocookie.com/embed/abc?autoplay=1&amp;rel=0&amp;modestbranding=1"
        );
    }

    #[test]
    fn embed_url_for_iframe_escapes_attribute_breakers() {
        let raw = "https://x.test/?q=<script>&v=\"1'2\"";
        let escaped = embed_url_for_iframe(raw);
        assert!(!escaped.contains('"'));
        assert!(!escaped.contains('<'));
        assert!(!escaped.contains('>'));
        assert!(escaped.contains("&quot;"));
        assert!(escaped.contains("&lt;"));
        assert!(escaped.contains("&gt;"));
    }

    #[test]
    fn embed_url_for_iframe_passes_safe_chars_through() {
        let raw = "https://example.com/path?id=abc-123_def&x=1";
        let escaped = embed_url_for_iframe(raw);
        assert_eq!(escaped, "https://example.com/path?id=abc-123_def&amp;x=1");
    }
}
