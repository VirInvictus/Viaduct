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

    // Debug-mode periodic memory ticker (no-op outside --debug).
    viaduct::spawn_debug_memory_ticker();

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

    // Strip our own `--debug` flag from argv before handing it to GTK —
    // GApplication parses argv itself and bails on unknown options.
    // `init_tracing` already pulled the flag's intent into the global
    // DEBUG_MODE atomic, so the filtered argv is fully equivalent.
    let args: Vec<String> = std::env::args().filter(|a| a != "--debug").collect();
    let exit = app.run_with_args(&args);

    // At-exit memory snapshot so every session has a record of its peak.
    // Always-on (not gated on debug mode); cheap and useful for triage.
    log_session_memory_summary();
    exit
}

fn log_session_memory_summary() {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return;
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
    let rss_mb = rss_kb / 1024;
    let peak_mb = hwm_kb / 1024;
    let budget_mb = 500u64;
    if peak_mb > budget_mb {
        tracing::warn!(
            rss_mb,
            peak_mb,
            budget_mb,
            "session exit: peak RSS exceeded 500 MB ceiling"
        );
    } else {
        tracing::info!(rss_mb, peak_mb, budget_mb, "session exit: memory summary");
    }
}

fn init_tracing() {
    let mut default_level = "info,html5ever=error";
    if std::env::args().any(|arg| arg == "--debug") {
        default_level = "debug,viaduct=trace,html5ever=error";
        viaduct::set_debug_mode(true);
    }
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
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
        // v1.2.0-pre1: paint the GTK chrome with the article theme's
        // accent so the whole window visually echoes the reading pane.
        viaduct::preferences::apply_article_theme_accent(&settings);
    }
    apply_sidebar_styling();
    let window = ui::window::ViaductWindow::new(app, account);
    window.present();
}

/// One-time CSS provider for sidebar + timeline refinements (v1.2.0
/// pre3 + pre4). Pill-shaped unread badges, bolder section headers for
/// the SmartFeedGroup row, dimmed-row treatment for read articles in
/// the timeline. Lives at APPLICATION priority so the accent provider
/// (USER+100) still wins for accent-coloured surfaces.
fn apply_sidebar_styling() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let css = "\
.viaduct-sidebar-heading {\n\
    font-weight: 700;\n\
    font-size: 0.78em;\n\
    letter-spacing: 0.07em;\n\
    text-transform: uppercase;\n\
    opacity: 0.65;\n\
}\n\
.viaduct-unread-badge {\n\
    font-size: 0.82em;\n\
    font-weight: 600;\n\
    padding: 1px 8px;\n\
    border-radius: 9999px;\n\
    background-color: alpha(currentColor, 0.10);\n\
    min-width: 18px;\n\
}\n\
listview > row:selected .viaduct-unread-badge {\n\
    background-color: alpha(@accent_fg_color, 0.20);\n\
}\n\
/* Timeline rows: support labels dim slightly more on read items so\n\
 * the eye reads the row state at a glance, beyond the title alone. */\n\
.viaduct-row-read {\n\
    opacity: 0.55;\n\
}\n\
/* DIAGNOSTIC pre4.4 — bright red, bold, large to verify date_label\n\
 * is even getting allocated space in smart-feed timelines. */\n\
.viaduct-debug-date {\n\
    color: red;\n\
    font-weight: 800;\n\
    font-size: 1.1em;\n\
    background-color: yellow;\n\
}\n\
";
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    // Leak — process-wide static CSS, no swap needed.
    Box::leak(Box::new(provider));
}
