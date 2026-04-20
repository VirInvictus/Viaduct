# Viaduct — Roadmap

What's done, what's next, what's deferred. Sequenced for maximum performance, full NetNewsWire feature parity (local-only), and a strictly defined 1.0 Wayland/Linux release. Updated as of v1.0.0-dev.

---

## Architecture Strategy & Implementation Plan

Viaduct is a direct translation of NetNewsWire's architecture to Linux/Wayland, leveraging Rust and GNOME 50+ standards (GTK4 + libadwaita 1.7). The goal is feature parity for local-only reading, local-first data ownership, and strict background threading that avoids the UI locks and high memory footprint common in web-based readers.

### 1. Data Layer: Double-Database Segregation (Raw `rusqlite`)
NetNewsWire avoids UI locking and bloat by dividing its database into separate SQLite files. We will bypass ORMs and use raw `rusqlite` to guarantee 1:1 parity with their schemas (WAL mode, FTS5 optimizations), adapted for a strictly local-only workflow.
*   **ArticlesDatabase:** Stores `articles`, `statuses`, `authors`, and `search` (FTS5 index) tables.
*   **AccountSettingsDatabase:** Stores `feeds` and `folders` hierarchy.
*   **Concurrency:** A dedicated `tokio` worker thread will act as the single serialization point for SQLite writes, acting as a `DatabaseQueue` analog to ensure the GTK UI thread never blocks on I/O.

### 2. State Management & Network (Raw `gtk-rs` & `tokio`)
We mirror NetNewsWire's explicit MVC architecture instead of using reactive frameworks.
*   **Async Reactor:** `tokio` handles the `reqwest` connection pool and background queuing.
*   **Main Loop Yielding:** State changes (e.g., `ArticleChanges` batches with `new`, `updated`, and `deleted` sets) will be piped directly back to the GTK main loop using `glib::MainContext::channel`, mimicking the behavior of Swift's `@MainActor`.
*   **RSWeb Translation:** Our network layer must implement strict `ETag` and `If-Modified-Since` headers to prevent bandwidth waste, alongside `HTTPResponse429` (rate limiting) handling and concurrent connection limits with exponential backoff.

### 3. UI Layer (Standard `.ui` XML & GNOME 50)
The interface will be constructed declaratively using standard `.ui` GTK Builder XML files for maximum native tooling compatibility.
*   **Adaptive Layout:** GNOME 50 `AdwNavigationSplitView` will handle the responsive three-pane layout (sidebar, timeline, article body).
*   **List Recycling:** The timeline will strictly enforce memory efficiency by binding a custom `gio::ListModel` directly to `GtkListView` via a `GtkSignalListItemFactory`.
*   **Native Text Rendering:** No WebKit. Parsed HTML bodies will be stripped via `ammonia` and translated natively into `GtkTextTag` structures rendered in a `GtkTextView`.

### 4. Parsing Engine (`quick-xml` & `serde_json`)
A parallel parsing architecture using `quick-xml` (RSS/Atom) and `serde_json` (JSON Feed) inside spawned `tokio` tasks. 
*   **HTML Metadata Extraction:** We will implement an `HTMLMetadataDownloader` to scan raw HTML for hidden `<link rel="alternate">` RSS feeds.
*   **Article Extractor:** We will build a Mercury/Readability-style "Reader View" to scrape full text from truncated RSS feeds natively.

---

## Phase 1: Database Segregation & The SQLite Engine
- [ ] **Articles Database:** Map the `ArticlesDatabase` module (`ArticlesTable`, `StatusesTable`, `AuthorsTable`, and `SearchTable` FTS5 index) using `rusqlite`.
- [ ] **Account Settings Database:** Map the `AccountSettingsDatabase` to store `Feed` and `Folder` structures.
- [ ] **Thread Safety:** Implement WAL mode across both databases and establish a `DatabaseQueue` analog (e.g., a dedicated `tokio` worker thread or `r2d2` pool) to serialize writes and guarantee GTK UI thread immunity.

## Phase 2: The RSWeb Translation (Network & Caching)
- [ ] Spin up the `tokio` runtime for background network I/O.
- [ ] **Conditional GETs:** Replicate `HTTPConditionalGetInfo`—implement strict `ETag` and `Last-Modified` (If-Modified-Since) headers for `reqwest` to eliminate redundant feed parsing and bandwidth waste.
- [ ] **Rate Limiting:** Replicate NNW's `HTTPResponse429` handling and `DownloadSession` concurrent connection limits with exponential backoff.
- [ ] **Main Loop Yielding:** Wire up `glib::MainContext::channel` to emit NetNewsWire's `ArticleChanges` (new, updated, deleted sets) directly to the GTK event loop, replacing Swift's `@MainActor` UI updates.

## Phase 3: The Parsing Crucible (RSParser)
- [ ] Translate the `RSParser` architecture: build parallel parsing tracks for RSS, Atom, JSON Feed, and RSS-in-JSON using `quick-xml` and `serde_json` inside dedicated `tokio::spawn` tasks.
- [ ] **HTML Metadata Extraction:** Implement the `HTMLMetadataDownloader` to parse raw HTML for `<link rel="alternate" type="application/rss+xml">` when users add raw website URLs.
- [ ] Build defensive fallback logic for malformed XML, missing dates, and broken CDATA tags.

## Phase 4: Local OPML Architecture
- [ ] Build the local-only engine abstraction based on NNW's `LocalAccount`.
- [ ] Implement the `OPMLNormalizer` to safely ingest and flatten nested `RSOPMLItem` feeds from OPML files.
- [ ] Ensure adding 500 feeds simultaneously batches database writes across the `AccountSettingsDatabase` and `ArticlesDatabase` to prevent UI freezing.

## Phase 5: The UI Skeleton & Recycling (GNOME 50+)
- [ ] Scaffold the `AdwApplicationWindow` and the three-pane `AdwNavigationSplitView` (standardized for GNOME 50+ libadwaita).
- [ ] Implement strict widget recycling via `GtkListView` and `GtkSignalListItemFactory` for the timeline pane.
- [ ] Bind the factory directly to a custom `gio::ListModel` backed by the `ArticlesDatabase` to ensure rendering 10,000 articles consumes identical RAM to rendering 10.

## Phase 6: Native Text Translation & The Article Extractor
- [ ] Pass extracted article bodies through `ammonia` for strict sanitization.
- [ ] Write the HTML-to-`GtkTextBuffer` parser, mapping structural tags to GTK typography using GNOME 50 styling conventions.
- [ ] **Reader View:** Implement the "Article Extractor" (Mercury/Readability style) to parse full text for truncated RSS feeds.

## Phase 7: Asset & Memory Management
- [ ] Intercept `<img src>` tags during buffer translation.
- [ ] **Favicons:** Implement the `IconImageCache` to fetch, scale, and cache feed favicons cleanly to disk.
- [ ] Dispatch async workers to fetch inline images, load them as native `GdkTexture` objects, and cache them locally.
- [ ] Profile memory to ensure idle RAM stays within the 200-300MB target.

## Phase 8: Smart Feeds & Spatial Filtering
- [ ] Wire up the `SmartFeeds` logic for "Today", "All Unread", and "Starred", recalculating dynamically off the `StatusesTable`.
- [ ] Wire up `GtkSearchEntry` for real-time filtering backed by the FTS5 `SearchTable` index.

## Phase 9: Keyboard Spatial Navigation
- [ ] Implement comprehensive keyboard accelerators.
- [ ] `Space` for smart reading (scroll page, jump to next unread article at bottom).
- [ ] `j`/`k` list navigation.
- [ ] `m` to toggle read status, `s` to star/save.

## Phase 10: Enclosures & Media
- [ ] Parse media enclosures (podcasts/video).
- [ ] Provide a clean UI indicator for media attachments and a hotkey to pipe URLs to system players (`mpv`, `yt-dlp`).

## Phase 11: System Integration & Theming (GNOME 50+)
- [ ] Integrate `libadwaita` system color scheme preferences (Dark/Light).
- [ ] Implement `AppNotifications` / `libnotify` for new article counts.
- [ ] Build the background daemon (via portals) for cron-based fetching while the UI is closed.

## Phase 12: The Pruning Engine
- [ ] Implement an automated database vacuum matching NetNewsWire's `RetentionStyle` heuristics.
- [ ] Execute background routines to drop unread/unstarred articles older than user-defined limits to protect the memory target.

## Phase 13: Flatpak Sandboxing & 1.0 Release
- [ ] Write the Flatpak manifest, locking down permissions to strictly network and required XDG paths.
- [ ] Finalize AppStream metadata and icons.
- [ ] Tag 1.0.0 and submit to Flathub.