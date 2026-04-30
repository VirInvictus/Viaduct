// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

// v2.6.12: replace glibc malloc with mimalloc for the binary. The
// v2.6.11 SQLite WAL containment bounded the file→RSS contribution,
// but RSS still drifted up cycle-over-cycle due to glibc's per-thread
// arena retention — freed memory stays in the arena and never returns
// to the OS. mimalloc returns aggressively, typically 30–50% lower
// steady-state RSS on bursty workloads exactly like a refresh cycle.
// Global-allocator declarations affect the binary only; `viaduct-core`
// stays allocator-agnostic.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use adw::prelude::*;
use gtk::glib;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt};
use viaduct::database::accounts::Account;
use viaduct::{database, fonts, paths, ui};

fn main() -> glib::ExitCode {
    // v2.6.14: tune mimalloc's idle-page purge before its global heap
    // initializes. Default `MIMALLOC_PURGE_DELAY` is 1000 ms — pages
    // freed by the application sit in mimalloc's pools for a full
    // second before being decommitted. At 100 ms (10×) the OS reclaims
    // memory much faster after each refresh cycle, which is the cadence
    // we care about for the run-in-background long-session case.
    // `unsafe` because Rust 2024 marks `env::set_var` unsafe (it mutates
    // process-shared state without sync); we run before any thread is
    // spawned so this is fine. Skip if the user has already exported the
    // variable so a power user can tune higher / disable purging.
    tune_mimalloc();
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
    if let Err(err) = fonts::install_bundled() {
        // Font install is best-effort — log but don't abort. Themes will
        // still render (browser falls back to system fonts when the bundled
        // ones aren't installed).
        tracing::warn!(?err, "failed to install bundled fonts");
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

    // v2.5.0: wire the system-tray indicator. Starts an SNI service when
    // the `run-in-background` GSetting is on so the user always has a
    // visible "viaduct is running" cue + a Quit menu item, regardless
    // of window-visibility state. Listens for GSetting changes too —
    // flipping the toggle on / off in Preferences immediately
    // shows / hides the icon. No-op when the schema isn't installed.
    viaduct::tray::wire(&app);

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

/// v2.6.14: install mimalloc tuning env vars before the global heap
/// initializes. `MIMALLOC_PURGE_DELAY` controls how long freed pages
/// sit in mimalloc's internal pools before being decommitted to the
/// OS. Default 1000 ms; we drop to 100 ms so a refresh cycle's
/// transient allocations come back to the OS quickly. Other defaults
/// stay in place. User-supplied env vars win.
fn tune_mimalloc() {
    if std::env::var_os("MIMALLOC_PURGE_DELAY").is_none() {
        // SAFETY: called before any thread is spawned and before any
        // tokio runtime is built. The `env::set_var` 2024-edition
        // unsafe marker exists for the multi-thread case.
        unsafe {
            std::env::set_var("MIMALLOC_PURGE_DELAY", "100");
        }
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
    // After v1.5.0's workspace split, the binary crate sits at
    // `viaduct/` and the schema source still lives at the repo root's
    // `data/`. Walk one level up to reach it in dev builds.
    let schema_dir = std::path::Path::new(manifest_dir)
        .parent()
        .map(|p| p.join("data"))
        .unwrap_or_else(|| std::path::Path::new(manifest_dir).join("data"));
    if schema_dir.join("gschemas.compiled").exists() {
        // SAFETY: called before tokio runtime / gio init / threads spawn.
        unsafe {
            std::env::set_var("GSETTINGS_SCHEMA_DIR", &schema_dir);
        }
    }
}

fn build_ui(app: &adw::Application, account: Arc<Account>) {
    // Phase 17 D-Bus re-summon: if a window already exists for this
    // application (run-in-background mode, hidden after close, then
    // re-activated via the dock icon or `gtk-launch`), present it
    // instead of building a second one. Without this branch, opening
    // viaduct while it's hidden silently spawns a second window and
    // the user ends up with two of them.
    if let Some(existing) = app
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<ui::window::ViaductWindow>().ok())
    {
        existing.present();
        // v2.6.3 diagnostics: cancel the hidden-state RSS ticker and
        // log the re-show snapshot. Must run before the timeline
        // repopulate so the logged delta is "hidden idle → just
        // re-shown", not "hidden idle → re-shown + repopulated".
        existing.unhide_from_background();
        // Repopulate from the still-selected sidebar item so the user
        // lands back on the feed they were reading, plus any articles
        // that arrived while the window was hidden.
        existing.reload_current_timeline();
        return;
    }

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
/* Video thumbnails get rounded corners and a subtle border so they\n\
 * read as inline media without dominating the row. */\n\
.viaduct-timeline-thumb {\n\
    border-radius: 6px;\n\
    background-color: alpha(currentColor, 0.05);\n\
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
