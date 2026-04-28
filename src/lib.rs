// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! viaduct library root. The binary (`src/main.rs`) is a thin wrapper that
//! spins up tracing + tokio + the GTK application; everything else lives here
//! so auxiliary binaries (profiling harnesses, future CLI tools) can share
//! the same modules.

pub mod database;
pub mod error;
pub mod models;
pub mod network;
pub mod parser;
pub mod paths;
pub mod preferences;
pub mod ui;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

pub fn is_debug_mode() -> bool {
    DEBUG_MODE.load(Ordering::Relaxed)
}

pub fn set_debug_mode(enabled: bool) {
    DEBUG_MODE.store(enabled, Ordering::Relaxed);
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
