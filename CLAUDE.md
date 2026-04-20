# Viaduct - AI Agent Architecture & Guidelines

This file (`CLAUDE.md`) serves as the definitive reference for AI agents (Claude, Gemini, etc.) and human contributors working on the **viaduct** project. It outlines the codebase structure, strict design philosophies, and operational instructions.

## 1. Map of the Codebase Structure

viaduct is built in Rust using `gtk4` and `libadwaita`, heavily leveraging `tokio` for async isolation.

### Top-Level Files
* `README.md`, `roadmap.md`, `spec.md`, `patchnotes.md`: Definitive project goals, memory budgets, implementation phases, and application specs. **Always consult these before making architectural changes.**
* `Cargo.toml`: Rust dependencies and workspace configuration.

### `src/` Directory
* `main.rs`: Application entrypoint. Initializes tracing, ensures XDG directories exist, and launches the `adw::Application`.
* `paths.rs`: Resolves XDG directories (`$XDG_DATA_HOME/viaduct`, `$XDG_CACHE_HOME/viaduct`) for SQLite databases, OPML files, and image caches.
* `models.rs`: Core Rust structs representing domain objects (`Feed`, `Folder`, `Article`, `ArticleStatus`, `FeedSettings`, `ParsedItem`, `ArticleChanges`).
* `error.rs`: Centralized error handling using the `thiserror` crate (`ViaductError` wrapping `DatabaseError`, `NetworkError`, etc.).

### Modules
* **`src/database/`**: The SQLite and persistence layer.
  * `articles.rs`: Interfaces with `ArticlesDatabase` (manages articles, read/starred statuses, authors, and FTS5 search indexing).
  * `settings.rs`: Interfaces with `FeedSettingsDatabase` (manages per-feed caches like ETag, Last-Modified, favicons, and overrides).
  * `opml.rs`: Disk-based persistence for the OPML file, which acts as the ultimate source of truth for the feed/folder hierarchy.
  * `worker.rs`: A dedicated, single `tokio` task that exclusively owns SQLite connections. All writes are sent here via `mpsc` channels to prevent main UI thread blocking.
* **`src/network/`**: The HTTP and network layer.
  * `fetcher.rs`: The `reqwest` based feed downloader. Handles Conditional GETs, Rate Limits (429s), and coalescing requests.
  * `cache.rs`: Asynchronous downloading and disk caching of assets (favicons, inline images).
* **`src/parser/`**: The feed parsing crucible.
  * `xml.rs`: Zero-allocation XML parsing using `quick-xml` (RSS/Atom).
  * `json.rs`: JSON Feed parsing using `serde_json`.
  * `html.rs`: Strict HTML sanitization utilizing the `ammonia` crate.
* **`src/ui/`**: The GTK4 / libadwaita native view layer.
  * `window.rs`: The root `AdwApplicationWindow` and `AdwNavigationSplitView` scaffolding.
  * `sidebar.rs`: The feeds and folders list, bound to a `gio::ListModel`.
  * `timeline.rs`: The article list. Uses `GtkListView` and strict `GtkSignalListItemFactory` recycling to keep memory flat regardless of list size.
  * `article.rs`: The reading pane. Translates sanitized HTML into native `GtkTextTag` ranges rendered inside a `GtkTextView`. **WebKit is strictly forbidden.**

---

## 2. The NetNewsWire Porting Philosophy

**CRITICAL INSTRUCTION:** viaduct is a direct, 1:1 architectural translation of the [NetNewsWire](https://netnewswire.com/) **local account** to Linux. 

### Sourcing NetNewsWire Code
Whenever you are tasked with implementing a new feature, fixing a bug, or building a parser, you **MUST NOT build custom or bespoke logic**. Instead, reference the NetNewsWire source code and port the logic directly from Swift to Rust.

**Where to find the source:**
Clone the NetNewsWire repository locally or browse it via GitHub:
`https://github.com/Ranchero-Software/NetNewsWire`

**Key areas to map:**
* **Parsing:** NNW's `Modules/RSParser` -> viaduct `src/parser/`
* **Dates:** NNW's permissive `RSDateParser` -> port to Rust using `chrono`.
* **Data Layer:** NNW's strict separation between `ArticlesDatabase` (SQL), `FeedSettingsDatabase` (SQL), and `OPML` (Disk) must be maintained exactly.
* **Coalescing:** NNW's `CoalescingQueue` and `BatchUpdate` paradigms must be replicated via Rust channels and debouncing.

### When to Deviate (The "Unless It Has To" Rule)
You are strictly forbidden from inventing new logic or architectural patterns **UNLESS** the semantic differences between Swift/Apple Platforms and Rust/Linux/GNOME demand it.
* **Concurrency:** Swift uses Grand Central Dispatch and `@MainActor`. In Rust, we use `tokio` multi-threading, `mpsc` channels, and `glib::MainContext::channel` to pipe data back to the UI thread.
* **UI:** AppKit/UIKit patterns map directly to GTK4 `gio::ListModel` and `GtkListView` recycling.

---

## 3. GNOME 49+ Exclusivity

viaduct is engineered explicitly for **GNOME 49 and above**. 

* **No Backwards Compatibility Hacks:** Do not introduce polyfills, fallback code, or conditional compilation to support GNOME 48 or older.
* **Modern GTK4 / libadwaita:** Always use the latest widgets (e.g., `AdwNavigationSplitView` instead of the deprecated `AdwLeaflet`).
* **Guidelines:** Strictly adhere to the latest GNOME Human Interface Guidelines (HIG). The UI is declarative, using standard `.ui` XML files or constructed natively with libadwaita 1.7+ capabilities.

---

## 4. Agent Operational Constraints

As an AI agent (Gemini, Claude, etc.) working on this repository, you must strictly abide by these rules:

1. **Memory Budget is Supreme:** Idle RAM must sit at 100–300 MB. Peak RAM must *never* exceed 500 MB. Do not propose features (like embedded WebKit browsers or eager full-text extraction) that violate this ceiling.
2. **Never Block the Main Thread:** All network IO, disk IO, XML parsing, and SQLite writing occurs in `tokio` tasks. The GTK main thread only reads data and renders the UI.
3. **No External Sync Engines in v1.0:** Do not add support for Feedbin, Miniflux, or other remote APIs. We only support a raw local OPML account right now.
4. **Assume the NetNewsWire Way:** If a user request is ambiguous, look up how NetNewsWire handles it. That is your default implementation plan.