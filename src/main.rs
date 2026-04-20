pub mod database;
pub mod network;
pub mod parser;
pub mod ui;

use adw::prelude::*;
use gtk::glib;
use tracing::info;

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt::init();
    info!("Starting Viaduct...");

    let app = adw::Application::builder()
        .application_id("org.virinvictus.Viaduct")
        .build();

    app.connect_activate(build_ui);

    app.run()
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Viaduct")
        .default_width(1200)
        .default_height(800)
        .build();

    window.present();
}