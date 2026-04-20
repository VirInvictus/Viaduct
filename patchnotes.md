# viaduct — Patch Notes

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

Target: NetNewsWire local-account feature parity on GNOME 50. See [roadmap.md](roadmap.md) for the phase-by-phase plan.
