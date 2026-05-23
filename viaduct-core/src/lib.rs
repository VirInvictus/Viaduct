// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! viaduct-core: headless library crate housing every non-GUI module.
//! Database, network, parser, models, error types, XDG path helpers, and
//! the global tokio runtime + debug-mode toggles all live here.
//!
//! The GTK / libadwaita / WebKit binary lives in the sibling `viaduct`
//! crate which depends on this one. The split (introduced in v1.5.0)
//! enforces architectural boundaries by making it a *compile error* to
//! reach into GTK from data / network / parser code, rather than relying
//! on review discipline. It also lets profiling harnesses (`mem_check`)
//! and future headless CLIs share the same code paths.

pub mod database;
pub mod error;
pub mod models;
pub mod network;
pub mod parser;
pub mod paths;
pub mod smart_feeds;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

pub fn is_debug_mode() -> bool {
    DEBUG_MODE.load(Ordering::Relaxed)
}

pub fn set_debug_mode(enabled: bool) {
    DEBUG_MODE.store(enabled, Ordering::Relaxed);
}

/// In debug mode, spawn a background tokio task that periodically reads
/// `/proc/self/status` and emits a `tracing::info!` line with VmRSS and
/// VmHWM. Random interval between 8 and 25 seconds — enough cadence to
/// catch leaks during a refresh cycle, infrequent enough that the log
/// isn't a fire hose. No-op outside debug mode.
pub fn spawn_debug_memory_ticker() {
    if !is_debug_mode() {
        return;
    }
    spawn_on_runtime(async {
        use std::time::Duration;
        // Crude PRNG seeded from the current time — we just need
        // varying cadence, not crypto. Avoids pulling in the rand crate.
        let mut state: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xCAFE_F00D)
            | 1;
        loop {
            // xorshift64
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let secs = 8 + (state % 18); // 8..=25
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let (rss_mb, hwm_mb) = read_memory_mb();
            tracing::info!(
                rss_mb,
                peak_mb = hwm_mb,
                budget_mb = 500,
                "debug: memory tick"
            );
        }
    });
}

/// v2.6.16: RSS broken down by mapping class. Read from
/// `/proc/self/smaps_rollup`. All fields in MB; sum of `anon_mb` +
/// `file_mb` + `shmem_mb` ≈ `rss_mb` (with rounding + tiny
/// kernel-internal accounting overlap). The breakdown answers the
/// question the single `rss_mb` value can't: when memory grows during
/// a refresh cycle, *which class* grew. Anon = mimalloc heap + tokio
/// stacks + Rust allocations. File = SQLite mmap, binaries, fonts,
/// installed shared objects. Shmem = WebKit's shared-memory regions
/// between UIProcess and the WebProcess child (its private memory is
/// in a separate process's smaps and not visible here).
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryBreakdown {
    pub rss_mb: u64,
    pub anon_mb: u64,
    pub file_mb: u64,
    pub shmem_mb: u64,
    pub swap_mb: u64,
}

/// Parse `/proc/self/smaps_rollup` into a `MemoryBreakdown`. Returns
/// the default (all zeros) if the file can't be read — non-Linux
/// hosts, sandbox lockdown, etc. Caller can log unconditionally.
pub fn rss_breakdown() -> MemoryBreakdown {
    let Ok(rollup) = std::fs::read_to_string("/proc/self/smaps_rollup") else {
        return MemoryBreakdown::default();
    };
    let mut out = MemoryBreakdown::default();
    for line in rollup.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb_str) = rest.split_whitespace().next() else {
            continue;
        };
        let kb: u64 = kb_str.parse().unwrap_or(0);
        let mb = kb / 1024;
        match key {
            "Rss" => out.rss_mb = mb,
            "Anonymous" => out.anon_mb = mb,
            "Pss_File" => out.file_mb = mb,
            "Pss_Shmem" => out.shmem_mb = mb,
            "Swap" => out.swap_mb = mb,
            _ => {}
        }
    }
    out
}

/// Read VmRSS + VmHWM from `/proc/self/status`, both in MB. Returns
/// `(0, 0)` if the file can't be read (non-Linux test sandboxes, etc.) so
/// callers can log unconditionally without branching on `Option`.
pub fn read_memory_mb() -> (u64, u64) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (0, 0);
    };
    let mut rss_kb = 0u64;
    let mut hwm_kb = 0u64;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kb = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("VmHWM:") {
            hwm_kb = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        }
    }
    (rss_kb / 1024, hwm_kb / 1024)
}

/// Global Tokio runtime handle. Initialized once by the binary (`main.rs`
/// for the GTK app, individual bins like `mem_check.rs` for harnesses) via
/// `init_runtime` below. All library callers use `spawn_on_runtime` to run
/// async work outside the GTK main loop without rebuilding a runtime.
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Install a tokio runtime as the library-wide runtime. Second+ calls are
/// no-ops (returns `Err` in the underlying OnceLock but we swallow — the
/// library doesn't care which runtime wins as long as one is set).
pub fn init_runtime(rt: tokio::runtime::Runtime) {
    let _ = RUNTIME.set(rt);
}

/// Spawn a future on the library-wide tokio runtime. Panics if no runtime
/// has been installed via `init_runtime` — that's a startup bug, not a
/// recoverable condition.
pub fn spawn_on_runtime<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    RUNTIME
        .get()
        .expect("viaduct: tokio runtime not initialized")
        .spawn(future)
}

/// Block-run a future on the library-wide tokio runtime. Use only from
/// synchronous callers that cannot be made async (e.g. the one-time
/// `Account` init in `main.rs`). Do NOT call from inside tokio tasks
/// or from the GTK main loop — either will deadlock or stall the UI.
pub fn block_on_runtime<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    RUNTIME
        .get()
        .expect("viaduct: tokio runtime not initialized")
        .block_on(future)
}
