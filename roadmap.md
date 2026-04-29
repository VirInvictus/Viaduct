# viaduct — Roadmap

What's done, what's next, what's deferred. Sequenced for maximum performance, full NetNewsWire **local-account and Inoreader** feature parity, and a strictly defined 1.0 Wayland/Linux release. Updated as of v1.0.0.

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
*   **World-Class Typography**: Transitioning to a single, strictly neutered WebKit instance to render flawless CSS themes (Sepia, Gruvbox, Midnight) without the memory bloat of unconstrained browser engines. This will enforce comfortable reading widths (e.g., `max-width: 44em`) and native hover states for links.
*   **Article Settings Popover**: Future UX addition to allow users to dynamically adjust text scaling, line height, and themes without leaving the reading context.
*   **Engine Separation**: Refactor into a Cargo workspace, pulling `database`, `network`, and `parser` into a `viaduct-core` headless crate to enforce architectural boundaries.

### 4. Parsing Engine (`quick-xml` & `serde_json`)
Parallel parsing architecture using `quick-xml` (RSS/Atom/OPML) and `serde_json` (JSON Feed, RSS-in-JSON) inside spawned `tokio` tasks.

*   **HTML metadata extraction**: an `HTMLMetadataExtractor` scans raw HTML for hidden `<link rel="alternate" type="application/rss+xml">` feeds when the user adds a bare website URL.
*   **Permissive date parsing**: port of NNW's `RSDateParser` to handle the zoo of real-world date formats found in feeds.
*   **Reader View** (optional, RAM-gated — see Phase 10): local Readability port to scrape full text from truncated RSS feeds. NNW's version calls a remote Mercury service; ours must be local-only.
*   **Render Caching**: Aggressive disk-level caching of extracted Reader View HTML (or raw outputs) to speed up subsequent loads, avoiding the RAM-pinning mistakes seen in other clients.
*   **Thumbnail Extraction**: Implement a `video_thumbnail_extractor` to fetch preview images for YouTube/Vimeo links found in feeds, sprucing up the timeline view.

---

## Memory Budget (hard target)

*   **Idle**: 100–300 MB after full sync + image cache warm.
*   **Peak**: < 500 MB during any operation, including Reader View extraction and WebKit rendering.

Every phase ends with a `heaptrack` / `massif` profiling checkpoint. Features that can't hit the budget get gated, deferred, or cut. The "Single Neutered WebKit Instance" rule ensures we achieve perfect typography (reusing NNW CSS variables) without allowing Javascript execution or multiple WebProcess bloat.

---

## Phase 0: Project Scaffolding
- [x] Cargo project skeleton with GTK4 / libadwaita / tokio / rusqlite / reqwest / quick-xml / ammonia.
- [x] Module layout: `database/`, `network/`, `parser/`, `ui/`.
- [x] Basic `adw::Application` window that opens and closes cleanly.
- [ ] Meson build wrapper so `cargo` output can be packaged as Flatpak without hand-rolled plumbing. *(deferred to Phase 17)*
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
- [x] Folder selection aggregates articles from all child feeds (newest-first). *(`fetch_folder_articles` in `window.rs`)*

## Phase 6: World-Class Typography via Neutered WebKit
- [ ] Transition from `GtkTextView` to exactly ONE heavily constrained `WebKitWebView` instance.
- [ ] Enforce strict `WebKitSettings`: disable JavaScript, plugins, WebGL, and Local Storage.
- [ ] Implement strict Content Security Policies (CSP) to block background network requests and trackers.
- [ ] Port NetNewsWire theme bundles (`.nnwtheme`) as CSS variables to achieve flawless typography (Sepia, Gruvbox, etc.).
- [ ] Enforce typographic constraints: `max-width: 44em` for the reading column.
- [x] Baseline: `ammonia` whitelist configuration (strip scripts, iframes, inline styles, trackers).
- [ ] Link handling: wire WebKit's hover signals to a native `UrlOverlay` (Phase 2 improvements).
- [ ] Image `<img>` tags: handled natively by WebKit with disk-cache support.

## Phase 7: Asset & Memory Management
- [x] `IconImageCache` analog: favicon fetch → disk cache in `$XDG_CACHE_HOME/viaduct/favicons/`. *(`network::cache::ImageCache::favicon`)*
- [x] `ColorHash` port for the AdwAvatar fallback color. *(`network::cache::color_for`)*
- [x] Inline image fetch worker: async download, disk cache in `$XDG_CACHE_HOME/viaduct/images/`, 250-entry LRU. *(`network::cache::ImageCache::image` — `GdkTexture` decode happens at the GTK call site so the LRU stays `Send`)*
- [x] Wire favicons into `SidebarDataSource` row binder. AdwAvatar with auto-derived accent + initial; on bind, async-fetch settings → favicon URL → ImageCache → `GdkTexture` → `set_custom_image`. Stale-row guard via avatar text comparison. *(`spawn_favicon_fetch` in `sidebar.rs`)*
- [x] Wire inline `<img>` tags in `article::render_html` to `gtk::Picture` widgets at `TextChildAnchor` positions, async-loaded via `ImageCache`. Display-width capped at 600px. *(`insert_image_anchor` in `article.rs`)*
- [x] **Memory checkpoint (DB + parser path)**: `src/bin/mem_check.rs` runs 500 feeds × 10 articles through the real single-writer worker and reports `VmHWM`. Current release-build peak: **29 MB** (hard budget 500 MB). Run via `cargo run --release --bin mem_check`.
- [ ] **Memory checkpoint (image-cache warmup)**: same scenario but with 500 favicons + ~50 inline images warmed. Requires either a live network pass or a synthetic image-serving fixture; deferred until we have a refresh button wired in Phase 9 and can exercise the full path end-to-end.

## Phase 8: Smart Feeds & Search
- [x] Smart-feed sidebar rows (Today / All Unread / Starred) that drive timeline fetches via the wired sidebar-selection handler in `ViaductWindow::wire_models`. *(The `SmartFeedDelegate` trait abstraction was deferred — port-first, the three queries already exist on `LocalAccount` and the window dispatches by name.)*
- [x] "Today" (articles arrived/published since midnight local time), "All Unread" (aggregate across all feeds), "Starred" (retained indefinitely). *(`fetch_today_articles`, `fetch_unread_articles`, `fetch_starred_articles`)*
- [x] `GtkSearchEntry` wired to FTS5 `MATCH`, ranked by `rank`. *(`window.ui` SearchBar + `ViaductWindow::wire_search`; query escaped + prefix-wrapped before MATCH)*
- [x] Live-filter as the user types, debounced 150ms via `glib::timeout_add_local_once`.
- [x] Search-scope toggle (current feed vs. all feeds). *(`scope_toggle` in `window.ui`; tracks `selected_feed_id` from sidebar selection and passes to `search_articles_with_snippets`)*
- [x] Snippet extraction (`snippet()` FTS5 function) surfaced in the timeline preview row. *(`ArticleNode::with_snippet` + `populate_timeline_with_snippets`)*

## Phase 9: Keyboard Spatial Navigation
- [x] `Space`: smart-read — port of NNW `scrollOrGoToNextUnread`. Pages article down if scrollable; else marks current read and jumps to next unread. *(`act_smart_read` in `window.rs`)*
- [x] `Shift+Space`: page article up. *(`act_scroll_up`)*
- [x] `n` / `Down` / `j`: next article (skipping read). `-` / `Up` / `k`: previous unread. *(`advance_unread`, NNW key + roadmap aliases stacked)*
- [x] `r` / `m`: toggle read. `Shift+m`: mark unread and advance. `s`: toggle star. *(`act_toggle_read`, `act_mark_unread_advance`, `act_toggle_star`)*
- [x] `b` / `Enter`: open in default browser via `gio::AppInfo::launch_default_for_uri`. *(`act_open_in_browser`; matches NNW's two bindings)*
- [x] `Ctrl+r` fetch now (drives `LocalAccountRefresher` against full OPML), `Ctrl+f` focus search, `F9` toggle sidebar (collapses outer `AdwNavigationSplitView`). *(`act_refresh`, `act_focus_search`, `act_toggle_sidebar`)*
- [x] `Ctrl+k` mark all read; `l` mark all read & advance; `o` mark older read. *(`act_mark_all_read`, `act_mark_all_read_advance`, `act_mark_older_read`)*
- [x] `Ctrl+?` accelerator cheat-sheet — `gtk::ShortcutsWindow` from declarative `shortcuts.ui`. *(`act_shortcuts`)*

## Phase 10: Reader View (RAM-gated)
- [x] Prototype with the `readability` crate (html5ever + scoring heuristics) in a `tokio::task::spawn_blocking`. *(`src/ui/reader_view.rs`)*
- [x] Extracted HTML funnels through the same `ammonia` → `GtkTextBuffer` pipeline from Phase 6. *(`render_article_body` calls `article::render_html` with the extracted body)*
- [x] Triggered on-demand via toolbar toggle in the article pane's `AdwHeaderBar`. *(`reader_btn` template child)*
- [x] Respect the `readerViewAlwaysEnabled` per-feed flag from `FeedSettingsDatabase`. *(timeline-selection handler resolves it async and pre-toggles the button)*
- [x] Input HTML capped at `INPUT_SIZE_CAP` (5 MB) before extraction; oversized pages return `ReaderError::TooLarge`.
- [ ] **Memory gate verification**: re-run `mem_check` with a Reader-View extraction over a representative corpus (current harness only exercises DB path). If a single extraction pushes peak RSS above 500 MB, fall back to the subprocess-isolation pattern documented in `reader_view.rs`.

## Phase 11: Enclosures & Media
- [x] Parse `<enclosure>` and `<media:content>` / `<media:thumbnail>` in RSS; `<link rel="enclosure">` in Atom; `attachments[]` in JSON Feed. All flow into `Article.attachments` (persisted as JSON column on the `articles` table). *(`enclosure_from_attrs`, `media_attachment_from_attrs`, `parse_jf_attachments`, `AtomLinkRel::Enclosure`)*
- [x] UI indicator on the timeline row: audio/video/image icon based on the first attachment's MIME type, with a count badge when there's more than one. *(`media_icon` + `media_count` in `setup_timeline_list_view`)*
- [x] `Ctrl+Enter` hotkey opens the first attachment via `gio::AppInfo::launch_default_for_uri` — the system's MIME handler decides the player. Users with `mpv` configured as their audio/video default get mpv (and yt-dlp via mpv's url-handler); we don't hard-code a player. *(`act_open_enclosure`)*
- [x] No in-app playback — system media players do the work.

### Parser fidelity follow-ups (bundled here because they all require extending `ParsedFeed`/`ParsedItem`)
- [x] Capture RSS channel `<image><url>` as the feed-level icon URL; refresher persists into `FeedSettings.icon_url`. *(in_channel_image state in `parse_rss`, `fetcher.rs::refresh_one_feed` persists `parsed.icon_url`)*
- [x] Capture Atom `<icon>` and `<logo>` (icon wins over logo per NNW). *(in `parse_atom`, prefer `icon_url.or(logo_url)`)*
- [x] Capture RSS `<language>` and Atom `<feed xml:lang>` into `ParsedFeed.language`. Stored on the parsed feed; reading-pane direction-tagging deferred (no v1.0 user need yet).
- [ ] Atom `type="xhtml"` `<content>` and `<summary>` currently parse as plain text. NNW uses `XMLSAXParser.captureRawInnerContent` to hand back the raw inner bytes; `quick-xml` has no direct analog. Options: (a) accept degraded fidelity, (b) detect `type="xhtml"` at Start and buffer raw input until matching End, (c) switch to `html5ever` for those subtrees.

## Phase 12: OPML Import & Export

User-facing OPML exchange. The internal `parse_opml` / `serialize_opml` path already exists (Phase 2); this phase exposes it via menu actions and makes sure the merge semantics match NNW's behavior. NNW counterparts: `.netnewswire/Shared/Exporters/OPMLExporter.swift`, `.netnewswire/Mac/MainWindow/OPML/ImportOPMLWindowController.swift`, `.netnewswire/Mac/MainWindow/OPML/ExportOPMLWindowController.swift`, `.netnewswire/Modules/Account/Sources/Account/OPMLNormalizer.swift`.

- [x] **Import menu action** wired into the primary menu (`menu_btn` in `window.ui`). Opens a `gtk::FileDialog` (GTK4 integrates `org.freedesktop.portal.FileChooser` automatically). *(`act_import_opml` in `window.rs`)*
- [x] **Export menu action**. Serializes the current `OpmlFile` via the new `serialize_account_opml` to a user-chosen path. Hand-rolled writer matches NNW `OPMLExporter.OPMLString` byte-for-byte (XML decl, `<!-- OPML generated by viaduct -->` comment, tab indent, attribute order, `description=""`, `version="RSS"`).
- [x] **Import merge semantics**: union by `xmlUrl`. Never overwrites existing `edited_name`. Imported folders merge into existing folders by name; missing folders are created. *(`merge_opml` in `database/opml.rs`)*
- [x] Faithful port of NNW `OPMLNormalizer`: nameless-folder wrappers promote children up; named folders flatten descendants into a single feed list (folders-only-one-level-deep); feeds dedup by `xmlUrl` within their parent. *(`normalize_opml` in `database/opml.rs`)*
- [x] After import, `LocalAccountRefresher::refresh_feeds` kicks against just the newly-added feeds. *(`refresh_specific_feeds` in `window.rs`)*
- [x] Progress/completion feedback via `adw::Toast` through a new `AdwToastOverlay` template child (`toast_overlay` in `window.ui`). Shows feed-count on import success, file path on export success, and human-readable failure copy.
- [x] Failure modes: malformed OPML → toast + `tracing::warn` (`parse_opml` errors bubble through `LocalAccount::import_opml`). File-dialog dismissal is silent.

## Phase 13: System Integration & Theming
- [x] `libadwaita` system color scheme follow (Dark / Light / Auto). *(`adw::StyleManager` driven by the `color-scheme` GSetting; user override via Preferences dropdown — `apply_color_scheme` in `src/preferences.rs`)*
- [x] Desktop notifications for new article counts per refresh cycle. *(`gio::Notification` via `Application::send_notification`, gated by the `notifications-on-refresh` GSetting; tally happens in `run_refresh_with_tally` then dispatches via `dispatch_refresh_notification` on the GTK thread. NNW's per-feed `newArticleNotificationsEnabled` toggle deferred — needs a feed-inspector pane we don't have yet, so v0.8.0 ships a single global toggle.)*
- [x] `GSettings` schema for user prefs. *(`data/org.virinvictus.Viaduct.gschema.xml` declares `color-scheme`, `notifications-on-refresh`, `refresh-interval-minutes`, `retention-days`, `font-monospace`, `font-serif`. v0.8.0 wires the first two into behavior; the remaining three are reserved for the phases that introduce their consumers (Phase 14 retention, future cron Phase 17, future font-override pane).)*
- [ ] ~~Background daemon via `xdg-desktop-portal` Background API for cron-based refresh while the UI is closed.~~ *(Moved to Phase 17 — needs `ashpd` + Flatpak manifest plumbing; naturally pairs with the sandbox work there. NNW's mac equivalent (`NSBackgroundActivityScheduler`) has no Linux analog without a portal client.)*

## Phase 14: The Pruning Engine
- [x] Port NNW's `RetentionStyle.feedBased` — local and Inoreader accounts prune against the feed's own content. *(`update_feed` per-feed prune since v0.5.2; startup cleanup chain — `delete_articles_not_in_feeds` + `delete_old_statuses` + `vacuum_databases` — added in v0.9.0 via `LocalAccount::cleanup_at_startup`)*
- [x] Age-based purge: articles older than the configured retention (default 30 days) and not starred are deleted. *(`UpdateFeed` now plumbs `retention_days` through; `act_refresh` and `refresh_specific_feeds` in `window.rs` read `retention-days` from GSettings via `current_retention_days` and pass it to `LocalAccountRefresher::new`. Schema default 30; range `[1, 365]`.)*
- [x] Unread status does not protect from pruning (NNW semantics). *(per-update prune in `update_feed` checks `starred` only; `delete_old_statuses` also checks `starred = 0` only)*
- [x] Periodic `VACUUM` on startup; coalesced with OPML load so it runs off the main thread. *(new `ArticlesDbOp::Vacuum` and `SettingsDbOp::Vacuum`; `LocalAccount::vacuum_databases` fires both serially through the worker channel from the startup cleanup chain — runs on the worker thread, GTK never blocks)*
- [x] Cascade: deleting an article row triggers FTS5 row deletion via the existing trigger. *(`articles_ad` trigger in `setup_schema` since v0.5.2 — covered by the existing FTS5 invariant)*

## Phase 15: Inoreader Sync Engine (NetNewsWire Port)
- [x] Refactor `LocalAccount` into a generic `Account` structure backed by an `AccountDelegate` trait, strictly mirroring NetNewsWire's abstraction.
- [x] Port `SyncDatabase` from `.netnewswire/Modules/SyncDatabase/` to track remote sync state (article UUIDs, read/starred sync status).
- [x] Port `InoreaderAccountDelegate` and the API caller from `.netnewswire/Modules/Account/` into Rust. We will strictly port the Swift networking logic, token-bucket approach, and batch sync rules.
- [x] Secure credentials storage via `libsecret` (porting the concepts from `.netnewswire/Modules/Secrets/`).

## Phase 16: QA, Test Suites, & Debug Mode
- [x] Implement a `viaduct --debug` flag or environment variable that enables verbose `tracing` logs, disables database WAL truncation (for easier inspection), and adds a hidden Debug menu to the UI.
- [ ] **Workspace Refactoring**: Migrate to a Cargo workspace, pulling `database`, `network`, and `parser` into a `viaduct-core` headless crate to enforce architectural boundaries and support zero-UI testing.
- [ ] **Video Thumbnail Extraction**: Implement a `video_thumbnail_extractor` to fetch and cache preview images for YouTube/Vimeo links found in feeds.
- [x] Build out integration test suites for the refresh pipeline (`LocalAccountRefresher` and Inoreader sync), mocking the network layer.
- [x] Implement UI test harnesses to ensure sidebar/timeline/article pane state transitions are rock solid.
- [x] Port any remaining applicable unit tests from `.netnewswire/Tests/` and module test directories.
- [x] **DB worker supervision**: `database::worker::spawn_db_worker` spawns a plain `std::thread::spawn` — if the worker panics every future op is orphaned. Add a supervisor loop that restarts the worker (with a small backoff) and logs the panic.

## Phase 17: Flatpak Sandboxing & 1.0 Release
- [x] Flatpak manifest: `network` permission only; no `--filesystem=home`. OPML I/O entirely via `org.freedesktop.portal.FileChooser`.
- [x] AppStream metadata (`appdata.xml`), icons at all required sizes, desktop entry.
- [x] Reproducible build verified against the target Flathub runtime.
- [x] Background daemon via `xdg-desktop-portal` Background API for cron-based refresh while the UI is closed (moved from Phase 13). Adds `ashpd` for the portal client; pairs with the manifest's `org.freedesktop.portal.Background` entry.
- [ ] Tag `1.0.0` and submit to Flathub.

---

## Success Criteria (v1.0)

1. Import a 500-feed OPML file without hanging the GTK main thread.
2. Background engine fetches + parses 1,000 new articles while the user smoothly scrolls the list view.
3. Idle memory sits between 100–300 MB; peak stays under 500 MB through every supported operation.
4. FTS5 search across all cached articles returns results in under 50 ms on a 50k-article corpus.
5. Full compliance with GNOME HIG and libadwaita 1.7 styling.
6. Flathub-accepted Flatpak build running in a strict sandbox.
ox.
