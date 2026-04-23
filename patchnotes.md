# viaduct — Patch Notes

## v0.5.1 — Licensing, Attribution, & Upstream Sync

Maintenance update to align the project with NetNewsWire's licensing and update core reference materials.

### Added
- **MIT License Transition:** Switched project license from GPL-3.0 to MIT for full compatibility with NetNewsWire. Updated `Cargo.toml`, `LICENSE`, and `README.md`.
- **Comprehensive Attributions:** Created `ATTRIBUTIONS.md` to formally credit Brent Simmons, Ranchero Software, and the diverse Rust ecosystem libraries powering the engine.
- **License Headers:** Injected MIT license headers into all Rust source files and UI XML templates.
- **Upstream Sync:** Updated the `.netnewswire` reference folder to commit `ec06277`. 
    - *Architectural Note:* Documented the major upstream refactor of `StripHTML` from legacy C to native Swift, including new performance benchmarks for whitespace collapsing.
    - *Assets:* Synchronized new themes (*Biblioteca*, *Tiqoe Dark*, *Verdana Revival*).

### Fixed
- **Runtime Reconstruction:** Restored the global `tokio` runtime (`static RUNTIME`) and asynchronous infrastructure in `src/main.rs` that was inadvertently reverted during maintenance. This includes the proper initialization of the database worker and the `LocalAccount` orchestrator via `block_on`.

### Changed
- **CLAUDE.md Verbosity:** Expanded the architectural guidelines to include detailed notes on the latest upstream changes to assist with future porting efforts.

## v0.5.0 — Assets, Smart Feeds, & Search

Phases 7 and 8 are complete, bringing native image caching, Smart Feeds, and FTS5 search to the application while maintaining strict Wayland memory budgets.

### Added
- **Image & Favicon Caching:** Built an async `tokio` network worker (`src/network/cache.rs`) to fetch and disk-cache assets using MD5 hashes (`$XDG_CACHE_HOME/viaduct/`).
- **Memory Strictness:** Implemented a fixed-size LRU cache (250 items) for in-memory `gdk::Texture` objects to strictly guarantee the 500 MB peak RAM budget, compensating for the lack of a reliable low-memory broadcast on Linux.
- **AdwAvatar Fallback:** Integrated `libadwaita`'s `AdwAvatar` to natively generate color-hashed circular widgets with feed initials for missing favicons, replacing NetNewsWire's custom `ColorHash` and CoreGraphics code.
- **Smart Feeds:** Ported NNW's `SmartFeedDelegate` architecture to Rust, implementing "Today", "All Unread", and "Starred" pseudo-feeds right into the GTK sidebar.
- **FTS5 Search:** Added a `GtkSearchBar` with a debounced (~150ms) entry that executes native SQLite `MATCH` queries against the FTS5 virtual table without blocking the UI thread.

## v0.4.0 — Native Reader & Inoreader Pivot

Phase 6 is complete, and the project scope has been officially expanded to support Inoreader.

### Added
- **Native HTML Pipeline:** Built a native `GtkTextTag` string walker (`src/ui/article.rs`) to safely render sanitized HTML inside a `GtkTextView`.
- **System Typography:** Applied GNOME HIG spacing, system typography, and programmatic styling to map structural HTML (h1-h6, p, blockquote, lists, code) into GTK primitives without relying on WebKit.
- **Interactive Links:** Attached gesture controllers to parse buffer coordinates, extracting and launching URLs natively via `gio::AppInfo::launch_default_for_uri`.

### Changed
- **Project Scope Expansion:** Officially expanded the roadmap, spec, and Claude system prompt to include support for Inoreader as the sole supported remote sync backend. Inserted Phase 14 for Inoreader integration into the roadmap.
- **Refactor Plan:** Documented that we will port NNW's `Account` / `AccountDelegate` abstractions to restructure the `LocalAccount` work from earlier phases to handle `InoreaderAccountDelegate`.

## v0.3.1 — Sidebar Glue & Delegation

- **Sidebar Delegate:** Added `SidebarTreeControllerDelegate` port from NetNewsWire to `src/ui/sidebar.rs`. This correctly implements the `TreeControllerDelegate` trait, handling the logic of turning the parsed OPML (Folders and standalone Feeds) and Smart Feeds into the `TreeNode` structure that the `TreeController` manages. This completes the loop between the OPML on disk and the GTK Sidebar.

## v0.3.0 — UI Skeleton & Coalescing Primitives

Phase 5 has begun, establishing the foundational UI structure and translating NetNewsWire's coalescing and tree-management objects into GTK4 primitives.

### Added
- **Application Window:** Scaffolded `AdwApplicationWindow` and a responsive `AdwNavigationSplitView` natively using `.ui` XML for a 3-pane layout (`src/ui/window.ui`).
- **Coalescing Primitives:** Ported `BatchUpdate` (`src/ui/batch.rs`) and `CoalescingQueue` (`src/ui/coalescing_queue.rs`) from `RSCore` into Rust equivalents using `gio` timeouts and `glib::MainContext` affinity. Prevents UI notification storms.
- **Fetch Request Queue:** Ported `FetchRequestQueue` (`src/ui/fetch_queue.rs`) to safely cancel stale `tokio::task` futures during rapid sidebar navigation.
- **Tree Controller & Sidebar Data Source:** Ported the `RSTree` module (`Node.swift`, `TreeController.swift`) into `src/ui/tree.rs` using `glib::Object` subclasses, and created `SidebarDataSource` (`src/ui/sidebar.rs`) to map the domains `TreeNode` model into a `gio::ListStore` for the sidebar.

## v0.0.1 — Scaffolding

Phase 0 ground-work. The window still opens empty, but the plumbing underneath is now load-bearing.

### Added
- **XDG paths module** (`src/paths.rs`): honors `$XDG_DATA_HOME` and `$XDG_CACHE_HOME` with proper fallback to `$HOME/.local/share` and `$HOME/.cache`. Exposes resolved paths for the OPML file, both SQLite DBs, and favicon/image caches. `ensure_dirs()` creates the full tree on first launch.
- **Error hierarchy** (`src/error.rs`): `ViaductError` top-level type wrapping `DatabaseError`, `NetworkError`, `ParseError` via `thiserror`. Each layer holds its backend's source error (`rusqlite`, `reqwest`, `quick_xml`, `serde_json`, `url::ParseError`).
- **Structured logging**: `tracing-subscriber` with `EnvFilter` support — `RUST_LOG` now controls verbosity (default `info`).
- **CI pipeline** (`.github/workflows/ci.yml`): Ubuntu 24.04 runner, installs GTK4 / libadwaita / sqlite dev packages, runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all` on every push and PR.

### Changed
- Bumped package version from `1.0.0-dev` to `0.0.1`.
- Added `thiserror` dependency; enabled `env-filter` feature on `tracing-subscriber`.
- Roadmap and spec realigned to NetNewsWire's actual architecture (three-store data layer, OPML feed hierarchy on disk, local-only v1.0 scope, RAM budget 100–300 MB idle / 500 MB peak).

### Deferred
- Meson build wrapper (moved to Phase 14 with the rest of the Flatpak plumbing).

---

## v0.1.0 — The Parsing Crucible

Phase 3 is complete.

### Added
- **Date Parser** (`src/parser/date.rs`): Ported NetNewsWire's zero-allocation `RSDateParser` from Swift to Rust. Handles permissive parsing for W3C / ISO 8601 and RFC 822 / pubDate string formats using raw byte inspection to maintain strict memory budgets. Integrated with `chrono` for precise date and timezone manipulation.
- **XML Parsers** (`src/parser/xml.rs`): Ported NetNewsWire's `RSSParser`, `AtomParser`, and `OPMLParser` to Rust using the zero-allocation `quick-xml` crate.
- **JSON Parsers** (`src/parser/json.rs`): Ported NetNewsWire's `JSONFeedParser` and `RSSInJSONParser` to Rust using `serde_json`.
- **HTML Metadata Extractor** (`src/parser/html.rs`): Extracts `<link>` and `<meta>` tags to find RSS/Atom feeds within raw websites.

## v0.2.0 — Network & Refresh Engine

Phase 4 is complete.

### Added
- **Fetcher (`DownloadSession` analog)** (`src/network/fetcher.rs`): Built a robust HTTP client using `reqwest` (with `rustls-tls` and HTTP/2) that coalesces duplicate URL requests in flight, preventing redundant bandwidth usage during mass updates. Also implements exponential backoffs honoring HTTP 429 `Retry-After` headers.
- **LocalAccountRefresher** (`src/network/fetcher.rs`): Orchestrates feed downloading by checking `FeedSettingsDatabase` for conditional GETs (`If-None-Match`, `If-Modified-Since`) and honors `max-age` Cache-Control to skip unnecessary hits. Automatically skips feeds hitting 304 Not Modified. Also ported NetNewsWire's 25-hour special-case cutoff.
- **Cross-Thread Wiring**: Hooked up `tokio::sync::mpsc::UnboundedSender` to emit `ArticleChanges` back to the UI layer safely from background parser tasks.

### Refactored
- **Code Cleanup**: Conducted a comprehensive bug sweep and refactor pass. Resolved over 60 Clippy lints including nested `if` collapses, redundant `match` blocks, and manual range loop optimizations.
- **Type Aliasing**: Simplified the network layer's internal state management by introducing the `FetchSender` type alias for complex broadcast channels.
- **API Standards**: Implemented `Default` traits for core service structs (`Fetcher`) and ensured strict alignment with the Rust 2024 edition formatting standards via `cargo fmt`.

---

## v1.0.0 (Planned)

Target: NetNewsWire local-account and Inoreader feature parity on GNOME 50. See [roadmap.md](roadmap.md) for the phase-by-phase plan.
n.
