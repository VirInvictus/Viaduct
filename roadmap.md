# viaduct — Roadmap

What's done, what's next, what's deferred. Sequenced for maximum performance, full NetNewsWire **local-only** feature parity, and a strictly defined 1.0 Wayland/Linux release. Updated as of v1.0.0-dev.

---

## Architecture Strategy & Implementation Plan

viaduct is a direct translation of NetNewsWire's local-account architecture to Linux/Wayland, leveraging Rust and GNOME 50+ standards (GTK4 + libadwaita 1.7). The goal is feature parity for local reading, local-first data ownership, and strict background threading that avoids the UI locks and high memory footprint common in web-based readers.

Remote sync engines (Feedbin / Miniflux / FreshRSS / CloudKit) are **explicitly out of scope for v1.0**. The app ships as a pure local RSS client.

### 1. Data Layer (raw `rusqlite` + OPML-on-disk)
NetNewsWire splits state across multiple stores and does *not* keep the feed hierarchy in SQL — that detail matters. We replicate the layout 1:1, adapted for local-only:

*   **ArticlesDatabase** (SQLite): `articles`, `statuses`, `authors`, `authorsLookup`, plus an FTS5 `search` virtual table. (NNW uses FTS4; we modernize to FTS5.)
*   **FeedSettingsDatabase** (SQLite): per-feed cache and overrides — `homePageURL`, `iconURL`, `faviconURL`, `editedName`, `contentHash`, conditional-GET info (ETag / Last-Modified), Cache-Control info, authors JSON, folder-relationship JSON, `lastCheckDate`, `readerViewAlwaysEnabled`.
*   **OPML file on disk**: feeds + folder hierarchy live in `$XDG_DATA_HOME/viaduct/local.opml`, saved via a coalesced ~500ms debounced writer (NNW's `OPMLFile` + `CoalescingQueue` pattern).
*   **Concurrency**: a dedicated `tokio` worker task acts as the single serialization point for SQLite writes (NNW's `DatabaseQueue` analog). Both DBs run in WAL mode. The GTK main thread never blocks on I/O.

### 2. Network & Refresh (raw `reqwest` + `tokio`)
We mirror NetNewsWire's explicit refresh pipeline rather than using reactive frameworks.

*   **DownloadSession analog**: `reqwest` connection pool with per-host concurrency caps, coalescing of duplicate URL requests in flight, and a recently-errored feed cooldown list.
*   **Conditional GETs**: ETag / If-Modified-Since values are read from and persisted to `FeedSettingsDatabase` per feed. `HTTP 304` short-circuits parsing entirely.
*   **Rate limiting**: HTTP 429 handling with exponential backoff, honoring `Retry-After`.
*   **Main-loop yielding**: state changes (`ArticleChanges` batches with `new`, `updated`, and `deleted` sets) are piped to the GTK main loop via `glib::MainContext::channel`, mimicking Swift's `@MainActor` delivery. A `BatchUpdate` coalescer suppresses UI notification storms during bulk inserts.

### 3. UI Layer (standard `.ui` XML & GNOME 50)
The interface is constructed declaratively using standard `.ui` GTK Builder XML files for maximum native tooling compatibility.

*   **Adaptive layout**: GNOME 50 `AdwNavigationSplitView` handles the responsive three-pane layout (sidebar → timeline → article body) with graceful collapse on narrow windows.
*   **List recycling**: the timeline strictly enforces memory efficiency by binding a custom `gio::ListModel` directly to `GtkListView` via a `GtkSignalListItemFactory`. Rendering 10,000 articles consumes identical RAM to rendering 10.
*   **FetchRequestQueue analog**: selection changes cancel in-flight timeline fetches so rapid sidebar navigation doesn't pile up stale work.
*   **Native text rendering**: no WebKit. Parsed HTML bodies are sanitized via `ammonia` and translated natively into `GtkTextTag` structures rendered in a `GtkTextView`.

### 4. Parsing Engine (`quick-xml` & `serde_json`)
Parallel parsing architecture using `quick-xml` (RSS/Atom/OPML) and `serde_json` (JSON Feed, RSS-in-JSON) inside spawned `tokio` tasks.

*   **HTML metadata extraction**: an `HTMLMetadataExtractor` scans raw HTML for hidden `<link rel="alternate" type="application/rss+xml">` feeds when the user adds a bare website URL.
*   **Permissive date parsing**: port of NNW's `RSDateParser` to handle the zoo of real-world date formats found in feeds.
*   **Reader View** (optional, RAM-gated — see Phase 10): local Readability port to scrape full text from truncated RSS feeds. NNW's version calls a remote Mercury service; ours must be local-only.

---

## Memory Budget (hard target)

*   **Idle**: 100–300 MB after full sync + image cache warm.
*   **Peak**: < 500 MB during any operation, including Reader View extraction.

Every phase ends with a `heaptrack` / `massif` profiling checkpoint. Features that can't hit the budget get gated, deferred, or cut.

---

## Phase 0: Project Scaffolding
- [x] Cargo project skeleton with GTK4 / libadwaita / tokio / rusqlite / reqwest / quick-xml / ammonia.
- [x] Module layout: `database/`, `network/`, `parser/`, `ui/`.
- [x] Basic `adw::Application` window that opens and closes cleanly.
- [ ] Meson build wrapper so `cargo` output can be packaged as Flatpak without hand-rolled plumbing. *(deferred to Phase 14)*
- [x] XDG path helpers: `$XDG_DATA_HOME/viaduct/` for OPML + DBs, `$XDG_CACHE_HOME/viaduct/` for images + favicons. *(v0.0.1)*
- [x] Error type hierarchy via `thiserror` (`DatabaseError`, `NetworkError`, `ParseError`, `ViaductError`). *(v0.0.1)*
- [x] `tracing-subscriber` configured with env-filter. *(v0.0.1)*
- [x] GitHub Actions CI: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` on Linux. *(v0.0.1)*

## Phase 1: Core Data Models & ArticlesDatabase
- [x] Rust structs: `Feed`, `Folder`, `Article`, `ArticleStatus`, `Author`, `ParsedItem`, `ParsedFeed`.
- [x] `ArticleChanges { new, updated, deleted }` batch type — the delivery unit for all UI updates.
- [x] `ArticlesDatabase` tables: `articles`, `statuses`, `authors`, `authorsLookup`, FTS5 virtual table `search`, and the trigger that deletes search rows on article delete.
- [x] WAL mode + sensible pragmas (`synchronous=NORMAL`, `temp_store=MEMORY`, `mmap_size`).
- [x] **Single-writer task**: a dedicated `tokio` task owns both `rusqlite::Connection` handles; all writes arrive via an `mpsc` channel. GTK thread holds only a `Sender`.
- [x] Core ops: batch-insert articles, upsert statuses, fetch by feed ID / article ID, fetch unread / starred / today, FTS5 search with ranking.
- [x] Unit tests against an in-memory DB for each op.

## Phase 2: FeedSettingsDatabase & OPML Persistence
- [x] `FeedSettingsDatabase` with the NNW schema: `feedURL`, `feedID`, `homePageURL`, `iconURL`, `faviconURL`, `editedName`, `contentHash`, conditional-GET (`lastModified`, `etag`, `dateCreated`), cache-control (`dateCreated`, `maxAge`), `authors` JSON, `folderRelationship` JSON, `lastCheckDate`, `readerViewAlwaysEnabled`.
- [x] `OPMLFile` loader: parses the account's OPML document into a `Feed` + `Folder` tree.
- [x] `OPMLNormalizer`: flattens nested `outline` structures NNW's way (folders can only be one level deep).
- [x] Coalesced save queue: ~500ms debounce, atomic write via temp-file + rename.
- [x] `LocalAccount` struct: owns the OPML file + both SQLite DBs, exposes the account API surface.
- [x] Startup cleanup: `FeedSettingsDatabase::deleteSettingsForFeedsNotIn(feedURLs)` prunes orphaned rows.

## Phase 3: The Parsing Crucible
- [x] RSS 2.0 parser via `quick-xml` (including `<media:*>` namespace for enclosures).
- [x] Atom parser via `quick-xml`.
- [x] JSON Feed parser via `serde_json`.
- [x] RSS-in-JSON parser.
- [x] OPML import/export parser.
- [x] `HTMLMetadataExtractor`: scans HTML for `<link rel="alternate" type="application/rss+xml">` and `<link rel="alternate" type="application/atom+xml">` when users paste a website URL.
- [x] `RSDateParser` analog: permissive parser handling RFC 822, RFC 3339, and the long tail of malformed real-world dates.
- [x] Defensive fallbacks: malformed XML, missing required fields, broken CDATA, wrong encoding declarations.
- [x] Test suite against the corpus of feed samples in `netnewswire/Modules/RSParser/Tests` (cherry-picked).

## Phase 4: Network & Refresh Engine
- [x] `reqwest` client with `rustls-tls`, HTTP/2, user-agent string, per-host connection cap.
- [x] `DownloadSession` analog: coalesces duplicate URL requests, tracks recently-errored feeds with cooldown.
- [x] Conditional-GET path: inject `If-None-Match` + `If-Modified-Since` from `FeedSettingsDatabase`; on 200, persist new headers; on 304, short-circuit parsing.
- [x] `CacheControlInfo` persistence: honor `max-age` to skip feeds still within freshness window.
- [x] HTTP 429 handling with exponential backoff + `Retry-After` header.
- [x] `LocalAccountRefresher` port: orchestrate feed list → download → parse → diff against DB → emit `ArticleChanges`.
- [x] `glib::MainContext::channel` wiring: `ArticleChanges` delivered to the GTK main loop.
- [x] 25-hour special-case cutoff for feeds flagged as high-frequency (NNW's `specialCaseCutoffDate`).

## Phase 5: UI Skeleton + Coalescing Primitives
- [x] `AdwApplicationWindow` + three-pane `AdwNavigationSplitView` scaffolded in `.ui` XML.
- [x] `BatchUpdate` analog: a coalescer that suppresses UI notifications during bulk DB writes and flushes on transaction commit.
- [x] `FetchRequestQueue` analog: cancellable timeline-fetch pipeline so rapid sidebar clicks don't pile up stale work.
- [x] Sidebar: `GtkListView` bound to a `gio::ListModel` backed by the OPML tree, with expandable folder rows.
- [x] Timeline: `GtkListView` + `GtkSignalListItemFactory` + custom `gio::ListModel` backed by `ArticlesDatabase` paging. Title, source, date, 2-line preview.
- [x] Article pane: placeholder `GtkTextView` (populated in Phase 6).
- [x] Unread-count badges on sidebar rows, recalculated off `StatusesTable` deltas.

## Phase 6: Native HTML → GtkTextBuffer Rendering
- [ ] `ammonia` whitelist configuration (strip scripts, iframes, inline styles, trackers).
- [ ] HTML walker that maps structural tags to `GtkTextTag` instances: `h1`-`h6`, `p`, `blockquote`, `pre`, `code`, `strong`, `em`, `a`, `ul`/`ol`/`li`, `hr`.
- [ ] System-font typography with monospace override for `pre`/`code`; GNOME 50 styling conventions.
- [ ] Link handling: click → `xdg-open` (also bound to Enter key per spec).
- [ ] Image `<img>` tags register an anchor; actual image fetch lives in Phase 7.

## Phase 7: Asset & Memory Management
- [ ] `IconImageCache` analog: favicon fetch → scale → disk cache in `$XDG_CACHE_HOME/viaduct/favicons/`.
- [ ] Favicon generator fallback (NNW `ColorHash` + `FaviconGenerator`): hashed color + first letter for feeds with no icon.
- [ ] Inline image fetch worker: async download, decode to `GdkTexture`, disk cache in `$XDG_CACHE_HOME/viaduct/images/`, LRU eviction.
- [ ] Image placeholder widget until the texture is ready; no main-thread blocking.
- [ ] **Memory checkpoint**: profile with `heaptrack` under a 500-feed / 5,000-article scenario. Idle must sit 100–300 MB; peak during image warmup must stay under 500 MB. Cut features that bust the budget.

## Phase 8: Smart Feeds & Search
- [ ] `SmartFeedDelegate` analog: virtual feeds with pluggable fetch strategy.
- [ ] "Today" (articles since midnight local time), "All Unread" (aggregate across all feeds), "Starred" (retained indefinitely).
- [ ] `GtkSearchEntry` wired to FTS5 `MATCH` queries with ranking and snippet extraction.
- [ ] Search-scope toggle: current feed vs. all feeds.
- [ ] Live-filter as the user types, debounced to ~150ms.

## Phase 9: Keyboard Spatial Navigation
- [ ] `Space`: smart read — scroll article if not at bottom; otherwise jump to next unread and mark current read.
- [ ] `j` / `Down`: next article. `k` / `Up`: previous article.
- [ ] `m`: toggle read/unread. `s`: toggle star.
- [ ] `Enter`: open in default browser via `xdg-open`.
- [ ] `Ctrl+R`: fetch now. `Ctrl+F`: focus search. `F9`: toggle sidebar.
- [ ] Accelerator cheat-sheet via `Ctrl+?` (GNOME HIG standard).

## Phase 10: Reader View (RAM-gated)
- [ ] Prototype with the `readability` crate (html5ever + scoring heuristics) in a tokio blocking task.
- [ ] Extracted HTML funnels through the same `ammonia` → `GtkTextBuffer` pipeline from Phase 6.
- [ ] Triggered on-demand via hotkey/toolbar only — **never** eagerly on every article.
- [ ] **Memory gate**: if a single extraction pushes peak RSS above 500 MB, either (a) run extraction in a short-lived subprocess for memory isolation, or (b) cut Reader View from v1.0.
- [ ] Respect the `readerViewAlwaysEnabled` per-feed flag from `FeedSettingsDatabase`.

## Phase 11: Enclosures & Media
- [ ] Parse `<enclosure>` and `<media:content>` tags in RSS; `attachments[]` in JSON Feed.
- [ ] UI indicator (audio/video icon) on articles with media attachments.
- [ ] Hotkey: pipe enclosure URL to `mpv` (or `yt-dlp` for services mpv can't handle directly).
- [ ] No in-app playback — system media players do the work.

## Phase 12: System Integration & Theming
- [ ] `libadwaita` system color scheme follow (Dark / Light / Auto).
- [ ] `libnotify` for new article counts per refresh cycle (opt-in per feed via `newArticleNotificationsEnabled`).
- [ ] Background daemon via `xdg-desktop-portal` Background API for cron-based refresh while the UI is closed.
- [ ] `GSettings` schema for user prefs (refresh interval, retention days, font overrides).

## Phase 13: The Pruning Engine
- [ ] Port NNW's `RetentionStyle.feedBased` — local accounts prune against the feed's own content.
- [ ] Age-based purge: articles older than the configured retention (default 30 days) and not starred are deleted.
- [ ] Unread status does not protect from pruning (NNW semantics).
- [ ] Periodic `VACUUM` on startup; coalesced with OPML load so it runs off the main thread.
- [ ] Cascade: deleting an article row triggers FTS5 row deletion via the existing trigger.

## Phase 14: Flatpak Sandboxing & 1.0 Release
- [ ] Flatpak manifest: `network` permission only; no `--filesystem=home`. OPML I/O entirely via `org.freedesktop.portal.FileChooser`.
- [ ] AppStream metadata (`appdata.xml`), icons at all required sizes, desktop entry.
- [ ] Reproducible build verified against the target Flathub runtime.
- [ ] Tag `1.0.0` and submit to Flathub.

---

## Success Criteria (v1.0)

1. Import a 500-feed OPML file without hanging the GTK main thread.
2. Background engine fetches + parses 1,000 new articles while the user smoothly scrolls the list view.
3. Idle memory sits between 100–300 MB; peak stays under 500 MB through every supported operation.
4. FTS5 search across all cached articles returns results in under 50 ms on a 50k-article corpus.
5. Full compliance with GNOME HIG and libadwaita 1.7 styling.
6. Flathub-accepted Flatpak build running in a strict sandbox.
