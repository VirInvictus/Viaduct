// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

// Network layer for fetching feeds and images
pub mod cache;
pub mod fetcher;

pub use fetcher::{Fetcher, LocalAccountRefresher};

pub fn init_network() {
    // Phase 2: Setup reqwest and Tokio async reactor
}
