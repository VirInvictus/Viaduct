// Compile GSettings schemas in `data/` so dev runs of `cargo run` find them
// via `GSETTINGS_SCHEMA_DIR` (set in `main.rs`), and compile the bundled icon
// GResource (`data/icons/viaduct.gresource.xml`) into the binary. For
// installed Flatpak builds the manifest will install + compile schemas into
// the runtime's prefix and this step is redundant.
//
// We don't fail the build if the glib tools are missing — that lets CI
// runners without GLib dev tools at least produce a binary. The runtime will
// simply fall back to default values when `gio::Settings::new` fails, and to
// theme/GTK-builtin icons when the resource bundle is absent.

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset");
    // Workspace layout (v1.5.0+): the binary crate sits at `viaduct/` and
    // the schema source lives at the repo root's `data/` so `data/` is one
    // level up from CARGO_MANIFEST_DIR.
    let data_dir = Path::new(&manifest_dir)
        .parent()
        .map(|p| p.join("data"))
        .unwrap_or_else(|| Path::new(&manifest_dir).join("data"));

    println!("cargo:rerun-if-changed=../data/io.github.virinvictus.Viaduct.gschema.xml");
    println!("cargo:rerun-if-changed=../data/icons/viaduct.gresource.xml");
    println!("cargo:rerun-if-changed=../data/icons/hicolor");

    if !data_dir.exists() {
        return;
    }

    match Command::new("glib-compile-schemas").arg(&data_dir).status() {
        Ok(status) if status.success() => {}
        Ok(status) => println!(
            "cargo:warning=glib-compile-schemas exited with status {status}; gio::Settings will fall back to defaults"
        ),
        Err(e) => println!(
            "cargo:warning=could not run glib-compile-schemas ({e}); gio::Settings will fall back to defaults"
        ),
    }

    // The icon bundle: glib-compile-resources writes into OUT_DIR and
    // `main.rs` registers it with `gio::resources_register` before any
    // widget exists. See `data/icons/viaduct.gresource.xml` for why.
    // The empty target is seeded first so a machine without the glib
    // tool still compiles: `main.rs`'s include_bytes! resolves, the
    // empty resource fails to load there, and icon lookups fall back to
    // the theme chain with a warning.
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR unset");
    let compiled = Path::new(&out_dir).join("viaduct-icons.gresource");
    let _ = std::fs::write(&compiled, b"");
    match Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(data_dir.join("icons"))
        .arg("--target")
        .arg(&compiled)
        .arg(data_dir.join("icons").join("viaduct.gresource.xml"))
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => println!(
            "cargo:warning=glib-compile-resources exited with status {status}; icon lookups fall back to the theme chain"
        ),
        Err(e) => println!(
            "cargo:warning=could not run glib-compile-resources ({e}); icon lookups fall back to the theme chain"
        ),
    }
}
