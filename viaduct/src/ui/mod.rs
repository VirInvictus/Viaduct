// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

// UI layer for plain GTK4 (libadwaita removed in v3.0.0)

// The shared row builders, Alert dialog, and Escape-to-close live in
// vir-gtk's widget kit since 1.4.0; the historical `rows` module path
// re-exports it so call sites keep resolving.

/// Close `window` when Escape is pressed. Re-exported from
/// [`vir_gtk::widgets`] (capture phase, so the dialog closes even while a
/// child entry has focus; `GtkText` would otherwise swallow the key).
pub use vir_gtk::widgets::close_on_escape;

pub use vir_gtk::widgets as rows;

pub mod actions;
pub mod activity_dialog;
pub mod add_feed_dialog;
pub mod article_pane_view;
pub mod article_renderer;
pub mod article_renderer_widget;
pub mod avatar;
pub mod batch;
pub mod preferences_dialog;
pub mod reader_view;
pub mod refresh;
pub mod sidebar;
pub mod sidebar_view;
pub mod smart_feed_dialog;
pub mod status_page;
pub mod timeline;
pub mod timeline_view;
pub mod tree;
pub mod welcome_dialog;
pub mod window;
pub mod window_title;
