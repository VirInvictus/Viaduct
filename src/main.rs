pub mod database;
pub mod error;
pub mod models;
pub mod network;
pub mod parser;
pub mod paths;
pub mod ui;

use adw::prelude::*;
use gtk::glib;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt};

fn main() -> glib::ExitCode {
    init_tracing();

    if let Err(err) = paths::ensure_dirs() {
        error!(?err, "failed to create XDG directories; aborting");
        return glib::ExitCode::FAILURE;
    }

    info!(version = env!("CARGO_PKG_VERSION"), "Starting viaduct");

    let app = adw::Application::builder()
        .application_id("org.virinvictus.Viaduct")
        .build();

    app.connect_activate(build_ui);

    app.run()
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

fn build_ui(app: &adw::Application) {
    let window = ui::window::ViaductWindow::new(app);
    window.present();
}
