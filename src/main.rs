// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use adw::prelude::*;
use gtk::glib;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt};
use viaduct::database::accounts::LocalAccount;
use viaduct::{database, paths, ui};

fn main() -> glib::ExitCode {
    init_tracing();

    // Install the library-wide Tokio runtime. Multi-thread flavor because
    // the refresher fans out per-feed fetches via tokio::spawn.
    let rt = tokio::runtime::Runtime::new().expect("Unable to create Tokio runtime");
    viaduct::init_runtime(rt);

    if let Err(err) = paths::ensure_dirs() {
        error!(?err, "failed to create XDG directories; aborting");
        return glib::ExitCode::FAILURE;
    }

    info!(version = env!("CARGO_PKG_VERSION"), "Starting viaduct");

    let (db_tx, db_rx) = mpsc::channel(100);
    if let Err(e) = database::worker::spawn_db_worker(db_rx) {
        error!(?e, "Failed to spawn database worker; aborting");
        return glib::ExitCode::FAILURE;
    }

    let account = Arc::new(
        viaduct::block_on_runtime(LocalAccount::new(db_tx))
            .expect("Failed to initialize LocalAccount"),
    );

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
