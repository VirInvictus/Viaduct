# viaduct — Roadmap

What's done, what's next, what's deferred. Sequenced for maximum performance, full NetNewsWire **local-account and Inoreader** feature parity, and a strictly defined 1.0 Wayland/Linux release. Updated as of v2.6.6.

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
- [x] Meson build wrapper so `cargo` output can be packaged as Flatpak without hand-rolled plumbing. *(shipped v1.5.1; `meson.build` at the repo root, Flatpak manifest switched to `buildsystem: meson`)*
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

## Phase 6: World-Class Typography via Neutered WebKit *(shipped v1.1.0)*
- [x] Transition from `GtkTextView` to exactly ONE heavily constrained `WebKitWebView` instance. *(v1.1.0-pre1; deleted `src/ui/article.rs`, added `src/ui/article_renderer.rs`, `webkit6 = "0.4"` dep)*
- [x] Enforce strict `WebKitSettings`: disable JavaScript, plugins, WebGL, and Local Storage. *(`apply_locked_down_settings` — also disables WebRTC, IndexedDB, app cache, fullscreen, back-forward gestures, JS-window-open, media autoplay)*
- [x] Implement strict Content Security Policies (CSP) to block background network requests and trackers. *(v1.1.0-pre4; `default-src 'none'; img-src viaduct-img: data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'` in `data/themes/page.html`)*
- [x] Port NetNewsWire theme bundles (`.nnwtheme`) as CSS variables to achieve flawless typography. *(v1.1.0-pre2; all 8 themes — Sepia, Appanoose, Biblioteca, Hyperlegible, NewsFax, Promenade, Tiqoe Dark, Verdana Revival — bundled byte-for-byte via `include_str!`. Macro engine `render_with_macros` is a port of NNW's `RSCore.MacroProcessor.processMacros()`. `select_for_dark_mode` pairs Sepia ↔ Tiqoe Dark; user-facing theme picker queued for v1.2.0.)*
- [x] Enforce typographic constraints: `max-width: 44em` for the reading column. *(carried natively by the bundled NNW theme stylesheets)*
- [x] Baseline: `ammonia` whitelist configuration (strip scripts, iframes, inline styles, trackers).
- [x] Link handling: wire WebKit's hover signals to a native `UrlOverlay`. *(v1.1.0-pre5; `gtk::Label` overlay child with `osd` + `caption` style classes, halign=start / valign=end / can-target=False. `install_hover_url_overlay` connects `mouse-target-changed`.)*
- [x] Image `<img>` tags: handled natively by WebKit with disk-cache support. *(v1.1.0-pre4; `viaduct-img://` URI scheme registered on the default `WebContext`. `sanitize_and_rewrite_image_srcs` rewrites every `img@src` http(s)→viaduct-img via `ammonia::Builder::attribute_filter`. The scheme handler clones the URISchemeRequest GObject, hops to tokio for `ImageCache::image()`, then back to GTK to call `request.finish()` with a `MemoryInputStream`.)*
- [x] Memory checkpoint: real-world session peak measured at **292 MB / 500 MB budget**. *(v1.1.0-pre6; at-exit `log_session_memory_summary()` always-on, plus the `--debug` periodic ticker)*

## Phase 7: Asset & Memory Management
- [x] `IconImageCache` analog: favicon fetch → disk cache in `$XDG_CACHE_HOME/viaduct/favicons/`. *(`network::cache::ImageCache::favicon`)*
- [x] `ColorHash` port for the AdwAvatar fallback color. *(`network::cache::color_for`)*
- [x] Inline image fetch worker: async download, disk cache in `$XDG_CACHE_HOME/viaduct/images/`, 250-entry LRU. *(`network::cache::ImageCache::image` — `GdkTexture` decode happens at the GTK call site so the LRU stays `Send`)*
- [x] Wire favicons into `SidebarDataSource` row binder. AdwAvatar with auto-derived accent + initial; on bind, async-fetch settings → favicon URL → ImageCache → `GdkTexture` → `set_custom_image`. Stale-row guard via avatar text comparison. *(`spawn_favicon_fetch` in `sidebar.rs`)*
- [x] Wire inline `<img>` tags in `article::render_html` to `gtk::Picture` widgets at `TextChildAnchor` positions, async-loaded via `ImageCache`. Display-width capped at 600px. *(`insert_image_anchor` in `article.rs`)*
- [x] **Memory checkpoint (DB + parser path)**: `src/bin/mem_check.rs` runs 500 feeds × 10 articles through the real single-writer worker and reports `VmHWM`. Current release-build peak: **29 MB** (hard budget 500 MB). Run via `cargo run --release --bin mem_check`.
- [x] **Memory checkpoint (image-cache warmup)**: `mem_check` now spins up an in-process `tokio::net::TcpListener`-backed HTTP/1.1 fixture serving canned 1 KB favicon and 50 KB image bodies on path-prefix routing (`/fav-*`, `/img-*`), then concurrently warms 500 favicons + 50 images through the real `ImageCache`. 500 favicons exceeds the 250-entry per-kind LRU cap so the eviction path is exercised. Current release-build numbers: post-DB peak **36 MB**, post-warmup peak **59 MB**, 550/550 hits in ~1.1s. Comfortably under the 500 MB ceiling.

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
- [x] **Memory gate verification**: `mem_check` runs `ui::reader_view::extract` 10× sequentially against a synthesized ~100 KB article HTML laden with chrome / sidebar / ads so the readability scoring path actually fires. Current release-build delta: **5 MB** over the post-warmup peak (59 → 64 MB), with all 10 extractions completing in ~25 ms total. Subprocess isolation deferred — in-process is comfortably under the 500 MB ceiling, so no need to pay the IPC overhead.

## Phase 11: Enclosures & Media
- [x] Parse `<enclosure>` and `<media:content>` / `<media:thumbnail>` in RSS; `<link rel="enclosure">` in Atom; `attachments[]` in JSON Feed. All flow into `Article.attachments` (persisted as JSON column on the `articles` table). *(`enclosure_from_attrs`, `media_attachment_from_attrs`, `parse_jf_attachments`, `AtomLinkRel::Enclosure`)*
- [x] UI indicator on the timeline row: audio/video/image icon based on the first attachment's MIME type, with a count badge when there's more than one. *(`media_icon` + `media_count` in `setup_timeline_list_view`)*
- [x] `Ctrl+Enter` hotkey opens the first attachment via `gio::AppInfo::launch_default_for_uri` — the system's MIME handler decides the player. Users with `mpv` configured as their audio/video default get mpv (and yt-dlp via mpv's url-handler); we don't hard-code a player. *(`act_open_enclosure`)*
- [x] No in-app playback — system media players do the work.

### Parser fidelity follow-ups (bundled here because they all require extending `ParsedFeed`/`ParsedItem`)
- [x] Capture RSS channel `<image><url>` as the feed-level icon URL; refresher persists into `FeedSettings.icon_url`. *(in_channel_image state in `parse_rss`, `fetcher.rs::refresh_one_feed` persists `parsed.icon_url`)*
- [x] Capture Atom `<icon>` and `<logo>` (icon wins over logo per NNW). *(in `parse_atom`, prefer `icon_url.or(logo_url)`)*
- [x] Capture RSS `<language>` and Atom `<feed xml:lang>` into `ParsedFeed.language`. Stored on the parsed feed; reading-pane direction-tagging deferred (no v1.0 user need yet).
- [x] Atom `type="xhtml"` `<content>` and `<summary>` raw inner HTML capture. *(option (b) from the original list — `capture_atom_xhtml_inner` in `parser/xml.rs` re-serializes the inner XML via `quick_xml::Writer`, scoping `trim_text(false)` around the capture so inline whitespace is preserved. Includes the spec-required `<div xmlns="http://www.w3.org/1999/xhtml">` wrapper, which renders fine through ammonia. NNW's `captureRawInnerContent` parity.)*

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
- [x] Desktop notifications for new article counts per refresh cycle. *(`gio::Notification` via `Application::send_notification`, gated by the `notifications-on-refresh` GSetting. v0.8.0 shipped a single global toggle; **v2.4.0 added the per-feed `new_article_notifications_enabled` flag** + Feed Settings dialog — see the `Shipped since v1.6.0` v2.4.0 entry below.)*
- [x] `GSettings` schema for user prefs. *(`data/org.virinvictus.Viaduct.gschema.xml` declares `color-scheme`, `notifications-on-refresh`, `refresh-on-startup`, `refresh-interval-minutes`, `run-in-background`, `retention-days`, `font-monospace`, `font-serif`, `article-theme`, `video-playback-mode`. v0.8.0 wired the first two; v1.8.0 wired `refresh-on-startup` + `refresh-interval-minutes`; Phase 17 wired `run-in-background`. Font overrides reserved for a future phase.)*
- [x] **Sync feeds when viaduct opens** + **Sync feeds periodically** preferences *(shipped v1.8.0)*. New `refresh-on-startup` GSetting fires one refresh ~1500 ms after window shown; `refresh-interval-minutes` (range 0..=1440, 0 = disabled) drives a `glib::timeout_add_seconds_local` that re-arms when the user changes the dropdown. Both surfaced in Preferences → Sync.
- [x] **Refresh while window is closed** *(landed in main; ships under Phase 17's background-daemon checklist)*. Window-hide-on-close + `xdg-desktop-portal` Background API + D-Bus activation for re-summoning, all wired through the new `run-in-background` GSetting in Preferences → Sync. NNW's mac equivalent (`NSBackgroundActivityScheduler`) has no Linux analog without a portal client; we route through `ashpd` and `viaduct-core/src/network/background.rs`.

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
- [x] **Workspace Refactoring** *(shipped v1.5.0)*: Cargo workspace with two members. `viaduct-core/` (headless: database, network, parser, models, error, paths, runtime helpers) and `viaduct/` (binary: main, ui, preferences, fonts, mem_check). `viaduct/src/lib.rs` re-exports `viaduct_core` symbols at the crate root so every existing `crate::xxx` UI import keeps working. CI updated to `cargo {clippy,test} --workspace`. The boundary is now a compile error, not a code-review rule.
- [x] **Video Thumbnail Extraction** *(shipped v1.3.0)*: `src/network/video_thumbs.rs` detects YouTube + Vimeo URLs in `external_url` / `url` / body HTML; YouTube resolves to a deterministic `i.ytimg.com/vi/<id>/hqdefault.jpg`, Vimeo via the public oEmbed endpoint. Bytes flow through `ImageCache::video_thumbnail` (new `Kind::VideoThumb`, dedicated cache dir at `$XDG_CACHE_HOME/viaduct/video-thumbs/`). Timeline row gained a leading 80×45 `gtk::Picture` column shown only when `detect_video` matches.
- [x] **In-pane video playback** *(shipped v1.4.0)*: YouTube + Vimeo. New `play_video_btn` in the article-pane header bar (hidden until `detect_video` matches the current article). Three modes via the new `video-playback-mode` GSetting + Preferences picker: `in-pane` (transient `AdwDialog` housing a fresh `WebKitWebView`, JS on, all storage off, embed origin scoped to `youtube-nocookie.com` / `player.vimeo.com`, dialog close → `try_close` → WebProcess shuts down so audio stops cleanly), `external` (hand the canonical watch URL to `gio::AppInfo::launch_default_for_uri` — works with mpv / yt-dlp / browser handlers), `disabled` (button hidden). Article-pane WebView's lockdown profile is unchanged in every mode. PeerTube intentionally skipped — federated, no canonical embed origin without per-instance config.
- [x] **Empty-state flash on sidebar selection** *(v1.2.1)*: `populate_timeline` and `populate_timeline_with_snippets` switched to `gio::ListStore::splice(0, n_existing, &nodes)` so observers see one atomic `items_changed` instead of an empty-then-populated pair. Search-clear branch in `wire_search` also moved to splice.
- [x] Build out integration test suites for the refresh pipeline (`LocalAccountRefresher` and Inoreader sync), mocking the network layer.
- [x] Implement UI test harnesses to ensure sidebar/timeline/article pane state transitions are rock solid.
- [x] Port any remaining applicable unit tests from `.netnewswire/Tests/` and module test directories.
- [x] **DB worker supervision**: `database::worker::spawn_db_worker` spawns a plain `std::thread::spawn` — if the worker panics every future op is orphaned. Add a supervisor loop that restarts the worker (with a small backoff) and logs the panic.

## v1.2.0: UI Polish *(shipped)*
- [x] **Theme-driven app-wide accent**: every selected article theme propagates its accent color (warm cinnamon for Sepia, deep blue for Biblioteca, warm tan for Tiqoe Dark, etc.) across the GTK chrome — sidebar selection, focus rings, switches, suggested-action buttons, text selection, link buttons. CSS provider at `STYLE_PROVIDER_PRIORITY_USER + 100` overrides libadwaita's accent integration on three layers (`@define-color`, `:root` custom properties, selector-targeted overrides for high-traffic widgets). Beats GNOME 47+'s system accent.
- [x] **Theme picker**: new `article-theme` GSetting + `AdwComboRow` in the prefs dialog. Auto / Adwaita / 8 NNW themes. Live switch without restart.
- [x] **Adwaita theme**: ninth theme with libadwaita-native typography (Cantarell + system-ui), `prefers-color-scheme` baked in for auto dark/light. `accent_hex: None` so GNOME's system accent surfaces unchanged.
- [x] **Hand-tuned dark variants**: each of the 7 light NNW themes gets a `dark.css` overlay activated via `@media (prefers-color-scheme: dark)`. Sepia → roasted-coffee, Biblioteca → leather-bound deep blue, NewsFax → ink-black newsprint, etc. NNW byte-perfect stylesheets stay UNCHANGED.
- [x] **Bundled fonts**: `viaduct-font://` URI scheme + Atkinson Hyperlegible Next bundle so the Hyperlegible theme renders correctly even when the system doesn't ship the font.
- [x] **Empty states**: `AdwStatusPage` for both panes — "No articles" when the timeline is empty, "No article selected" when nothing's loaded. 150 ms crossfade, auto-flips via `connect_items_changed`.
- [x] **Sidebar polish**: 24 px avatars, pill-shaped unread badges, "Smart Feeds" section heading, refined spacing.
- [x] **Timeline polish**: relative date column (`Just now` / `5h ago` / `Yesterday` / weekday / `Mar 19` / `Mar 19, 2025`), HTML-stripped previews with entity decoding, sharper read/unread visual hierarchy. Row layout restructured so date column stays visible regardless of title length.
- [x] **Refresh-in-progress spinner**: refresh button child swaps to `GtkSpinner` during fetch, swaps back when the cycle ends.
- [x] **Adaptive layout**: two `AdwBreakpoint`s (`max-width: 900sp` / `600sp`) collapse the inner / both split views for narrow windows. App reflows to navigation-stack mode on laptops / phone form factors.
- [x] **Article pane scrolling restored**: WebKit-side CSS override re-enables `html, body { overflow: auto }` since v1.1.0-pre1.6 dropped the parent `GtkScrolledWindow`. Styled WebKit scrollbar (8 px, gray thumb).
- [x] **Singleton `gio::Settings`**: `crate::preferences::settings()` returns a process-singleton via thread_local OnceCell so `connect_changed` handlers stay alive past their callsite's stack frame. (Without this v1.2.0-pre1 shipped a non-functional theme picker.)
- [x] **Empty-state flash on sidebar selection** *(shipped v1.2.1)*: `populate_timeline` / `populate_timeline_with_snippets` / search-clear all moved to `gio::ListStore::splice(0, n_existing, &nodes)` so the empty-state stack page can't flash between rebuild steps.

## Phase 17: Flatpak Sandboxing & 1.0 Release
- [x] Flatpak manifest: `network` permission only; no `--filesystem=home`. OPML I/O entirely via `org.freedesktop.portal.FileChooser`.
- [x] AppStream metadata (`appdata.xml`), icons at all required sizes, desktop entry.
- [x] Reproducible build verified against the target Flathub runtime.
- [x] **Background daemon via `xdg-desktop-portal` Background API** — full architecture in `docs/background-service-plan.md`. The `ashpd` request helper at `viaduct-core/src/network/background.rs` is now wired through the Preferences switch. Sub-items:
  - [x] **Window-hide-on-close** in `ViaductWindow`: `connect_close_request` calls `hide_for_background` and returns `glib::Propagation::Stop` when the `run-in-background` GSetting is enabled; otherwise the original popover-cleanup + `Proceed` path runs.
  - [x] **`run-in-background` GSetting** (boolean, default false) added to `data/org.virinvictus.Viaduct.gschema.xml`. Switch row in Preferences → Sync (`run_in_background_row` in `ui/preferences_dialog.rs`).
  - [x] **Portal request wiring**: `connect_changed("run-in-background")` listener fires `viaduct_core::network::background::request_background_permission` via `crate::spawn_on_runtime` whenever the value flips to true. Result is delivered back via `tokio::sync::oneshot` + `glib::spawn_future_local`; on denial we set the GSetting back to false (the bind syncs the switch off) and `show_toast_public` explains why.
  - [x] **D-Bus activation for re-summoning**: `main.rs build_ui` checks `app.windows()` and `present`s an existing `ViaductWindow` instead of building a second one. After re-presenting it calls `reload_current_timeline` so the user lands on the still-selected sidebar item with any articles fetched while hidden.
  - [x] **Flatpak manifest permission**: `--talk-name=org.freedesktop.portal.Background` added to `org.virinvictus.Viaduct.json` `finish-args` alongside the existing `org.freedesktop.secrets`. Notifications portal accessible by default (no explicit `--talk-name` needed; v0.8.0 notifications already work).
  - [x] **Idle memory reduction on hide**: `hide_for_background` calls `ImageCache::clear_memory` (drops all three in-memory LRUs, disk retained), loads `about:blank` in the article-pane `WebKitWebView` to idle the WebProcess, resets `article_display` / `current_video`, hides the play-video button, `remove_all`s the timeline `gio::ListStore`, and flips both stacks to their empty pages before `set_visible(false)`.
  - [x] **`mem_check` background-cycle checkpoint**: fourth checkpoint added that calls `ImageCache::clear_memory_now()` after the warmup + reader-view phases and reports the RSS delta. The full GUI-side hide cycle (WebView idle, ListStore compact) needs interactive QA — those widgets can't be constructed from a headless bin. Headless run shows the LRU clear releases ~4 MB on the synthetic 500-favicon + 50-image corpus.
  - ~~System tray indicator deferred~~ — **shipped in v2.5.0** (`viaduct/src/tray.rs`). Demand arrived: closing the window with run-in-background on used to make the app vanish invisibly. Now a `ksni` StatusNotifierItem appears whenever the GSetting is on, with "Show / Quit" menu items. Works on KDE / XFCE / Cinnamon / MATE natively; on GNOME via the AppIndicator extension.
- [ ] Tag `1.0.0` and submit to Flathub *(blocked on the user — needs Flathub onboarding credentials, not code. v1.6.0 is the existing stable tag the submission should target.)*

## Shipped since v1.6.0 (the stable mark)

These don't belong inside any earlier phase — they're discoverability / polish work that surfaced as the app got real use.

- [x] **v1.7.0 — Add Feed dialog**. Ctrl+N / "Add Feed…" in the primary menu. URL field accepts feed *or* website (port of NNW `FeedFinder` in `viaduct-core/src/network/feed_discovery.rs` does the two-pass discovery). Optional name override, optional folder picker. `Account::add_feed` + `Account::remove_feed` symmetric helpers.
- [x] **v1.7.1 — Right-click context menus**. Sidebar feed rows: Mark All as Read / Refresh / Copy Feed URL / Delete Feed (destructive-styled `AdwAlertDialog` confirmation). Sidebar folder rows: Mark All as Read. Timeline rows: Toggle Read / Toggle Star / Open in Browser / Open Enclosure / Copy URL. Implementation uses `widget.pick(x, y)` + parent-walk for `viaduct-article` / `viaduct-sidebar-item` data attached during `connect_bind`.
- [x] **v1.8.0 — Sync-on-open + periodic refresh**. New `refresh-on-startup` boolean GSetting + Preferences switch (auto-fires a refresh ~1500 ms after window shown). `refresh-interval-minutes` GSetting wired (was declared back in v0.8.0 but never connected to anything). Schema lower bound dropped from 10 to 0 with 0 = disabled sentinel. Combo row in Preferences with discrete options Never / 15 min / 30 min / 1 hour / 2 hours / 6 hours / Daily; re-arms on dropdown change.
- [x] **v1.9.0 — Perf instrumentation**. New `viaduct::perf` log target with structured timing on every sidebar click → timeline navigation cycle (`item`, `articles`, `fetch_ms`, `populate_ms`, `status_ms`, `total_ms`); promotes from INFO to WARN at `total_ms ≥ 500`. `selection_fetch_generation` cancel-stale-fetch counter. New `Account::fetch_articles_by_feeds` bulk DB op (one IN-clause query, replaces the sequential N-round-trip fan-out in `fetch_folder_articles`). `docs/debugging.md` walks through the perf-log workflow end-to-end.
- [x] **v1.9.1 — Hot-path caps for long articles**. `strip_html_for_preview` early-exits at 400 output chars (Pango shapes the entire input even when the label only displays 2 lines; for 60 KB stripped articles that was hundreds of milliseconds per row bind). `scan_html_for_video` truncates input to first 8 KB at a UTF-8 boundary (most video-bearing articles reference the embed in the lead paragraph; we bail rather than scan all 100 KB). Plus the `connect_close_request` cleanup that unparents v1.7.1 popovers before the listview finalizes, fixing the `Finalizing GtkListView, but it still has children left` warnings on exit.
- [x] **v1.10.0 — Refresh while the window is closed (Phase 17 background daemon)**. New `run-in-background` GSetting + Preferences switch wires `xdg-desktop-portal` Background API (via the existing `ashpd` helper at `viaduct-core/src/network/background.rs`) so closing the main window hides it instead of quitting; periodic refresh keeps firing. D-Bus activation re-summons the same window when the user clicks the dock icon, and `reload_current_timeline` repopulates from the still-selected sidebar item with whatever articles arrived while hidden. `hide_for_background` sheds memory by clearing the `ImageCache` LRU (disk cache retained), idling the article-pane WebView (`load_uri("about:blank")`), and `remove_all`-ing the timeline `gio::ListStore`. Flatpak manifest gets `--talk-name=org.freedesktop.portal.Background`; `mem_check` gains a fourth post-clear checkpoint. The full GUI hide cycle is interactive-QA only.
- [x] **v2.0.0 — Phase 18 architectural refinement**. Three-pane decomposition (`ArticlePaneView` / `TimelineView` / `SidebarView`) shrank `window.rs` from 2900 to 2043 lines (−30 %); `ArticleRenderer` GObject promotion with per-renderer `WebContext`; derived unread-count aggregation in `TreeNode` (parents auto-sum from leaves via the `notify::unread-count` cascade); WebKit ↔ GTK CSS bridge polish (`currentColor`-driven scrollbars, 150 ms article-to-article fade-in, `--accent-color` propagation that picks up the libadwaita system accent for the Adwaita theme). See `patchnotes.md` v2.0.0-pre1 through v2.0.0 for the six-pre-release arc.
- [x] **v2.1.0 — Sidebar editing**. Closes the most user-visible NetNewsWire-parity gap left after v2.0: in-app rename / new-folder / move-to-folder. Three new `Account` helpers (`rename_feed` / `create_folder` / `move_feed_to_folder`) follow the existing `add_feed`/`remove_feed` `load_opml → mutate → save_opml` pattern. UI: "Rename Feed…" + "Move to Folder…" entries on the sidebar feed right-click menu (with `AdwAlertDialog` + `GtkEntry` / `GtkDropDown`); "New Folder…" added to the primary menu. Pre-v2.1.0 these required hand-editing OPML.
- [x] **v2.2.0 — Print Article (Ctrl+P)**. New `ArticleRenderer::print(parent)` wraps `webkit6::PrintOperation::run_dialog`; threaded through `ArticlePaneView::print` and `ViaductWindow::act_print_article`. New `win.print-article` action with Ctrl+P accelerator + "Print Article…" entry in the primary menu. Output is whatever the WebKit pane is currently rendering — theme + macros + CSP intact.
- [x] **v2.3.0 — Article Appearance popover**. Closes the "Article Settings Popover" item that has been a *Future UX addition* in §3 of this roadmap since v1.0. New `font-x-generic-symbolic` button in the article-pane header bar opens a `gtk::Popover` with `AdwSpinRow`s for **Text Size** (75–200 %) and **Line Spacing** (centi-units, 100–250 = 1.0–2.5). Two new GSettings keys (`article-font-scale`, `article-line-height`) bound bidirectionally to the spin rows; CSS plumbed through the same `:root` injection that carries `--accent-color`. Live re-render on slider drag via `notify::article-font-scale` / `notify::article-line-height` listeners.
- [x] **v2.6.6 — Tray icon renders without an installed icon theme** *(shipped)*. User report: "system tray icon doesn't work." `tray.rs` only implemented `icon_name` returning `"org.virinvictus.Viaduct"`, which SNI hosts resolve against the icon theme — fine for `meson install` / Flatpak builds (the SVG lands in `$datadir/icons/hicolor/scalable/apps/`), broken for `cargo run` dev builds where nothing installs the icon. Now `Tray::icon_pixmap` returns a `Vec<ksni::Icon>` decoded once at process startup from `docs/icon-{256,512}.png` (embedded via `include_bytes!`, ~42 KB compressed). Decode uses `gtk::gdk_pixbuf::PixbufLoader` (already a transitive of gtk4 — no new dep) on the GTK main thread; the resulting `Icon` (plain `Vec<u8>` + `i32`s) crosses to the ksni tokio worker. RGBA → ARGB32 network byte order via `pixel.rotate_right(1)` per the SNI spec. `icon_name` stays in place for hosts that prefer themable lookup (installed builds remain theme-aware); the pixmap is the universal fallback. Cached behind `OnceLock<Vec<Icon>>` so the second start_service call after a GSetting flip-off-flip-on doesn't re-decode.
- [x] **v2.6.5 — Ghost-data sweep + missing-file failsafes** *(shipped)*. Spring-cleaning audit. **Disk-cache sweep**: pre-v2.6.5 the three `~/.cache/viaduct/{favicons,images,video-thumbs}/` subdirs grew unboundedly — files for unsubscribed feeds and pruned articles stayed forever. New `viaduct-core/src/network/cache_sweep.rs` provides `sweep_targeted` (favicons: md5 set from `feed_settings.favicon_url` ∪ `icon_url`, exact, no false positives), `sweep_by_age` (images: `retention_days × 2`; video-thumbs: `retention_days`; mtime-gated since `noatime`/`relatime` mounts make atime unreliable), and `wipe_dir` (unconditional, used by the new Debug menu action). Wired into `cleanup_at_startup` as a fifth step after the existing article/status/author sweeps; I/O hops to `spawn_blocking` so the worker thread never stalls. **sync.sqlite ghost-row sweep**: when the active delegate is `LocalAccountDelegate` (no Inoreader credentials), every row in `syncStatus` is leftover from a previous remote session. New `SyncDbOp::WipeAll` + `AccountDelegate::is_local()` (default false; local impl overrides to true) drops the lot at startup. **Toast on OPML load failure**: pre-v2.6.5 a malformed `local.opml` produced a silent empty sidebar; now surfaces `adw::Toast` "Couldn't load local.opml — see log for details." **Debug "Wipe Disk Caches" action** (new `win.debug-clear-caches`, registered only when `--debug` is on): drops in-memory LRU + `wipe_dir`s all three cache subdirs, toasts the file count. **Missing-file audit**: confirmed `paths::ensure_dirs()` recreates missing data/cache dirs, `Connection::open` + `setup_schema` recreate missing SQLite files, `load_opml` returns empty `OpmlFile` when `local.opml` is missing, and `delete_settings_for_feeds_not_in([])` / `delete_articles_not_in_feeds([])` regression-tested early-returns mean an empty OPML doesn't nuke DBs. 23 viaduct + 90 viaduct-core (+8 cache_sweep unit tests) + 1 integration = 114 tests pass.
- [x] **v2.6.4 — Favicon discovery + YouTube placeholder filter** *(shipped)*. Two cosmetic but persistent rough edges, fixed in one release. **Favicon discovery**: pre-v2.6.4 the sidebar only consulted `settings.favicon_url.or(settings.icon_url)`, both populated only from feed-level XML metadata (`<image><url>`, Atom `<icon>`/`<logo>`). Most personal blogs (Sacha Chua, Karthinks, public voit, Howardism, vv's blog, Protesilaos Stavrou, …) ship neither, so the sidebar fell back to AdwAvatar initials forever. New `viaduct-core/src/network/favicon_discovery.rs` ports NNW's `SingleFaviconDownloader` flow: GET the home page (256 KB cap), scan the head with `parser::extract_metadata` for `<link rel="icon">` / `<link rel="shortcut icon">` / `<link rel="apple-touch-icon">` (plain icon wins; apple-touch-icon as fallback), HEAD-verify the candidate (with GET fallback for Cloudflare-fronted hosts that 405 HEAD), and fall back to `<origin>/favicon.ico`. Successful discoveries persist into `FeedSettings.favicon_url`, so the probe runs at most once per feed across the install lifetime. `fetcher.rs::refresh_one_feed` also now persists `parsed.home_page_url` into `new_settings.home_page_url` (was being dropped on the floor — favicon discovery needs it as a probe base). `Fetcher` gained a `pub fn client(&self) -> Client` accessor so adjacent network work reuses the connection pool. **YouTube thumbnail placeholder filter**: `spawn_video_thumbnail_fetch` in `timeline.rs` now drops decoded textures whose `width() < 200`. Real `hqdefault.jpg` is 480×360; YouTube's "no thumbnail" placeholder is exactly 120×90 and gets returned for invalid / removed video IDs (or ones we mis-extracted from article body text — Michael Tsai's link-roundup posts, HN summaries with embedded YT URLs, news clips). Picture stays invisible, the column collapses, the row reflows tightly. 23 viaduct + 82 viaduct-core (was 73 — +9 favicon-discovery unit tests) + 1 integration = 106 tests pass.
- [x] **v2.6.3 — Background-mode memory diagnostics** *(shipped)*. User reported `~450 MB` peak after a session with run-in-background on; v1.1.0 baseline with foreground-only was ~280 MB. Don't yet know whether this is a real leak or just `VmHWM`-monotonic-aggregation across many refresh peaks over a long session, so v2.6.3 ships the instrumentation rather than guessing. Five `tracing::info` checkpoints, all gated on default `viaduct=info` so they surface in normal output:
  - **`diag: hide_for_background post-clear`** — verifies v1.10.0's "≤ 100 MB hidden" target still holds.
  - **`diag: reload_current_timeline re-show`** — fires from the new `ViaductWindow::unhide_from_background` method, called by `main.rs build_ui` when GApplication re-activates the existing window. Delta from the last hide line shows re-summon cost.
  - **`diag: refresh cycle pre` / `diag: refresh cycle post`** — wraps `run_refresh_with_tally` (manual `Ctrl+R`, post-import refresh, and the v1.8.0 periodic-refresh `glib::timeout` all funnel through it). Post-line carries `peak_delta_mb` directly. Climbing peak across many cycles = real leak; flat peak = `VmHWM` is just sticky.
  - **`diag: background tick`** — `glib::timeout_add_seconds_local(300)` armed by `hide_for_background`, cancelled by `unhide_from_background`. Logs VmRSS every 5 min while hidden. New `imp.hidden_state_ticker: RefCell<Option<glib::SourceId>>` on `ViaductWindow` holds the source so re-show can cancel cleanly.
  - **`diag: tray Show` / `diag: tray Quit`** — `viaduct/src/tray.rs` receive loop logs on each menu activation.
  Implementation: `viaduct_core::read_memory_mb` promoted from private to `pub` (re-exported at the `viaduct` crate root); the helper was already there for the existing `--debug` ticker, just not callable from outside the crate. After ship, run overnight in background mode → share log lines → diagnose. Suspects to investigate if a leak is confirmed: ksni's tokio worker + zbus retention, `LocalAccountRefresher` per-cycle `Arc<Account>` retention, `WebContext` scheme-handler closure retention, any new `glib::Object` cycles introduced in v2.x.
- [x] **v2.6.2 — Mark All Read leaves badge stuck at 1**. Same count-vs-fetch-disagreement pattern as v2.6.1, in a sibling spot. `smart_feed_counts.all_unread` and `.starred_unread` ran bare `COUNT(*) FROM statuses` while `fetch_unread` / `fetch_starred` (and the per-feed badge query) `INNER JOIN articles`. Orphan status rows (preserved when an article is deleted but its status is kept so a re-fetch restores the user's read/starred state — `articles_ad_lookup` cascade-skip behaviour) were counted by the badge but invisible to the click result. Mark All as Read marked the visible ones, the orphan stayed at `read=0`, badge stuck at 1. Both queries now `INNER JOIN articles`. New `smart_feed_counts_excludes_orphan_statuses` regression test.
- [x] **v2.6.1 — Today smart feed timezone fix**. `smart_feed_counts.today_unread` (badge) used `.and_utc()` on a local naive datetime — interpreting local midnight as UTC midnight — while `fetch_today` (click result) correctly converted local→UTC. The badge counted a window starting 4 h (EDT) / 5 h (EST) earlier than the click result, so on any non-UTC system the count and the list disagreed. Both paths now go through one `local_midnight_utc_seconds()` helper with a unit test (`local_midnight_helper_matches_explicit_local_midnight`) locking the semantic. `fetch_today` emits a `tracing::debug` line on every call (`RUST_LOG=viaduct=debug`) showing the resolved boundary + result count for diagnosis.
- [x] **v2.6.0 — Drag-and-drop sidebar reorder**. Closes the gap left after v2.1.0's right-click "Move to Folder…" action. `gtk::DragSource` (only enabled on `Feed` rows; provides `feed.url` as `String` content) + `gtk::DropTarget` (only enabled on `Folder` rows; calls `Account::move_feed_to_folder`) added to every sidebar row in `setup_sidebar_list_view`'s `connect_setup`. Both controllers read the bound `SidebarItem` via the existing `viaduct-sidebar-item` `set_data` pattern from v1.7.1. After a successful drop, the handler activates `win.reload-sidebar` (a new pure-delegation action on `ViaductWindow` pointing at `reload_sidebar_after_opml_change`) to repopulate the tree. Tokio→GTK thread bridge via `tokio::sync::oneshot`. The right-click "Move to Folder…" dialog still covers feed → standalone (no root drop target).
- [x] **v2.5.0 — System tray indicator**. Closes the deferral noted in v1.10.0's Phase 17 background-daemon checklist (the tray was deferred until explicit user demand). New `viaduct/src/tray.rs` registers a `ksni`-backed StatusNotifierItem when the `run-in-background` GSetting is on. Left-click → `Application::activate()` (same path as the dock icon). Right-click menu → "Show viaduct" / "Quit viaduct"; the quit path goes through `gio::Application::quit()` so it bypasses `connect_close_request`'s hide-instead-of-quit branch. Service lifecycle bound to the GSetting via `connect_changed`. SNI works natively on KDE / XFCE / Cinnamon / MATE; on GNOME via the AppIndicator extension. New deps: `ksni 0.3` + its `pastey` macro helper; reuses the `zbus` already in our tree from Phase 17's `ashpd`.
- [x] **v2.4.0 — Per-feed notifications**. Closes the deferral noted in v0.8.0's Phase 13 entry (the desktop-notifications shipped global-only because per-feed needed a feed-inspector pane that didn't exist yet). New "Feed Settings…" entry on the sidebar feed right-click menu opens an `AdwAlertDialog` with two `AdwSwitchRow`s: **New article notifications** (the new field) and **Always use Reader View** (existing `reader_view_always_enabled`, exposed in UI for the first time). Schema picks up `feed_settings.new_article_notifications_enabled INTEGER NOT NULL DEFAULT 0` (idempotent ALTER TABLE for upgrades). `RefreshTally` switched from `new_articles: usize` to `per_feed_new: HashMap<String, usize>`; `dispatch_refresh_notification` walks the map and fires a separate `gio::Notification` per feed when both gates (global `notifications-on-refresh` and per-feed flag) are on. Per-notification `id` of `viaduct.refresh.<feed_id>` so the desktop daemon coalesces repeated refreshes. **Closes the last user-visible NetNewsWire-parity gap.**

---

## Success Criteria (v1.0)

1. Import a 500-feed OPML file without hanging the GTK main thread.
2. Background engine fetches + parses 1,000 new articles while the user smoothly scrolls the list view.
3. Idle memory sits between 100–300 MB; peak stays under 500 MB through every supported operation.
4. FTS5 search across all cached articles returns results in under 50 ms on a 50k-article corpus.
5. Full compliance with GNOME HIG and libadwaita 1.7 styling.
6. Flathub-accepted Flatpak build running in a strict sandbox.

---

## Phase 18: v2.0 Architectural Refinement *(post-1.0)*

Comparative review against `.newsflash` (the only other Linux RSS reader worth using) surfaced four candidate architectural changes. Two adopted, one deferred indefinitely, one rejected outright. Full evaluation in `two-plans.md`. The goal is to reduce `ViaductWindow`'s monolith and tighten the `WebKitWebView` lifecycle without breaking the **"Port. Don't invent."** rule that anchors `CLAUDE.md` §4. Sequenced for landing **after** the 1.0 Flathub tag — not before. None of these block 1.0; they're polish for the 2.0 banner.

### Adopted

- **Decompose `ViaductWindow` into three widget subclasses**: `SidebarView`, `TimelineView`, `ArticlePaneView`. NNW already does this — `SidebarViewController`, `TimelineViewController`, `DetailViewController` — so the refactor is *more* port-faithful, not less. Each subclass owns its widgets, its signal handlers, and its `connect_*` lifetimes; `ViaductWindow` shrinks to cross-pane plumbing (toast overlay, header bars, action group, `Arc<LocalAccount>`, `Arc<ImageCache>`, `feed_names: OnceCell<FeedNameMap>`). The `act_*` methods migrate to the pane that owns the widgets they touch (e.g. `act_smart_read` → `TimelineView`, `act_toggle_reader` → `ArticlePaneView`). Action installation in `actions.rs` stays at the window level; bodies dispatch into the relevant child via signal or downcast.
  - [x] **`ArticlePaneView`** *(shipped v2.0.0-pre1)*. Owns the locked-down `WebKitWebView`, reader-view + play-video buttons, `viaduct-img://` / `viaduct-font://` scheme handlers, link interceptor, hover URL overlay, `ArticleDisplayState`, `current_video`, the in-pane embed dialog, and the macro substitution. Public surface: `set_article(ArticleRenderContext)` / `set_auto_reader` / `clear` / `idle_for_background` / `refresh_render` / `toggle_reader` / `play_video` / `current_article_url`. `window.rs` shrank from 2900 to 2358 lines.
  - [x] **`TimelineView`** *(shipped v2.0.0-pre2)*. Owns the timeline list view + `gio::ListStore` + `gtk::SingleSelection` + search bar + scope toggle + empty-state stack + FTS5 debounce + `selected_feed_id`. Public surface: `list_view` / `store` / `selection` / `selected_feed_id` / `set_selected_feed_id` / `current_article_node` / `populate` / `populate_with_snippets` / `clear` / `refresh_statuses` / `focus_search_entry`. The window-side `wire_search` method, `populate_timeline` / `populate_timeline_with_snippets` / `refresh_timeline_statuses` / `escape_fts5` file-scope helpers all moved into the new module. `window.rs` shrank from 2358 to 2306 lines.
  - [x] **`SidebarView`** *(shipped v2.0.0-pre3)*. Owns the sidebar list view, the OPML-derived tree (`SidebarTreeControllerDelegate` / `TreeController` / `SidebarDataSource`), the `feed_names` resolver, the right-click feed + folder popovers and gesture controller, the `right_clicked_feed/folder` cells, and the sidebar header bar (`mark_all_read_btn`, `sync_btn` + stack/spinner, `search_btn`, `menu_btn`, `primary_menu`). Public surface: `list_view` / `selection` / `search_btn` / `mark_all_read_btn` / `sync_btn` / `primary_menu` / `feed_names` / `controller` / `delegate` / `data_source` / `list_folder_names` / `take_right_clicked_feed/folder` / `apply_opml(OpmlFile)` / `refresh_unread_counts(account)` / `set_refresh_in_progress(on)` / `unparent_popovers`. The 80-line unread-count tree walk and the `display_name_for_feed` / `pick_sidebar_item_at` helpers moved out of `window.rs`. **`window.rs` shrank from 2306 to 2043 lines** — the cumulative Phase 18 decomposition delivered −857 lines (−30 %) versus the pre-pre1 baseline of 2900.
- [x] **Promote `article_renderer.rs` into a dedicated `ArticleRenderer` GObject** *(shipped v2.0.0-pre4)*. New `ViaductArticleRenderer` (in `viaduct/src/ui/article_renderer_widget.rs` + `article_renderer_widget.ui`) owns the `WebKitWebView`, its **per-renderer `WebContext`** (the architectural improvement — `viaduct-img://` + `viaduct-font://` schemes registered there, no longer leaking onto `WebContext::default()` and the video-dialog WebView), the link interceptor, the hover URL overlay, the locked-down `WebKitSettings` profile. `install_image_uri_scheme` / `install_font_uri_scheme` were refactored to take a `&WebContext` parameter so the renderer can target its own context. `ArticlePaneView` now talks to one method — `article_renderer.render_themed(theme, subs, base_uri)` — instead of reaching into a WebView template_child. Reader-mode + macro substitution + theme resolution stay on `ArticlePaneView` (orchestration layer); the renderer is purely "given these inputs, render this HTML in a locked-down WebView". Same v1.1.0 lockdown profile, same CSP, same `viaduct-img` routing — pure structural cleanup.
- [x] **Expand `glib::derived_properties` coverage where it eliminates manual refresh sweeps** *(shipped v2.0.0-pre5)*. `TreeNode::set_child_nodes` now subscribes to `notify::unread-count` on each new child and recomputes self = sum(children) on every fire — the cascade naturally propagates up through the tree's own notify-emit chain. `SidebarView::refresh_unread_counts` lost its `folder_total`/`group_total` accumulators and shrank from ~80 to ~50 lines; it walks one level down from each top-level node and only sets leaves. Adding nested folders later (currently disallowed by NNW normalization but possible) just works — the aggregation is now depth-N for free. NNW counterpart: the read-count notification in `ArticlesTable`.
- [x] **WebKit ↔ GTK CSS bridge polish** *(shipped v2.0.0-pre6)*. Three small refinements piped through `VIADUCT_PANE_OVERRIDE_CSS` so the byte-perfect NNW theme stylesheets stay untouched: (a) **scrollbar parity** — 6 px idle / 10 px hover thumb using `color-mix(in srgb, currentColor 30/55%, transparent)` so the thumb adopts the page's text color automatically (no GTK→WebKit color-querying required); (b) **article fade-in** — 150 ms `@keyframes viaduct-fade-in` on `body`, replayed on every `WebKitWebView::load_html`, matching the existing `article_stack` crossfade rhythm; (c) **system accent propagation** — `:root { --accent-color: <hex>; }` injected at the top of the style cascade, computed from `theme.accent_hex.unwrap_or_else(|| system_accent_hex())` where `system_accent_hex()` reads `adw::StyleManager::default().accent_color().to_standalone_rgba(is_dark)` (GNOME 47+). Adwaita theme picks up the system accent; named themes (Sepia, Biblioteca, etc.) keep their hard-coded values. Theme CSSes can opt in via `var(--accent-color)`; bundled NNW themes don't yet consume it but the infrastructure is now there.

### Considered & deferred

- **Blueprint (`.blp`) syntax conversion** — *deferred indefinitely.* Pure syntactic sugar over `.ui` XML; adds `blueprint-compiler` build dep with no runtime benefit. Existing `.ui` files (~250 lines total across `window.ui` + `shortcuts.ui`) are perfectly tractable. Reconsider only if Blueprint becomes the GNOME default and `.ui` is deprecated.

### Rejected

- **Relm4 / Actor-Model Event Bus rewrite** — violates `CLAUDE.md` §4 "Port. Don't invent." NNW uses `NotificationCenter` + KVO, not unidirectional event streams; adopting Relm4 means rewriting every UI handler in a framework whose abstractions don't map back to `.netnewswire/`, breaking the porting reference loop. The "boilerplate" the comparison flagged is largely a *feature* — it keeps each handler 1:1 readable against the corresponding NNW source. Centralized cross-cutting messaging already exists where it makes sense (`glib::MainContext::channel` for worker → GTK, `mpsc` for GTK → DB worker); we don't need a third layer.

