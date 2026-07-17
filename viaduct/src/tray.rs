// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! v2.5.0 — System tray indicator for run-in-background mode.
//!
//! Wires a `ksni`-backed StatusNotifierItem when the `run-in-background`
//! GSetting is on, so closing the window doesn't make the app vanish
//! invisibly. The tray icon shows up wherever the desktop hosts an SNI
//! tray (KDE / XFCE / Cinnamon / MATE natively; GNOME via the
//! AppIndicator extension). Left-click = `Application::activate()` (i.e.
//! re-summon the existing window via the same path the dock icon uses
//! in `main.rs build_ui`); right-click menu = "Show viaduct" / "Quit
//! viaduct". Quit goes through `gio::Application::quit()` which bypasses
//! our `connect_close_request` hide-instead-of-quit branch — that's the
//! whole point of the menu item.
//!
//! Lifecycle: the tray runs whenever `run-in-background` is enabled,
//! regardless of window-visibility state. Flip the GSetting off and the
//! handle's `shutdown()` is called; flip it on and a fresh service
//! spawns. Initial start at app startup reads the current GSetting
//! value.
//!
//! The `ksni::TrayService` runs on its own tokio runtime thread inside
//! ksni's worker. Menu callbacks fire there, so we use a
//! `tokio::sync::mpsc` channel to deliver `TrayAction`s back to the GTK
//! main thread, where a `glib::spawn_future_local` task awaits and
//! dispatches.

use gtk::gdk_pixbuf::PixbufLoader;
use gtk::glib;
use gtk::prelude::*;
use ksni::menu::StandardItem;
use ksni::{MenuItem, Tray, TrayMethods};
use std::cell::RefCell;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy)]
enum TrayAction {
    Show,
    Quit,
}

struct ViaductTray {
    tx: tokio::sync::mpsc::UnboundedSender<TrayAction>,
    /// Pre-decoded ARGB32 pixmaps, one entry per resolution. SNI hosts
    /// pick the closest size to their tray slot. Empty when decoding
    /// failed (we still install `icon_name` so themable hosts have a
    /// fallback). v2.6.6.
    icons: Vec<ksni::Icon>,
    /// Hicolor-structured directory containing
    /// `hicolor/<size>/apps/<icon_name>.png`. Reported via
    /// `Tray::icon_theme_path` so the GNOME AppIndicator extension —
    /// which mostly ignores `icon_pixmap` and resolves through GTK's
    /// icon theme — picks up our PNGs without needing the SVG to be
    /// installed system-wide. Empty string when the install failed
    /// (silently falls back to the host's icon-theme search path).
    /// v2.6.7.
    icon_theme_path: String,
}

impl Tray for ViaductTray {
    fn title(&self) -> String {
        "viaduct".into()
    }

    fn id(&self) -> String {
        "org.virinvictus.Viaduct".into()
    }

    fn icon_name(&self) -> String {
        // Resolves against the system icon theme. Works for installed
        // builds (Flatpak / `meson install`) where the SVG at
        // `data/icons/hicolor/scalable/apps/` lands in the runtime's
        // theme dir. Pre-v2.6.6 dev builds fell back to the SNI host's
        // placeholder here; v2.6.6 also implements `icon_pixmap` below
        // with bytes embedded at compile time, so the tray icon
        // renders correctly regardless of install state.
        "org.virinvictus.Viaduct".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn icon_theme_path(&self) -> String {
        self.icon_theme_path.clone()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayAction::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show viaduct".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(TrayAction::Show);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit viaduct".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

thread_local! {
    /// Active service handle. `None` when run-in-background is off (no
    /// tray showing). Lives on the GTK main thread; the inner
    /// `ksni::Handle` is `Send` but we don't need to share it — start /
    /// stop calls happen exclusively here.
    static TRAY_HANDLE: RefCell<Option<ksni::Handle<ViaductTray>>> =
        const { RefCell::new(None) };
}

/// Wire the tray: install the GSetting change listener, start the
/// service if run-in-background is currently on, and attach the
/// receiver loop that dispatches `TrayAction`s to the GTK main thread.
/// Call once from `main.rs build_ui` after the application is built.
pub fn wire(app: &gtk::Application) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<TrayAction>();

    // Receiver loop on the GTK main thread.
    let app_for_loop = app.clone();
    glib::spawn_future_local(receive_loop(rx, app_for_loop));

    let Some(settings) = crate::preferences::settings() else {
        // Schema not installed (dev env without `glib-compile-schemas`).
        // Tray simply won't fire — the app still works.
        return;
    };

    if crate::preferences::run_in_background(&settings) {
        start_service(tx.clone());
    }

    let tx_for_changes = tx.clone();
    settings.connect_changed(
        Some(crate::preferences::keys::RUN_IN_BACKGROUND),
        move |s, _| {
            if crate::preferences::run_in_background(s) {
                start_service(tx_for_changes.clone());
            } else {
                stop_service();
            }
        },
    );
}

async fn receive_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<TrayAction>,
    app: gtk::Application,
) {
    while let Some(action) = rx.recv().await {
        match action {
            TrayAction::Show => {
                // v2.6.3: log RSS at the tray re-summon path so the
                // overnight diagnostic timeline shows tray activations.
                let (rss_mb, peak_mb) = crate::read_memory_mb();
                tracing::info!(rss_mb, peak_mb, "diag: tray Show");
                // Same path the dock-icon click takes (Phase 17 D-Bus
                // re-summon). build_ui will find the existing window
                // and `present()` it, or build a new one if the user
                // ran the binary without one.
                app.activate();
            }
            TrayAction::Quit => {
                // v2.6.3: terminal RSS snapshot (the at-exit summary in
                // main.rs reports VmHWM; this captures the live VmRSS
                // at the user-visible click moment).
                let (rss_mb, peak_mb) = crate::read_memory_mb();
                tracing::info!(rss_mb, peak_mb, "diag: tray Quit");
                // gio::Application::quit() ends the main loop without
                // firing connect_close_request, so the run-in-background
                // hide-instead-of-quit branch is bypassed cleanly.
                stop_service();
                app.quit();
            }
        }
    }
}

fn start_service(tx: tokio::sync::mpsc::UnboundedSender<TrayAction>) {
    TRAY_HANDLE.with(|cell| {
        if cell.borrow().is_some() {
            // Already running — flip-on-while-on is a no-op.
            return;
        }
        let icons = cached_icons();
        let icon_theme_path = cached_icon_theme_path().unwrap_or_default();
        let tray = ViaductTray {
            tx,
            icons,
            icon_theme_path,
        };
        // ksni's `spawn` consumes the tray and yields a Handle on the
        // current Tokio runtime (the global one we install in `main`).
        // Has to run from a Tokio context; the wire() call site is on
        // the GTK thread but the global runtime is reachable from
        // anywhere via the runtime-builder we stored.
        let handle = match crate::block_on_runtime(async move { tray.spawn().await }) {
            Ok(handle) => handle,
            Err(e) => {
                tracing::warn!(?e, "ksni tray spawn failed; sys-tray disabled");
                return;
            }
        };
        cell.borrow_mut().replace(handle);
    });
}

/// v2.6.6: app icon bytes embedded at compile time. Two resolutions
/// give SNI hosts a choice — KDE / XFCE trays usually pick the smaller,
/// HiDPI extensions tend to want the larger. Stored at `docs/` rather
/// than `data/` because the meson install for shipped builds relies on
/// the SVG (themable, scalable) — the PNGs are exclusively the dev /
/// fallback path used by `icon_pixmap`.
const ICON_PNG_256: &[u8] = include_bytes!("../../docs/icon-256.png");
const ICON_PNG_512: &[u8] = include_bytes!("../../docs/icon-512.png");

/// Decode the embedded PNGs once per process and cache the resulting
/// `ksni::Icon` vec. `gtk::gdk_pixbuf::PixbufLoader` is `!Send`, so the
/// decode has to happen on the GTK main thread — `wire()` calls
/// `start_service()` from there, so we're safe. The returned `Icon`
/// struct is plain data (Vec<u8> + i32s) and crosses freely to the
/// tokio thread that ksni runs on.
///
/// Returns an empty Vec on decode failure; the SNI host falls back to
/// `icon_name` lookup in that case (works for installed builds).
fn cached_icons() -> Vec<ksni::Icon> {
    static ICONS: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    ICONS
        .get_or_init(|| {
            let mut out = Vec::with_capacity(2);
            for (label, bytes) in [("icon-256", ICON_PNG_256), ("icon-512", ICON_PNG_512)] {
                match decode_icon(bytes) {
                    Ok(icon) => out.push(icon),
                    Err(e) => {
                        tracing::warn!(label, error = %e, "tray icon decode failed");
                    }
                }
            }
            out
        })
        .clone()
}

/// PNG → ARGB32. PixbufLoader gives RGBA bytes plus dimensions; the
/// SNI spec wants ARGB in network byte order (big-endian), which on
/// little-endian hosts means swapping each pixel to BGRA in memory —
/// equivalent to `rotate_right(1)` on the four-byte pixel.
fn decode_icon(png_bytes: &[u8]) -> Result<ksni::Icon, String> {
    let loader = PixbufLoader::with_type("png").map_err(|e| e.to_string())?;
    loader.write(png_bytes).map_err(|e| e.to_string())?;
    loader.close().map_err(|e| e.to_string())?;
    let pixbuf = loader.pixbuf().ok_or("PixbufLoader returned no pixbuf")?;
    let width = pixbuf.width();
    let height = pixbuf.height();
    if !pixbuf.has_alpha() {
        // The bundled PNGs are RGBA. If a future asset drops alpha
        // we'd need a different conversion path — bail loudly.
        return Err("pixbuf is missing alpha channel".into());
    }
    if pixbuf.bits_per_sample() != 8 {
        return Err("pixbuf bits_per_sample is not 8".into());
    }
    // SAFETY: read_pixel_bytes returns a glib::Bytes owning the pixel
    // buffer; we copy out of it before the Pixbuf drops.
    let rgba = pixbuf.read_pixel_bytes();
    let mut data = rgba.to_vec();
    if data.len() % 4 != 0 {
        return Err(format!(
            "pixel buffer length {} not multiple of 4",
            data.len()
        ));
    }
    // RGBA → ARGB (network byte order). Per ksni docs + the SNI spec.
    for px in data.chunks_exact_mut(4) {
        px.rotate_right(1);
    }
    Ok(ksni::Icon {
        width,
        height,
        data,
    })
}

/// v2.6.7: write the embedded PNGs to a hicolor-shaped directory and
/// return its path so `Tray::icon_theme_path` can hand it to the SNI
/// host. Cached once per process via `OnceLock`.
///
/// Why this exists despite `icon_pixmap` working in v2.6.6: the GNOME
/// AppIndicator Shell extension (the only way GNOME sees SNI tray
/// items) mostly **ignores** the `IconPixmap` D-Bus property and
/// resolves through GTK's icon theme using `IconName` + `IconThemePath`.
/// We hand it a private theme path containing our PNGs so the icon
/// renders correctly on GNOME without polluting the user's system
/// icon theme.
///
/// Layout:
///
/// ```text
///   $XDG_CACHE_HOME/viaduct/tray-icons/
///   └── hicolor/
///       ├── 256x256/apps/org.virinvictus.Viaduct.png
///       └── 512x512/apps/org.virinvictus.Viaduct.png
/// ```
///
/// Returns `None` (caller falls back to empty `icon_theme_path`) when
/// any I/O step fails — the SNI host then falls back to the system
/// icon-theme search path or our `icon_pixmap` payload.
fn cached_icon_theme_path() -> Option<String> {
    static THEME_PATH: OnceLock<Option<String>> = OnceLock::new();
    THEME_PATH.get_or_init(install_tray_icon_theme).clone()
}

fn install_tray_icon_theme() -> Option<String> {
    const ICON_NAME: &str = "org.virinvictus.Viaduct";
    // v2.6.8: GTK's IconTheme spec requires a per-theme `index.theme`
    // declaring which subdirs hold which size buckets. Without it,
    // `St.IconTheme.lookup_icon_for_scale` (the GNOME Shell wrapper
    // that AppIndicator drives — see `appIndicator.js _getIconData`)
    // can't enumerate our 256x256 / 512x512 subdirs and falls back to
    // the placeholder. `Hidden=true` keeps the theme out of any
    // user-facing theme picker; `Type=Fixed` says "this directory
    // holds icons at exactly this size" rather than threshold/scalable.
    const INDEX_THEME: &str = "[Icon Theme]
Name=Viaduct Tray
Comment=viaduct tray indicator icons (auto-generated; safe to delete)
Hidden=true
Directories=256x256/apps,512x512/apps

[256x256/apps]
Size=256
Context=Applications
Type=Fixed

[512x512/apps]
Size=512
Context=Applications
Type=Fixed
";

    let base = match crate::paths::cache_dir() {
        Ok(p) => p.join("tray-icons"),
        Err(e) => {
            tracing::warn!(?e, "tray icon theme: cache_dir resolution failed");
            return None;
        }
    };
    let theme_root = base.join("hicolor");
    if let Err(e) = std::fs::create_dir_all(&theme_root) {
        tracing::warn!(?theme_root, ?e, "tray icon theme: create_dir_all failed");
        return None;
    }
    let index_path = theme_root.join("index.theme");
    let needs_index = match std::fs::read_to_string(&index_path) {
        Ok(existing) => existing != INDEX_THEME,
        Err(_) => true,
    };
    if needs_index && let Err(e) = std::fs::write(&index_path, INDEX_THEME) {
        tracing::warn!(?index_path, ?e, "tray icon theme: index.theme write failed");
        return None;
    }

    for (size, bytes) in [(256, ICON_PNG_256), (512, ICON_PNG_512)] {
        let dir = theme_root.join(format!("{size}x{size}/apps"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(?dir, ?e, "tray icon theme: create_dir_all failed");
            return None;
        }
        let path = dir.join(format!("{ICON_NAME}.png"));
        let needs_write = match std::fs::metadata(&path) {
            Ok(m) => m.len() != bytes.len() as u64,
            Err(_) => true,
        };
        if needs_write && let Err(e) = std::fs::write(&path, bytes) {
            tracing::warn!(?path, ?e, "tray icon theme: write failed");
            return None;
        }
    }
    Some(base.to_string_lossy().into_owned())
}

fn stop_service() {
    TRAY_HANDLE.with(|cell| {
        if let Some(handle) = cell.borrow_mut().take() {
            handle.shutdown();
        }
    });
}
