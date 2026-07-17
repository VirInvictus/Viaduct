// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

// UI layer for GTK4/Libadwaita
pub mod actions;
pub mod activity_dialog;
pub mod add_feed_dialog;
pub mod article_pane_view;
pub mod article_renderer;
pub mod article_renderer_widget;
pub mod batch;
pub mod coalescing_queue;
pub mod fetch_queue;
pub mod preferences_dialog;
pub mod reader_view;
pub mod refresh;
pub mod rows;
pub mod sidebar;
pub mod sidebar_view;
pub mod smart_feed_dialog;
pub mod timeline;
pub mod timeline_view;
pub mod tree;
pub mod welcome_dialog;
pub mod window;

pub fn init_ui() {
    // Phase 5: Setup AdwNavigationSplitView
}
