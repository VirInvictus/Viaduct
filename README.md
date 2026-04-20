<p align="center">
  <img src="logo.svg" alt="Viaduct" width="420">
</p>

<p align="center">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Language-Rust-blue" alt="Language: Rust"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-GPLv3-yellow.svg" alt="License: GPLv3"></a>
  <a href="https://ko-fi.com/bdkl"><img src="https://img.shields.io/badge/support-Ko--fi-ff5f5f?logo=kofi" alt="Ko-fi"></a>
</p>

---

# Viaduct

A fast, native GNOME RSS reader achieving full feature-parity with NetNewsWire. Viaduct is a direct translation of the NetNewsWire architectural philosophy into the Linux ecosystem via Rust and GTK4, built specifically for local-only, private reading.

## Why this exists

Modern readers often rely on web engines that consume excessive memory. Viaduct completely isolates the network and data layers from the UI thread using Rust's async ecosystem. It handles massive subscription lists without locking the UI thread. It targets an idle memory footprint of 250MB, trading ultra-minimalist asceticism for rock-solid performance, offline image caching, and local data sovereignty.

## Features

| Feature | Description |
|---------|-------------|
| **Smart Feeds** | Virtual feeds generated dynamically via SQLite queries (Today, All Unread, Starred). |
| **Local OPML** | Robust local-first architecture for importing and exporting your feed lists. |
| **Native Render Pipeline** | No WebKit. Uses native GTK widgets for rendering parsed HTML. |
| **Keyboard Shortcuts** | Standard desktop accelerators, prioritizing spatial navigation. |
| **The Pruning Engine** | Automated database vacuum and age-based article purging to maintain performance. |

## Installation

Viaduct is packaged as a Flatpak-first application.

```bash
# Build via Cargo
cargo build --release
```

## Architecture

- **Engine Port:** Fully isolates network and data layers from the UI thread via `tokio` multi-thread runtime.
- **Data Layer:** Backed by `rusqlite` with WAL mode and FTS5 enabled, using a double-database segregation strategy.
- **UI Thread:** GTK4 Main UI Thread reads from local models and sends commands down crossbeam channels.

## Support

If this saved you time, consider [buying me a coffee](https://ko-fi.com/bdkl).