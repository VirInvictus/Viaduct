// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

pub mod database;
pub mod error;
pub mod models;
pub mod network;
pub mod parser;
pub mod paths;
pub mod ui;

use crate::database::accounts::LocalAccount;
use adw::prelude::*;
use gtk::glib;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt};

// Store the Tokio runtime globally
static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

fn main() -> glib::ExitCode {
    init_tracing();

    // Initialize the Tokio runtime
    let rt = tokio::runtime::Runtime::new().expect("Unable to create Tokio runtime");
    RUNTIME.set(rt).expect("Runtime already initialized");

    if let Err(err) = paths::ensure_dirs() {
        error!(?err, "failed to create XDG directories; aborting");
        return glib::ExitCode::FAILURE;
    }

    info!(version = env!("CARGO_PKG_VERSION"), "Starting viaduct");

    // Initialize database infrastructure
    let (db_tx, db_rx) = mpsc::channel(100);
    if let Err(e) = database::worker::spawn_db_worker(db_rx) {
        error!(?e, "Failed to spawn database worker; aborting");
        return glib::ExitCode::FAILURE;
    }

    // Prepare LocalAccount. Wrapped in Arc so the window and any future
    // background tasks (refresher, search) can share it.
    let account =
        Arc::new(block_on(LocalAccount::new(db_tx)).expect("Failed to initialize LocalAccount"));

    let app = adw::Application::builder()
        .application_id("org.virinvictus.Viaduct")
        .build();

    let account_for_activate = account.clone();
    app.connect_activate(move |app| build_ui(app, account_for_activate.clone()));

    app.run()
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

fn build_ui(app: &adw::Application, account: Arc<LocalAccount>) {
    let window = ui::window::ViaductWindow::new(app, account);
    window.present();
}

/// Helper for spawning background work on the global Tokio runtime.
#[allow(dead_code)]
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let rt = RUNTIME.get().expect("Tokio runtime not initialized");
    rt.spawn(future)
}

/// Helper for blocking execution on the global Tokio runtime.
pub fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    let rt = RUNTIME.get().expect("Tokio runtime not initialized");
    rt.block_on(future)
}
