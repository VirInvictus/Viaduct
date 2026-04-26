// Compile GSettings schemas in `data/` so dev runs of `cargo run` find them
// via `GSETTINGS_SCHEMA_DIR` (set in `main.rs`). For installed Flatpak builds
// the manifest will install + compile schemas into the runtime's prefix and
// this step is redundant.
//
// We don't fail the build if `glib-compile-schemas` is missing — that lets
// CI runners without GLib dev tools at least produce a binary. The runtime
// will simply fall back to default values when `gio::Settings::new` fails.

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset");
    let schema_dir = Path::new(&manifest_dir).join("data");

    println!("cargo:rerun-if-changed=data/org.virinvictus.Viaduct.gschema.xml");

    if !schema_dir.exists() {
        return;
    }

    match Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => println!(
            "cargo:warning=glib-compile-schemas exited with status {status}; gio::Settings will fall back to defaults"
        ),
        Err(e) => println!(
            "cargo:warning=could not run glib-compile-schemas ({e}); gio::Settings will fall back to defaults"
        ),
    }
}
