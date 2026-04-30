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
    MemoryBreakdown, block_on_runtime, database, error, init_runtime, is_debug_mode, models,
    network, parser, paths, read_memory_mb, rss_breakdown, set_debug_mode,
    spawn_debug_memory_ticker, spawn_on_runtime,
};

pub mod fonts;
pub mod preferences;
pub mod tray;
pub mod ui;

/// v2.6.14: force mimalloc to return freed-but-cached pages to the
/// OS. Called at the end of each refresh cycle so the per-cycle
/// transient peak doesn't stick around as elevated RSS floor across
/// cycles. mimalloc's default `purge_delay` is 1000 ms; we also set
/// `MIMALLOC_PURGE_DELAY=100` at startup, but `mi_collect(true)` is
/// the explicit "now please" signal — completes synchronously in
/// ~1 ms on typical heaps.
///
/// Safe to call from any thread: `mi_collect` operates on the
/// process-wide default heap that every allocation in this binary
/// goes through (we registered mimalloc as the global allocator in
/// `main.rs`).
pub fn mimalloc_collect() {
    // SAFETY: `mi_collect` is FFI-safe — it takes a bool and returns
    // nothing. The libmimalloc-sys crate vendors the C library that
    // mimalloc-rs already depends on, so the symbol is always
    // resolvable in our binary; we don't need a dep declaration to
    // reach an FFI symbol that's already linked in.
    unsafe extern "C" {
        fn mi_collect(force: bool);
    }
    unsafe {
        mi_collect(true);
    }
}

/// v2.6.16: dump mimalloc's heap stats to stderr. Triggered from the
/// `--debug` "Memory snapshot" Debug-menu action so the user can grab
/// per-arena / per-size-class allocator state any time RSS spikes.
/// Mimalloc writes a few hundred lines of stats to stderr with size
/// classes, page commit/decommit counts, segment counts, etc.
pub fn mimalloc_print_stats() {
    unsafe extern "C" {
        fn mi_stats_print(out: *mut std::ffi::c_void);
    }
    unsafe {
        mi_stats_print(std::ptr::null_mut());
    }
}
