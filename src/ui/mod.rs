// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

// UI layer for GTK4/Libadwaita
pub mod actions;
pub mod article;
pub mod batch;
pub mod coalescing_queue;
pub mod fetch_queue;
pub mod preferences_dialog;
pub mod reader_view;
pub mod sidebar;
pub mod timeline;
pub mod tree;
pub mod window;

pub fn init_ui() {
    // Phase 5: Setup AdwNavigationSplitView
}
