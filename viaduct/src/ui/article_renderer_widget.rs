// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Phase 18 / v2.0.0-pre4 — `ViaductArticleRenderer`. Wraps the locked-down
//! `WebKitWebView` plus its hover URL overlay in a real GObject so
//! `ArticlePaneView` can stop reaching into the WebView directly. The
//! template root is a `GtkOverlay` (with the URL `GtkLabel` as the overlay
//! child); the `WebKitWebView` is constructed in `bootstrap` against a
//! per-renderer `WebContext` and parented as the overlay's main child.
//!
//! The per-renderer `WebContext` is the architectural improvement promised
//! in `roadmap.md` Phase 18: `viaduct-img://` and `viaduct-font://` scheme
//! handlers used to be registered globally on `WebContext::default()`,
//! which leaked them into every other WebView in the process (the video
//! playback dialog, future preview / multi-window WebViews). With this
//! commit each `ArticleRenderer` owns its own context and registers the
//! schemes only there. The video dialog continues to use the default
//! context — it doesn't need our schemes.
//!
//! NetNewsWire counterpart: the `ArticleRenderer` class in `Mac/Article`
//! plus the `WebViewController` it composes.

use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::OnceCell;
use std::sync::Arc;
use webkit6::prelude::WebViewExt;

use crate::network::ImageCache;
use crate::ui::article_renderer;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "article_renderer_widget.ui")]
    pub struct ArticleRenderer {
        #[template_child]
        pub overlay: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub url_overlay: TemplateChild<gtk::Label>,

        /// Per-renderer `WebContext`. Each `ArticleRenderer` registers its
        /// `viaduct-img://` + `viaduct-font://` handlers here, scoped to
        /// this renderer's WebView only.
        pub web_context: OnceCell<webkit6::WebContext>,
        /// Locked-down `WebKitWebView`. Constructed in `bootstrap` against
        /// `web_context` and parented as `overlay`'s main child.
        pub web_view: OnceCell<webkit6::WebView>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArticleRenderer {
        const NAME: &'static str = "ViaductArticleRenderer";
        type Type = super::ArticleRenderer;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ArticleRenderer {}
    impl WidgetImpl for ArticleRenderer {}
    impl BinImpl for ArticleRenderer {}
}

glib::wrapper! {
    pub struct ArticleRenderer(ObjectSubclass<imp::ArticleRenderer>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ArticleRenderer {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ArticleRenderer {
    /// Construct the per-renderer `WebContext`, register
    /// `viaduct-img://` + `viaduct-font://` schemes on it, build the
    /// locked-down `WebKitWebView`, install the link interceptor and
    /// hover URL overlay, and parent the WebView as the overlay's main
    /// child. Call once per renderer from
    /// `ArticlePaneView::bootstrap`.
    pub fn bootstrap(&self, image_cache: Arc<ImageCache>) {
        let imp = self.imp();

        // Per-renderer WebContext. Default `WebContext::default()` is
        // shared with the video-dialog WebView (in
        // `ArticlePaneView::present_video_dialog`); registering schemes
        // there used to leak our handlers into the embed iframe. With
        // a fresh context the handlers stay scoped to *this* renderer.
        let context = webkit6::WebContext::new();
        article_renderer::install_image_uri_scheme(&context, image_cache);
        article_renderer::install_font_uri_scheme(&context);

        let web_view = webkit6::WebView::builder()
            .web_context(&context)
            .vexpand(true)
            .hexpand(true)
            .build();

        article_renderer::apply_locked_down_settings(&web_view);
        article_renderer::install_link_interceptor(&web_view);
        article_renderer::install_hover_url_overlay(&web_view, &imp.url_overlay.get());

        imp.overlay.set_child(Some(&web_view));

        let _ = imp.web_context.set(context);
        let _ = imp.web_view.set(web_view);
    }

    /// Render an article body through the NNW page-wrapper + theme
    /// pipeline. Caller is responsible for picking the theme + building
    /// the substitutions; this method just runs the macro engine and
    /// loads the result. Mirrors the v1.1.0 free-function `render_themed`
    /// signature exactly so the macro behaviour stays byte-for-byte the
    /// same.
    pub fn render_themed(
        &self,
        theme: article_renderer::Theme,
        substitutions: article_renderer::ArticleSubstitutions,
        base_uri: Option<&str>,
    ) {
        let Some(view) = self.imp().web_view.get() else {
            tracing::warn!("ArticleRenderer::render_themed before bootstrap");
            return;
        };
        article_renderer::render_themed(view, theme, substitutions, base_uri);
    }

    /// Load `about:blank` to free the active page state. The WebProcess
    /// itself stays attached so we don't pay the spawn cost on the next
    /// render. Used by `ArticlePaneView::idle_for_background` (Phase 17).
    pub fn idle(&self) {
        if let Some(view) = self.imp().web_view.get() {
            view.load_uri("about:blank");
        }
    }

    /// v2.2.0: present the system print dialog scoped to the current
    /// article. Wraps `webkit6::PrintOperation::run_dialog(parent)`. The
    /// printed output is exactly what the WebKit pane currently renders
    /// (theme + macros + locked-down CSP); fonts and CSS print rules are
    /// honored by the underlying GTK print backend. No-op if the
    /// renderer hasn't been bootstrapped yet.
    pub fn print(&self, parent: Option<&gtk::Window>) {
        let Some(view) = self.imp().web_view.get() else {
            tracing::warn!("ArticleRenderer::print before bootstrap");
            return;
        };
        let op = webkit6::PrintOperation::new(view);
        op.run_dialog(parent);
    }

    /// Read access to the underlying `WebKitWebView`. Currently unused
    /// by callers outside `ArticlePaneView`; exposed for future preview /
    /// printing code paths that need the raw view.
    #[allow(dead_code)]
    pub fn web_view(&self) -> Option<webkit6::WebView> {
        self.imp().web_view.get().cloned()
    }
}
