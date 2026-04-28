// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use adw::prelude::*;
use gtk::glib;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt};
use viaduct::database::accounts::Account;
use viaduct::{database, paths, ui};

fn main() -> glib::ExitCode {
    init_tracing();

    // Point GSettings at our compiled schema dir before any gio call. Dev
    // builds rely on `build.rs` having run `glib-compile-schemas data/`;
    // installed builds (Flatpak in Phase 17) ignore this because the schema
    // ships in the runtime's prefix.
    ensure_schema_dir();

    // Install the library-wide Tokio runtime. Multi-thread flavor because
    // the refresher fans out per-feed fetches via tokio::spawn.
    let rt = tokio::runtime::Runtime::new().expect("Unable to create Tokio runtime");
    viaduct::init_runtime(rt);

    if let Err(err) = paths::ensure_dirs() {
        error!(?err, "failed to create XDG directories; aborting");
        return glib::ExitCode::FAILURE;
    }

    info!(version = env!("CARGO_PKG_VERSION"), "Starting viaduct");

    let (db_tx, db_rx) = mpsc::channel(256);
    if let Err(e) = database::spawn_db_worker(db_rx) {
        error!(?e, "Failed to spawn database worker; aborting");
        return glib::ExitCode::FAILURE;
    }

    let (sync_tx, sync_rx) = mpsc::channel(256);
    if let Err(e) = database::spawn_sync_worker(sync_rx) {
        error!(?e, "Failed to spawn sync worker; aborting");
        return glib::ExitCode::FAILURE;
    }

    let account = Arc::new(
        viaduct::block_on_runtime(Account::new(db_tx, sync_tx))
            .expect("Failed to initialize Account"),
    );

    let app = adw::Application::builder()
        .application_id("org.virinvictus.Viaduct")
        .build();

    let account_for_activate = account.clone();
    app.connect_activate(move |app| build_ui(app, account_for_activate.clone()));

    app.run()
}

fn init_tracing() {
    let mut default_level = "info";
    if std::env::args().any(|arg| arg == "--debug") {
        default_level = "debug,viaduct=trace";
        viaduct::set_debug_mode(true);
    }
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    fmt().with_env_filter(filter).init();
}

/// Set `GSETTINGS_SCHEMA_DIR` to the source-tree `data/` (where `build.rs`
/// dropped `gschemas.compiled`) so `gio::Settings::new` finds our schema in
/// dev builds. Skipped if the user has already exported the variable, or if
/// we're running from an installed prefix where the system schema dir wins.
fn ensure_schema_dir() {
    if std::env::var_os("GSETTINGS_SCHEMA_DIR").is_some() {
        return;
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let schema_dir = std::path::Path::new(manifest_dir).join("data");
    if schema_dir.join("gschemas.compiled").exists() {
        // SAFETY: called before tokio runtime / gio init / threads spawn.
        unsafe {
            std::env::set_var("GSETTINGS_SCHEMA_DIR", &schema_dir);
        }
    }
}

fn build_ui(app: &adw::Application, account: Arc<Account>) {
    if let Some(settings) = viaduct::preferences::settings() {
        viaduct::preferences::apply_color_scheme(&settings);
        viaduct::preferences::apply_fonts(&settings);
    }
    let window = ui::window::ViaductWindow::new(app, account);
    window.present();
}
