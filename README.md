<p align="center">
  <img src="logo.svg" alt="viaduct" width="420">
</p>

<p align="center">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Language-Rust-blue" alt="Language: Rust"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="License: MIT"></a>
  <a href="https://ko-fi.com/bdkl"><img src="https://img.shields.io/badge/support-Ko--fi-ff5f5f?logo=kofi" alt="Ko-fi"></a>
</p>

---

# viaduct

A fast, native GNOME RSS reader achieving full feature-parity with NetNewsWire. viaduct is a direct translation of the NetNewsWire architectural philosophy into the Linux ecosystem via Rust and GTK4, built specifically for local and Inoreader reading.

## Why this exists

Modern readers often rely on web engines that consume excessive memory. viaduct completely isolates the network and data layers from the UI thread using Rust's async ecosystem. It handles massive subscription lists without locking the UI thread. It targets idle RAM of 100–300 MB with a hard 500 MB peak ceiling, trading ultra-minimalist asceticism for rock-solid performance, offline image caching, and local data sovereignty.

## Features

| Feature | Description |
|---------|-------------|
| **Smart Feeds** | Virtual feeds generated dynamically via SQLite queries (Today, All Unread, Starred). |
| **Local OPML** | Robust local-first architecture for importing and exporting your feed lists. |
| **Neutered WebKit Pipeline** | Single, strictly sandboxed WebKit instance for flawless CSS typography without the memory bloat. |
| **Keyboard Shortcuts** | Standard desktop accelerators, prioritizing spatial navigation. |
| **The Pruning Engine** | Automated database vacuum and age-based article purging to maintain performance. |

## Installation

viaduct is packaged as a Flatpak-first application. The released Flatpak bundles every system dependency and is the recommended install path. Source builds need the development headers below.

### Build dependencies (source)

| Library | Version | Fedora package | Debian/Ubuntu package |
|---|---|---|---|
| GTK4 | ≥ 4.16 | `gtk4-devel` | `libgtk-4-dev` |
| libadwaita | ≥ 1.7 | `libadwaita-devel` | `libadwaita-1-dev` |
| WebKitGTK | 6.0 | `webkitgtk6.0-devel` | `libwebkitgtk-6.0-dev` |
| SQLite (bundled) | — | — | — |
| OpenSSL replacement | — | rustls (vendored) | rustls (vendored) |

WebKitGTK 6.0 powers the article reading pane. It's run in a heavily-neutered configuration (JavaScript disabled, no plugins, no local storage, strict CSP), used only for CSS typography fidelity — see `spec.md` §2.2 for the threat-model writeup.

```bash
# Fedora 43+
sudo dnf install gtk4-devel libadwaita-devel webkitgtk6.0-devel

# Debian/Ubuntu (24.04+)
sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev

# Build via Cargo (workspace root):
cargo build --release      # binary lands at target/release/viaduct
cargo run --release        # launch the GTK app

# OR: build via Meson — produces a system-style install layout for
# packagers / Flathub. Mirrors the path Flatpak takes during build:
sudo dnf install meson ninja-build glib2-devel    # Fedora
sudo apt install meson ninja-build libglib2.0-bin # Debian/Ubuntu
meson setup builddir --prefix=/usr -Dbuildtype=release
meson compile -C builddir
sudo meson install -C builddir
```

## Architecture

- **Engine Port:** Fully isolates network and data layers from the UI thread via `tokio` multi-thread runtime.
- **Data Layer:** Backed by `rusqlite` with WAL mode and FTS5 enabled, using a double-database segregation strategy.
- **UI Thread:** GTK4 Main UI Thread reads from local models and sends commands down crossbeam channels.

## Support

If this saved you time, consider [buying me a coffee](https://ko-fi.com/bdkl).