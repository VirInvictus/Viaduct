// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! viaduct binary crate's library root. Holds every GTK / libadwaita /
//! WebKit-touching module; everything headless lives in the sibling
//! `viaduct-core` crate. `main.rs` is the thin GTK entrypoint.
//!
//! Re-exports `viaduct_core` symbols at the crate root so existing
//! intra-binary callers can keep using the unprefixed names
//! (`crate::models`, `crate::network`, etc.) without a churn of
//! search-and-replace through every `ui::*` file.

pub use viaduct_core::{
    block_on_runtime, database, error, init_runtime, is_debug_mode, models, network, parser, paths,
    read_memory_mb, set_debug_mode, spawn_debug_memory_ticker, spawn_on_runtime,
};

pub mod fonts;
pub mod preferences;
pub mod tray;
pub mod ui;
