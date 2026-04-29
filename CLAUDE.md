# Viaduct — AI Agent Architecture & Guidelines

This file is the definitive reference for AI agents (Claude, Gemini, etc.) and human contributors working on **viaduct**. Project-scoped rules here **override** anything in `~/CLAUDE.md` for work inside this repo.

Before you touch code, read `README.md`, `spec.md`, `roadmap.md`, and `patchnotes.md`. They carry invariants this file does not repeat.

---

## 1. What viaduct Is

**viaduct is a port of NetNewsWire to GTK4 and Rust. That is the entire project.**

NetNewsWire is open-source, battle-tested, and has already solved every hard problem in this domain — feed parsing, date parsing, conditional GETs, article deduplication, coalesced UI updates, memory discipline. We are translating that solution from Swift/AppKit to Rust/GTK4 so Linux users can have it. **We are not designing a new RSS reader.** We are not re-litigating its architectural choices. We port first, then we make it work on Linux.

If you catch yourself thinking "I have a better idea" — you don't. Go read the corresponding Swift file in `.netnewswire/` and port *that*. The app is local-only, no sync backends, no WebKit, strict memory budget. Targets **GNOME 50+** and **libadwaita 1.7+** on Wayland.

Current version: **v1.0.0** (Stable). The xdg-desktop-portal Background daemon moved from Phase 13 to Phase 17 because it pairs naturally with the Flatpak manifest work. See `roadmap.md` for the live phase plan and `patchnotes.md` for the shipped log.

**License:** MIT. **Edition:** Rust 2024.

---

## 2. Source of Truth: `.netnewswire/`

A full clone of NetNewsWire lives at `.netnewswire/` in this repo. **It is already there — do not re-clone, re-download, or `git submodule add` it.** Treat it as read-only reference material.

> **Note on Latest Upstream Sync (April 28, 2026):**
> The `.netnewswire` reference folder has been updated to the latest upstream state (Commit `4d594181f`, post-7.0.5 release). Key changes since the previous sync at `ec06277`:
> *   **Parser refactor:** `MutableItem` renamed to `RSSItem` and shared between RSS and Atom parsers. A single `uniqueIDCalculator` is now shared across feed types. Atom now picks up `<icon>` favicons natively and parses `<summary>` correctly. Pure-rename refactors don't translate to our Rust code; keep porting from the new file names.
> *   **`domainsWithNoMinimumTime` expanded** from a small list to 19 domains in `LocalAccountRefresher.swift`. Our `is_no_minimum_domain` should sync to match — folded into v1.0.6 maintenance.
> *   **Macro template format:** NNW article templates use `[[key]]` (double-bracket), not `{{key}}`. Match exactly so themes drop in byte-perfect.
> *   **Issue #5280 / WebView cache:** NNW reverted a change that aggressively emptied the WebKit cache between renders because it triggered intermittent bugs. Lesson for our Phase 6: don't flush WebKit caches between articles. Our memory-only `WebKitNetworkSession` data store sidesteps this automatically.
> *   **Author/authorsLookup startup cleanup (Fixes #5232):** NNW added an explicit cleanup sweep on account init. Our `articles_ad_lookup` cascade trigger handles this differently — no port needed.
> *   **Date parser simplification & swiftString UTF-8 conversion:** Swift-specific micro-perf, not portable to our `chrono` + `quick-xml` stack.
> *   **CloudKit Storage Stats UI:** out of scope (we don't ship CloudKit).
> *   **Themes unchanged** since previous sync — Phase 6 plan to bundle all 8 (`Sepia`, `Appanoose`, `Biblioteca`, `Hyperlegible`, `NewsFax`, `Promenade`, `Tiqoe Dark`, `Verdana Revival`) is unaffected.

When you need to implement a feature, port from this local tree. Do not invent bespoke logic.

Key paths to consult:

| viaduct target | NetNewsWire source |
|---|---|
| `src/parser/xml.rs`, `src/parser/json.rs`, `src/parser/html.rs` | `.netnewswire/Modules/RSParser/Sources/RSParser/` (see `Feeds/XML/`, `Feeds/JSON/`, `HTML/`) |
| `src/parser/date.rs` (`DateParser` port) | `.netnewswire/Modules/RSParser/Sources/RSParser/Utilities/DateParser.swift` |
| `src/database/articles.rs` | `.netnewswire/Modules/ArticlesDatabase/Sources/ArticlesDatabase/` and `.netnewswire/Modules/Articles/Sources/Articles/` |
| `src/database/settings.rs`, `src/database/opml.rs`, `src/database/accounts.rs` | `.netnewswire/Modules/Account/Sources/Account/` (`FeedSettingsDatabase.swift`, `OPMLFile.swift`, `OPMLNormalizer.swift`, `LocalAccount/LocalAccountDelegate.swift`) |
| `src/network/fetcher.rs` | `.netnewswire/Modules/RSWeb/Sources/RSWeb/` plus `.netnewswire/Modules/Account/Sources/Account/LocalAccount/LocalAccountRefresher.swift` |
| Coalescing / BatchUpdate primitives | `.netnewswire/Modules/RSCore/Sources/RSCore/` (`CoalescingQueue.swift`, `BatchUpdate.swift`) and `.netnewswire/Modules/RSDatabase/Sources/RSDatabase/` (`DatabaseQueue.swift`) |
| Feed discovery from a website URL | `.netnewswire/Modules/FeedFinder/Sources/FeedFinder/` |

`.gitignore` may or may not exclude `.netnewswire/` depending on the branch; in either case, never commit changes inside it.

---

## 3. Codebase Map

### Top-level

* `Cargo.toml` — deps. Notables: `gtk4` v4.16, `libadwaita` v1.7, `tokio` (full features), `rusqlite` (bundled + FTS5), `reqwest` (rustls-tls), `quick-xml`, `serde_json`, `ammonia`, `crossbeam-channel`, `thiserror`, `chrono`, `md-5`, `tracing`. `anyhow` is present but reserved for binary glue only.
* `build.rs` — runs `glib-compile-schemas data/` so dev runs find the compiled GSettings schema. Failures emit `cargo:warning` rather than aborting, so CI runners without GLib dev tools still produce a binary; the runtime falls back to defaults when `gio::Settings::new` can't find the schema.
* `data/` — non-source assets. Currently holds `org.virinvictus.Viaduct.gschema.xml` (GSettings schema) and the build-produced `gschemas.compiled` (gitignored). Future home for icons / appstream metadata once Phase 17 packaging lands.
* `README.md`, `spec.md`, `roadmap.md`, `patchnotes.md` — authoritative design docs.
* `.github/workflows/ci.yml` — Ubuntu runner: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`. All three must stay green.

### `src/`

The crate is **lib + bin**. `src/lib.rs` is the library root (declares all module trees publicly). `src/main.rs` is the GTK application binary; auxiliary binaries live under `src/bin/`. This split lets profiling harnesses and future CLI tools share the same modules instead of duplicating them.

* `lib.rs` — library root. Just `pub mod` declarations for `database`, `error`, `models`, `network`, `parser`, `paths`, `preferences`, `ui`.
* `main.rs` — GTK entrypoint. Initializes tracing (`EnvFilter`, `RUST_LOG` controls verbosity), calls `ensure_schema_dir` to point `GSETTINGS_SCHEMA_DIR` at `$CARGO_MANIFEST_DIR/data` for dev builds, boots the global tokio runtime, calls `paths::ensure_dirs()`, constructs `LocalAccount`, launches the `adw::Application`. `build_ui` applies the color-scheme preference via `viaduct::preferences::apply_color_scheme` before constructing the window. Uses the library via `use viaduct::…`.
* `preferences.rs` — `gio::Settings` wrapper over `data/org.virinvictus.Viaduct.gschema.xml`. `settings()` returns `Option<gio::Settings>` (None when the schema isn't installed — dev environment without `glib-compile-schemas`). **Process-singleton via `thread_local OnceCell` (v1.2.0)**: every call returns the same GObject so `connect_changed` handlers registered through any callsite stay alive past the caller's stack frame. Without this v1.2.0-pre1 shipped a non-functional theme picker — the dropdown wrote correct values to dconf but every listening Settings instance had been freed. `apply_color_scheme(&settings)` drives `adw::StyleManager::default().set_color_scheme` and connects `notify::color-scheme` for live updates; `notifications_enabled(&settings)` reads `notifications-on-refresh` on demand; `retention_days(&settings)` reads `retention-days` clamped to `[1, 365]` and is consumed by `act_refresh`/`refresh_specific_feeds` on the GTK thread before handoff to tokio (`gio::Settings` is `!Send`). **`apply_article_theme_accent(&settings)` (v1.2.0)** plumbs the user's `article-theme` GSetting through `resolve_article_theme(&settings, is_dark)` and pushes the chosen theme's `accent_hex` into `article_renderer::apply_app_accent`. Connects both the GSetting change notify AND `adw::StyleManager::dark_notify` so flipping system dark mode auto-swaps the accent in `auto` mode. Schema id: `org.virinvictus.Viaduct` (matches `Application::application_id`). Schema path: `/org/virinvictus/Viaduct/`.
* `bin/mem_check.rs` — Phase 7 + Phase 10 memory checkpoint harness. Synthesizes a 500-feed × 10-article corpus in a tempdir XDG, runs `LocalAccount::update_feed` through the real single-writer worker, warms the favicon and image cache against an in-process `tokio::net::TcpListener` HTTP/1.1 fixture (500 favicons × 1 KB + 50 images × 50 KB, path-prefix routed `/fav-*` vs `/img-*`; 500 favicons exceeds the LRU's 250-per-kind cap so the eviction path is exercised), then runs `ui::reader_view::extract` 10× over a synthesized ~100 KB article HTML laden with chrome/sidebar/ads so the readability scoring path actually fires. Reads `VmHWM` from `/proc/self/status` at three checkpoints (post-DB, post-warmup, post-reader-view) and pass/fails against the 500 MB peak budget. `cargo run --release --bin mem_check`. Current release-build numbers: post-DB peak ~36 MB, post-warmup peak ~59 MB, post-reader-view peak ~64 MB. The harness installs the global tokio runtime via `viaduct::init_runtime` so `ImageCache`'s `spawn_on_runtime` calls resolve correctly.
* `paths.rs` — XDG resolution. Data → `$XDG_DATA_HOME/viaduct/` (fallback `~/.local/share/viaduct/`). Cache → `$XDG_CACHE_HOME/viaduct/` (fallback `~/.cache/viaduct/`). Resolves `local.opml`, `articles.sqlite`, `feed-settings.sqlite`, `favicons/`, `images/`.
* `models.rs` — domain types: `Feed`, `Folder`, `Article`, `ArticleStatus`, `Author`, `FeedSettings`, `ParsedItem`, `ParsedFeed`, `ArticleChanges { new, updated, deleted }`.
* `error.rs` — `ViaductError` (top-level) → `DatabaseError`, `NetworkError`, `ParseError`. All via `thiserror`. Each variant preserves its source error (`rusqlite`, `reqwest`, `quick_xml`, `serde_json`, `url::ParseError`).

### `src/database/`

Three stores, strict separation, single writer. Port of NNW's three-way split.

* `articles.rs` — `ArticlesDatabase`. Tables: `articles` (with `attachments JSON` column added by an idempotent ALTER for pre-existing DBs), `statuses`, `authors`, `authorsLookup`, FTS5 virtual `search`. Triggers: FTS index maintenance on insert/update/delete, and `articles_ad_lookup` to cascade-clean `authorsLookup` when an article is removed (status rows are deliberately NOT cascaded — NNW keeps them for reappearing articles). WAL + `synchronous=NORMAL`, `temp_store=MEMORY`, `mmap_size=30GB` (sparse mapping, not actual allocation). Public helpers: `article_id_for(feed_id, unique_id)` — MD5 of `"{feed_id} {unique_id}"` per NNW `Article.calculatedArticleID`; `parsed_to_article` which truncates `DateTime` to second precision so DB round-trips don't flag every article as "updated"; `pub const DEFAULT_RETENTION_DAYS: i64 = 30` for callers without a GSettings handle. Main op is `ArticlesDbOp::UpdateFeed { feed_id, items, delete_older, retention_days, reply }` which produces a real `ArticleChanges` diff (new / updated / deleted + status rows for new articles; stale items >6 months default to `read=1`; orphans non-starred older than `retention_days` get deleted when `delete_older=true`). Phase 14 prune ops: `DeleteArticlesNotInFeeds(feed_ids)` (empty input is a no-op, matches NNW `deleteArticlesNotInSubscribedToFeedIDs`), `DeleteOldStatuses { retention_days }` (NNW `feedBased` branch — `WHERE date_arrived < ? AND starred = 0 AND article_id NOT IN (SELECT article_id FROM articles)`), `Vacuum`. Other ops: `Search`, `SearchWithSnippets` (FTS5 `snippet()` + optional feed-scope filter), `FetchStatusesByIds` (bulk status lookup for keyboard navigation, chunked at 500 IDs to stay under SQLite's parameter limit), `UnreadCountsByFeed` (per-feed unread totals via `LEFT JOIN statuses` so missing-status rows count as unread per NNW), `SmartFeedCounts` (returns a `SmartFeedCounts { today_unread, all_unread, starred_unread }` struct used by the sidebar's three pinned rows).
* `settings.rs` — `FeedSettingsDatabase`. Per-feed cache: `feed_id` (PK, string — note NNW uses `feedURL` as PK, we diverge), `feed_url`, `home_page_url`, `icon_url`, `favicon_url`, `edited_name`, `content_hash`, conditional-GET (`etag`, `last_modified`, `date_created` = when those were received), cache-control (`max_age`), `authors_json`, `folder_relationship_json`, `last_check_date`, `reader_view_always_enabled`. `delete_settings_for_feeds_not_in` early-returns on empty input (regression-tested) — do NOT "simplify" that branch back to a bare DELETE or you'll wipe every row.
* `opml.rs` — OPML on disk is the source of truth for the feed/folder hierarchy. Coalesced save (~500ms debounce), atomic temp-file + rename. `OpmlWriter::spawn` owns its own tokio task; `save(OpmlFile)` queues and awaits the next flush. Phase 12 user-import pipeline lives here too: `normalize_opml` (port of NNW `OPMLNormalizer` — drops nameless wrappers, flattens nested folders one-level-deep, dedups feeds by `xmlUrl`), `merge_opml(existing, incoming) -> (OpmlFile, Vec<Feed>)` (union by `xmlUrl`, never overwrites `edited_name`, returns just the newly-added feeds for refresh), and `serialize_account_opml(title, &OpmlFile)` (hand-rolled NNW-shape writer matching `OPMLExporter.OPMLString` byte-for-byte: tab indent, attribute order, `description=""`, `version="RSS"`, `<!-- OPML generated by viaduct -->` comment). The serde-driven `serialize_opml` continues to back the on-disk debounced save.
* `accounts.rs` — `LocalAccount` orchestrator owning the OPML file + both DBs. Public async API: `load_opml`, `save_opml`, `batch_insert_articles`, `upsert_statuses`, `fetch_articles_by_feed`, `fetch_unread_articles`, `fetch_starred_articles`, `fetch_today_articles`, `search_articles`, `search_articles_with_snippets`, `fetch_statuses_by_ids`, `unread_counts_by_feed` (sidebar badges), `smart_feed_counts` (Today/All Unread/Starred totals), `fetch_feed_settings`, `upsert_feed_settings`, `update_feed(feed_id, items, delete_older, retention_days)` (the diff path; `retention_days` flows through to the per-update prune), `cleanup_orphaned_settings`, `cleanup_at_startup(retention_days)` (Phase 14 chain — settings prune → article prune → status sweep → vacuum on both DBs), `vacuum_databases`, `import_opml(path) -> Vec<Feed>` (parses → normalizes → merges → saves, returns newly-added feeds for refresh), `export_opml(path, title)` (writes the NNW-shaped string). `LocalAccount::new` runs `cleanup_at_startup` instead of just the settings sweep. Private helpers `opml_feed_urls` and `opml_feed_ids` walk standalone+folder feeds for the cleanup methods.
* `worker.rs` — **single writer**: `std::thread::spawn` pulls `DbOp`s off a tokio `mpsc::Receiver` via `blocking_recv` and dispatches to `articles::handle_op` or `settings::handle_op`. The GTK thread holds only a `Sender`; it never blocks on SQLite. *No panic supervisor today — tracked in Phase 16.*

### `src/network/`

* `fetcher.rs` — `Fetcher` (reqwest, rustls-tls, HTTP/2 auto-negotiated, UA `Viaduct/1.0 (Linux; GTK4)`) + `DownloadSession` analog that coalesces duplicate in-flight URLs via `broadcast::channel` and tracks per-host cooldowns after 429. `LocalAccountRefresher::new(account: Arc<LocalAccount>, changes_sender, retention_days: i64)` — `retention_days` is forwarded to `account.update_feed` so the per-feed orphan sweep honors the user's `retention-days` GSetting. The refresher pipeline actually does parse + diff + persist: conditional-GET headers from `FeedSettingsDatabase`, short-circuits on 304 (updates `last_check_date` only), content-hash short-circuits on byte-identical bodies, calls `parser::parse` → `account.update_feed` → emits real `ArticleChanges`, persists ETag/Last-Modified/`content_hash`/`last_check_date`/`max_age` back to settings. Implements NNW's **8-day conditional-GET expiry** (catches servers that always 304; openrss.org + rachelbythebay.com are exempt), **5-hour cap** on `Cache-Control: max-age` (openrss.org exempt), and **25-hour special-case cutoff** for high-frequency feeds. Skip rules: twitter.com/x.com, cache-control freshness window, 29-min minimum between checks. **Domain matching** via `url_host_matches_domain(url, &[&str])` — port of NNW `SpecialCase.urlStringMatchesDomain`. Parses URL → lowercases host → strips optional `www.` → exact-matches against the supplied lowercase domain list. Substring matching (the previous behavior) was a real bug: `evilrachelbythebay.com` would have false-matched. Three module-level constants drive the policy: `SPECIAL_CASE_DOMAINS` (rachelbythebay.com, openrss.org — get the 25-hour cutoff and conditional-GET exemption), `NO_MINIMUM_TIME_DOMAINS` (19 personal-site hosts that skip the 29-minute minimum entirely; synced from NNW `LocalAccountRefresher.domainsWithNoMinimumTime` as of `4d594181f`), and the `is_no_minimum_time_domain` short-circuit runs **first** in the timing skip check, mirroring NNW's order.
* `cache.rs` — `ImageCache` with two-tier storage: in-memory `LruCache<String, Vec<u8>>` capped at 250 entries per kind (favicons + images counted separately), disk at `$XDG_CACHE_HOME/viaduct/{favicons,images}/<md5-of-url>`. Deliberately stores `Vec<u8>` (not `GdkTexture`) so the LRU stays `Send`; decode to texture happens on the GTK main thread at the call site. Public API: `favicon(url)`, `image(url)`, `color_for(s)` (port of NNW `ColorHash`, returns `#rrggbb` from MD5 of input).

### `src/parser/`

* `xml.rs` — RSS 2.0, Atom, OPML via `quick-xml`. RSS handles: RDF (`rdf:RDF`), `<content:encoded>`, `<dc:creator>`/`<dc:date>` via unprefixed local-name match, `<guid isPermaLink="false">` attribute, relative-URL resolution against home-page URL, `</rss>`/`</RDF>` stop-sentinel, MD5 synthetic IDs, RSS `<enclosure>` and MRSS `<media:content>`/`<media:thumbnail>` → `Article.attachments`, channel `<image><url>` → `ParsedFeed.icon_url`, `<language>` → `ParsedFeed.language`. Atom handles: `in_author`/`in_source` state tracking (a `<source>` block inside an entry does NOT overwrite the entry's fields), `<author><name>`/`<email>`/`<uri>` capture into `MutableAuthor`, feed-level root author propagated to authorless entries at end of parse, `<link href>` resolution against home-page URL with `AtomLinkRel` (alternate/related/enclosure/other), `</feed>` stop-sentinel, `<icon>`/`<logo>` (icon wins) → `ParsedFeed.icon_url`, `<feed xml:lang>` → `ParsedFeed.language`, `<link rel="enclosure">` → `Article.attachments`, **`type="xhtml"` raw inner HTML capture for `<content>`/`<summary>`** via `capture_atom_xhtml_inner` (re-serializes inner XML through `quick_xml::Writer` with `trim_text(false)` scoped around the capture so inline whitespace survives — port of NNW `XMLSAXParser.captureRawInnerContent`). **CDATA bodies**: both parsers handle `Event::CData` alongside `Event::Text` via `rss_handle_text_or_cdata` / `atom_handle_text_or_cdata` helpers — Sacha Chua's blog and most WordPress / Hugo / Jekyll feeds wrap their `<description>` and `<content:encoded>` payloads in CDATA, and the parser dropped them silently before v1.0.8.
* `json.rs` — JSON Feed + RSS-in-JSON via `serde_json`. MD5 synthetic IDs (not `DefaultHasher` — must stay stable across builds).
* `html.rs` — `HTMLMetadataExtractor` (finds `<link rel="alternate" type="application/rss+xml|atom+xml">` when a user pastes a bare website URL); returns an `HtmlMetadata { url_string, tags }` bag. HTML sanitization for the reading pane lives in `src/ui/article_renderer.rs` via `ammonia::Builder` with the `viaduct-img://` URL scheme allowlisted.
* `date.rs` — `DateParser` port (NNW `RSDateParser`). Permissive: W3C / ISO 8601, RFC 822 / `pubDate`, and the long tail of malformed real-world dates. Byte-level inspection for zero-alloc parsing; `chrono` for the final conversion.

### `src/ui/`

The GTK4 + libadwaita native view layer. Phase 5+ work lives here. GTK types are `!Send` — see §6 gotchas.

* `window.rs` — `ViaductWindow` subclass of `AdwApplicationWindow`. Built via `window.ui` GTK Builder XML. Owns `Arc<LocalAccount>`, `Arc<ImageCache>`, the sidebar `Delegate`/`Controller`/`DataSource`, `timeline_store`, `timeline_selection`, `selected_feed_id` (for the search scope toggle), `feed_names: OnceCell<FeedNameMap>` (feed_id → display name resolver, rebuilt from OPML on every load/import via `rebuild_feed_names_from`), and `article_display: RefCell<ArticleDisplayState>` (raw_html / extracted_html / article_url / auto_reader — single source of truth for the article pane). `wire_models()` loads OPML on startup, wires sidebar-selection → fetcher (per-feed, smart-feed, or `fetch_folder_articles` for folders) → timeline store + bulk status fetch, timeline-selection → `render_article_body`. After timeline setup it calls `install_timeline_capture_shortcuts` to install a `gtk::ShortcutController` (`PropagationPhase::Capture`) on the timeline list view so navigation keys (Down/j/n, Up/k/-, space, Shift+space, r/m, s, b/Return, Ctrl+Return, l, o, Shift+m) beat `GtkListView`'s built-in cursor handlers. `wire_search()` binds the search bar to `search_btn`, debounces 150ms, resolves the scope at query-fire time, and calls `account.search_articles_with_snippets(fts, feed_filter)`. `render_article_body()` is the single re-render path; toggle flips and async Reader-View completion both call it. Per-action bodies are `act_*` methods invoked by the `win.*` `gio::SimpleAction`s installed by `actions.rs`. Display-name fallback chain (`display_name_for_feed`): `edited_name` → parsed `name` → URL host → raw URL. Phase 13 hooks: `act_preferences` opens the prefs dialog; `act_refresh` and `refresh_specific_feeds` route through file-scope helpers `pair_feeds_with_settings` (lifts the FeedSettings-or-blank logic) and `run_refresh_with_tally` (counts `ArticleChanges.new_articles` across the cycle, drops the refresher to close the channel, awaits the drain task) and dispatch `dispatch_refresh_notification` on the GTK thread which fires `gio::Notification` via `Application::send_notification` when `notifications-on-refresh` is on. Read/unread plumbing: timeline_selection auto-marks unread → read on selection (port of NNW `TimelineViewController.tableViewSelectionDidChange`), and `refresh_unread_counts` walks `controller.root_node.child_nodes()` after every status mutation (single-article toggle, bulk mark-read, mark-older-read, smart-read advance), every refresh-cycle completion, OPML load, and OPML import — applying per-feed counts from `unread_counts_by_feed` and Today/All-Unread/Starred totals from `smart_feed_counts`, and summing folder/group totals from their walked children.
* `window.ui` — three-pane `AdwNavigationSplitView` wrapped in an `AdwToastOverlay` (`toast_overlay` template child, used by Phase 12 import/export feedback). Outer split view (Feeds sidebar) clamped 220–360 px @ 22 % fraction; inner split view (Timeline pane) clamped 320–480 px @ 32 %. Top of the file declares a `GMenu` `primary_menu` (Import OPML, Export OPML, Preferences, Keyboard Shortcuts) that `menu_btn` binds via `menu-model`. **Sidebar header bar** holds `mark_all_read_btn`, `search_btn` (toggle), `menu_btn`, plus `sync_btn` whose child is a `GtkStack` flipping between a `view-refresh-symbolic` GtkImage and a `GtkSpinner` (v1.2.0 — `set_refresh_in_progress` on the window swaps visible-child-name for refresh-in-progress feedback). **Timeline pane** has a `GtkSearchBar` containing a hbox with `search_entry` (hexpand) + `scope_toggle` ("This feed"), then a `GtkStack` (`timeline_stack`) with `content` (the `GtkScrolledWindow` → `GtkListView`) and `empty` (`AdwStatusPage` "No articles") pages. The scrolled window has `hscrollbar-policy="never"` so rows can't overflow and inflate the pane. **Article pane**'s `AdwHeaderBar` carries `reader_btn` (Reader View toggle). Article body is a `GtkStack` (`article_stack`) with `content` (a `GtkOverlay` → `WebKitWebView` + `url_overlay` GtkLabel for hovered link previews) and `empty` (`AdwStatusPage` "No article selected") pages — both stacks crossfade 150 ms. **Adaptive layout** via two `AdwBreakpoint` blocks at the window root (v1.2.0): `max-width: 900sp` collapses `inner_split_view`; `max-width: 600sp` collapses both — narrow windows reflow to a navigation stack so the app remains usable on a laptop / phone form factor.
* `actions.rs` — installs every keyboard `gio::SimpleAction` on the window's `win` action group, plus accelerators on the application. NNW's `GlobalKeyboardShortcuts.plist` keys are primary; the roadmap's friendlier aliases (Down/Up/j/k for nav, m/Enter for status/open) layer on top so both NNW and Feedly muscle memories work. `import-opml`, `export-opml`, and `preferences` register without accelerators (NNW puts them in the menu only). Action bodies live as `act_*` methods on `ViaductWindow`; this file is wiring only.
* `shortcuts.ui` — declarative `gtk::ShortcutsWindow` for the `Ctrl+?` cheat sheet. Sections mirror `actions.rs` groups (Navigation / Status / Open / Application).
* `preferences_dialog.rs` — `present(parent)` builds an `AdwPreferencesDialog` with a single "General" page. Appearance group: color-scheme `AdwComboRow` (Follow system / Force light / Force dark) + **`article_theme_row` (v1.2.0)** — `AdwComboRow` listing "Follow color scheme" plus all 9 themes (Adwaita + 8 NNW). Schema dash-form ↔ `Theme::id` underscore-form translation handled by `theme_nick_to_index` / `theme_index_to_nick`. Notifications group: `notifications-on-refresh` `AdwSwitchRow` bound bidirectionally via `gio::Settings::bind`. Color-scheme + article-theme writes go through `Settings::set_string`; external flips re-sync via `connect_changed`. When `crate::preferences::settings()` returns None (schema not installed in dev env) the dialog renders an inert "Settings unavailable" row instead of crashing.
* `reader_view.rs` — local Reader View extractor. `extract(url, existing_html)` runs the `readability` crate inside `tokio::task::spawn_blocking`. NNW deviation: NNW calls hosted Mercury (`extract.feedbin.com/parser`); we don't depend on an external service. Input HTML capped at 5 MB (`INPUT_SIZE_CAP`) before extraction to keep readability's DOM allocations under the peak budget. Returns `Result<String, ReaderError>`; the extracted HTML rides the same `article::render_html` pipeline as the raw body.
* `sidebar.rs` — `SidebarDataSource`, `SidebarTreeControllerDelegate`, `setup_sidebar_list_view`. Row factory uses a `gtk::Stack` with two pages ("icon" = `gtk::Image`, "avatar" = `adw::Avatar`); folders/smart-groups show a symbolic icon, feeds show the avatar. Smart Feeds pinned at the top (Today / All Unread / Starred). `spawn_favicon_fetch` async-loads favicons via `FeedSettings.favicon_url`/`icon_url` → `ImageCache` → `GdkTexture` → `set_custom_image`, with a stale-row guard comparing avatar text to the expected feed name. Unread badges drive off `notify::unread-count` — `connect_bind` subscribes (id stashed via unsafe `set_data("viaduct-unread-handler")`) and `connect_unbind` disconnects. `apply_unread_badge(label, count)` shows the count when `> 0` and hides otherwise. **v1.2.0 polish**: avatars 24 px (was 20), inter-element spacing 10 px, vertical row padding 2 px. The "Smart Feeds" group row gets a `viaduct-sidebar-heading` CSS class so its label renders as an uppercase letter-spaced section heading. Unread badges carry a `viaduct-unread-badge` class (pill-shaped, currentColor 10 % background, switches to translucent `accent_fg_color` on the selected row). Static CSS lives in `apply_sidebar_styling` in `main.rs`.
* `tree.rs` — `TreeController` and `TreeNode` primitives, port of NNW `RSTree`. `TreeNode` is a `glib::Object` subclass so it can live in `gio::ListModel`. `unread_count` is a `glib::Properties`-derived `u32` — setting it via `set_unread_count` fires `notify::unread-count`, which the sidebar row factory subscribes to for live badge updates without re-binding the tree.
* `batch.rs` — `BatchUpdate` analog to suppress UI notification storms.
* `coalescing_queue.rs` — `CoalescingQueue` analog for throttled, deduplicated UI operations on the main thread.
* `fetch_queue.rs` — `FetchRequestQueue` analog for cancelling stale timeline fetches on rapid sidebar clicks.
* `timeline.rs` — `ArticleNode` (glib wrapper around `Article`, plus optional `snippet` for search rows and `read`/`starred` exposed as **glib derived properties** so notify-signals drive in-place title restyling without waiting for a row recycle) + `setup_timeline_list_view(list_view, store, FeedNameMap)`. `FeedNameMap` (= `Rc<RefCell<HashMap<feed_id, display_name>>>`) is the resolver passed in by the window; the bind closure reads through it on each row, falling back to the raw `feed_id` when the OPML hasn't loaded yet. `GtkListView` + `GtkSignalListItemFactory` with **strict widget recycling**. Rendering 10,000 articles must cost the same RAM as rendering 10. **v1.2.0 row layout**: each row is `row_hbox` containing `content_vbox` (hexpand=true: title + media indicator → feed-name → 2-line preview) and a separate top-aligned `date_label` (right column, 80 px hard `set_size_request` floor, right-aligned with `xalign=1.0`). All three labels carry `set_max_width_chars` caps (32 / 32 / 48) so the row's natural width stays bounded — without this, smart-feed views with long aggregated titles would inflate the timeline pane through `AdwNavigationSplitView::sidebar-width-fraction`. Title gets `EllipsizeMode::End`; preview wraps + ellipsizes. **Date formatter** is `format_relative_date` — `Just now` < 1 min, `5h ago` within the day, `Yesterday`, weekday name within the past week, `Mar 19` within the year, `Mar 19, 2025` older. **Preview cleaner** is `strip_html_for_preview` — drops tags, decodes 14 common HTML entities (`&amp;`, `&mdash;`, `&rsquo;`, etc.), collapses whitespace. Falls back to `content_html` when summary and content_text are both empty. **Read/unread visual**: `apply_read_styling` toggles `heading`/`dim-label` on the title; `apply_row_read_styling` adds `viaduct-row-read` (opacity 0.55) to feed-name + preview + date so the entire row dims when read. `connect_bind` connects a `notify::read` handler that re-runs both stylings; the handler-id is stashed via `unsafe item.set_data` and disconnected in `connect_unbind` so recycled rows don't accumulate handlers. Preview prefers `node.snippet()` (FTS5 excerpt) over `article.summary`/`content_text`. Returns `SingleSelection` so the window can drive article rendering and keyboard navigation from it.
* `article_renderer.rs` — reading pane (Phase 6 + v1.2.0 polish). Single neutered `WebKitWebView` instance drives all article rendering.
  - **Lockdown profile** via `apply_locked_down_settings`: JS / WebGL / WebRTC / plugins / DevTools / LocalStorage / IndexedDB / app cache / fullscreen / window-open / media-autoplay / back-forward gestures all OFF; `media_playback_requires_user_gesture(true)`.
  - **`install_link_interceptor`**: `decide-policy` cancels every `LinkClicked` / `FormSubmitted` / `NewWindowAction` and routes the URL to `gio::AppInfo::launch_default_for_uri` (xdg-open). `Other` / `Reload` / `BackForward` allowed through for `load_html`.
  - **`install_hover_url_overlay`**: `mouse-target-changed` updates a `gtk::Label` overlay child (`osd` + `caption` classes) so hovered link URLs preview in the bottom-left.
  - **`install_image_uri_scheme`**: registers `viaduct-img://` on the default `WebContext`. Handler clones the URISchemeRequest, hops to tokio for `ImageCache::image()`, then back to GTK to call `request.finish()` with a `gio::MemoryInputStream`. Article HTML runs through `sanitize_and_rewrite_image_srcs` first — `ammonia::Builder` with `viaduct-img` allowlisted + an `attribute_filter` that rewrites every `img@src` http(s) URL to `viaduct-img://i/<percent-encoded-original>`. Inlined `percent_encode`/`percent_decode` helpers (no `percent-encoding` crate dep).
  - **`install_font_uri_scheme` (v1.2.0)**: registers `viaduct-font://` for bundled fonts. `BUNDLED_FONTS` const array carries `include_bytes!` slices for Atkinson Hyperlegible Next (Regular/Bold/Italic/BoldItalic). `font_face_css()` returns four `@font-face` declarations referencing `viaduct-font://atkinson/{regular,bold,italic,bolditalic}` and is prepended to every theme stylesheet so the Hyperlegible theme renders correctly even on systems that don't ship the font.
  - **Themes (v1.2.0)**: `THEMES` const array carries 9 entries — `Adwaita` (the v1.2.0 GNOME-native default with `prefers-color-scheme` baked in, `accent_hex: None` so GNOME's system accent surfaces unchanged) plus the 8 NNW-ported bundles (Sepia, Appanoose, Biblioteca, Hyperlegible, NewsFax, Promenade, Tiqoe Dark, Verdana Revival). Each `Theme` struct carries `template`, `stylesheet`, `accent_hex: Option<&'static str>` (None means "no override"), and `dark_overlay: Option<&'static str>` (per-theme hand-tuned `prefers-color-scheme: dark` CSS for the 7 light NNW themes). `select_for_dark_mode` pairs Sepia ↔ Tiqoe Dark for the "auto" GSetting. `theme_by_id` falls back to Adwaita on unknown id.
  - **`apply_app_accent(Option<&str>)`** (v1.2.0): installs a CSS provider on the default `gdk::Display` overriding libadwaita's accent across three layers — legacy `@define-color`, modern `:root` CSS custom properties, and selector-targeted overrides for the highest-traffic accent widgets (suggested-action buttons, switches, focus rings, listview row selection, text selection, link buttons). Sits at `STYLE_PROVIDER_PRIORITY_USER + 100` so it beats GNOME 47+'s system-accent integration. `None` removes the active provider so GNOME's system accent comes back through (Adwaita theme uses this).
  - **Macro engine**: `render_with_macros` is a linear-scan port of NNW's `RSCore.MacroProcessor.processMacros()` — `[[key]]` delimiters, unknown keys preserved as literal `[[key]]`. `ArticleSubstitutions` mirrors NNW's `articleSubstitutions()` exactly so bundled themes work without modification.
  - **Page wrapper** (`data/themes/page.html`, NNW-shape) carries the strict CSP meta tag — `default-src 'none'; img-src viaduct-img: data:; font-src viaduct-font:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'` — so the WebProcess gets ZERO direct internet access.
  - **Style cascade in `render_themed`** (v1.2.0): `font_face_css()` → theme's stylesheet → optional `dark_overlay` → `VIADUCT_PANE_OVERRIDE_CSS`. The override re-enables `html, body { overflow: auto !important }` so WebKit can scroll itself (NNW themes set `overflow: hidden` assuming a parent NSScrollView; we removed our equivalent GtkScrolledWindow in v1.1.0-pre1.6) plus styles a subtle WebKit scrollbar (8 px, gray thumb, transparent track).
  - Two-pass substitution: inner = theme template + article subs; outer = page wrapper + (style, baseURL, title, inner). Final HTML rides `WebView::load_html(html, base_uri)`.

---

## 4. NetNewsWire Porting Philosophy

viaduct is a **translation**, not a reimagining. This is the most important section of this file.

### The Rule

Port. Don't invent.

When implementing anything — a parser, a queue, a refresher, a cache eviction policy, an error-handling branch — find the corresponding code in `.netnewswire/` and translate it to Rust. Match the structure. Match the names (adapted to Rust conventions). Match the edge cases, including the ones that look weird. NetNewsWire has been shipped and maintained since 2002 — the weird-looking branches are almost always there because someone's feed broke without them.

**Do not:**
* Invent your own architecture because the Swift approach "isn't idiomatic Rust."
* Substitute a crate with different semantics because it's more popular.
* Skip porting a branch because you can't immediately think of when it fires.
* "Improve" anything on the first pass. Make it work *the NNW way*, then we discuss.
* Build a feature from scratch when NNW already has it. If you can't find it, ask — it's probably there and you missed it.

### Workflow

1. Identify the NetNewsWire file(s) that implement the feature. Read them.
2. Translate to Rust, preserving structure and behavior.
3. Adapt only where Swift/Apple primitives have no direct analog (see §4, "Unless It Has To").
4. Verify behavior matches. Then we ship.

Ambiguous user request → go read `.netnewswire/`. That is the default plan.

### What NOT to Port

**Only `.netnewswire/Modules/` and `.netnewswire/Shared/` matter for logic porting.** The rest of the NNW tree is platform-specific UI we're replacing with GTK — ignore it entirely:

* `.netnewswire/iOS/`, `.netnewswire/Mac/`, `.netnewswire/Widget/` — AppKit / UIKit views, view controllers, storyboards. We are building a native GTK4 UI from scratch. Don't port these.
* `.netnewswire/Intents/`, `.netnewswire/AppleScript/` — Siri Shortcuts and AppleScript bindings. No Linux analog; skip.
* `.netnewswire/AppStore/`, `.netnewswire/Appcasts/` — distribution plumbing for Apple platforms. Irrelevant; we ship Flatpak.
* `.netnewswire/buildscripts/`, `.netnewswire/xcconfig/`, `.netnewswire/NetNewsWire.xcodeproj/` — Xcode build system. Irrelevant.

**viaduct supports local accounts and Inoreader.** We are not porting other remote-account or sync code. This is a hard scope boundary, not a "we'll get to it later."

Explicitly skip the following directories/modules when porting:

* `.netnewswire/Modules/CloudKitSync/` — iCloud sync. Skip entirely.
* `.netnewswire/Modules/NewsBlur/` — NewsBlur API client. Skip.
* `.netnewswire/Modules/SyncDatabase/` — exists to support remote sync. Port this to support Inoreader.
* `.netnewswire/Modules/Secrets/` — holds credentials for remote services. Port this to support Inoreader credentials.
* Anything inside `Modules/Account/` related to `FeedbinAccountDelegate`, `FeedlyAccountDelegate`, `FreshRSSAccountDelegate`, `NewsBlurAccountDelegate`, `BazQuxAccountDelegate`, `TheOldReaderAccountDelegate`. Port only `LocalAccountDelegate`, `InoreaderAccountDelegate`, and the shared account scaffolding they need.

When porting a file that mixes local-account/Inoreader logic with other remote-sync logic, take the local and Inoreader branches and drop the rest. Do not leave stubs, `todo!()` placeholders, or "future sync" interfaces — cut the other code out cleanly. If a generic abstraction exists solely to accommodate unsupported remote accounts, collapse it to the local and Inoreader shapes.

If you're unsure whether something is local-only/Inoreader or other remote-sync, ask. Don't port "just in case."

### Where to map

| Concept | NNW | viaduct |
|---|---|---|
| Parsing | `Modules/RSParser` | `src/parser/` |
| Dates | `RSDateParser` | `src/parser/date.rs` (`chrono`) |
| Articles store | `ArticlesDatabase` | `src/database/articles.rs` (`rusqlite`) |
| Settings store | `FeedMetadataFile` + per-feed metadata | `src/database/settings.rs` |
| Feed hierarchy | `OPMLFile` | `src/database/opml.rs` |
| Single-writer serialization | `DatabaseQueue` | `src/database/worker.rs` (tokio mpsc) |
| Coalescing UI updates | `CoalescingQueue`, `BatchUpdate` | tokio channels + debouncing |
| Refresh orchestration | `LocalAccountRefresher` | `src/network/fetcher.rs` |
| Download coalescing | `DownloadSession` | `src/network/fetcher.rs` |

### The "Unless It Has To" Rule

Deviate **only** when Swift/Apple primitives have no direct analog. Approved deviations:

* **Concurrency:** GCD / `@MainActor` → `tokio` multi-threaded runtime + `mpsc` for work, `glib::MainContext::channel` for hopping back to the GTK thread.
* **UI:** AppKit / UIKit view recycling → GTK4 `gio::ListModel` + `GtkListView` + `GtkSignalListItemFactory`.
* **Reader View:** NNW calls a remote Mercury service. We must run extraction locally (Phase 10), RAM-gated.
* **FTS:** NNW uses FTS4. We use FTS5.

That's the full list. If you think you need another deviation, stop and ask.

---

## 5. GNOME 50+ Exclusivity

Target stack: **GNOME 50+**, **GTK4 ≥ 4.16**, **libadwaita ≥ 1.7**, Rust 2024 edition.

* No polyfills, no fallback paths, no conditional compilation for older GNOME/GTK versions.
* Use modern widgets only: `AdwNavigationSplitView`, `AdwApplicationWindow`, `AdwHeaderBar`, etc. No `AdwLeaflet`, no deprecated APIs.
* UI is declarative: standard `.ui` GTK Builder XML files, or constructed natively against libadwaita 1.7+.
* Adhere to the latest GNOME HIG.

---

## 6. Data, Storage, and Thread Model (invariants)

* **Storage root:** `$XDG_DATA_HOME/viaduct/` holds `local.opml`, `articles.sqlite`, `feed-settings.sqlite`. `$XDG_CACHE_HOME/viaduct/` holds `favicons/` and `images/`.
* **OPML is the feed-hierarchy source of truth.** Not SQL. Do not add a `feeds` table with folder FK columns.
* **WAL mode** on both SQLite DBs, always. FTS5 on `articles`.
* **Single writer task.** All SQLite writes funnel through `src/database/worker.rs`. The GTK thread only ever sends commands or reads from in-memory models.
* **UI delivery:** background tasks emit `ArticleChanges { new, updated, deleted }` batches; these cross into the GTK main loop via `glib::MainContext::channel`. Bulk operations coalesce through a `BatchUpdate` primitive to avoid notification storms.
* **Article DB ID = `md5("{feed_id} {unique_id}")`.** Ported from NNW `Article.calculatedArticleID(feedID:uniqueID:)`. The `unique_id` is whatever the feed's `<guid>` / Atom `<id>` / JSON Feed `id` gave us, and the parser falls back to MD5 of a deterministic concatenation when that's missing. Under no circumstances use `DefaultHasher` for synthetic IDs — it's not stable across builds and will orphan status rows on every restart.
* **`parsed_to_article` truncates dates to whole seconds** before inserting, matching the integer column type. Don't remove this — without it, every refresh will flag every article as "updated" because the in-memory `DateTime<Utc>` has nanosecond precision that disagrees with the round-tripped seconds value.
* **`delete_settings_for_feeds_not_in(Vec::new())` must be a no-op.** NNW `guard !feedURLs.isEmpty else { return }`. We used to `DELETE FROM feed_settings` on empty input, which nuked the entire settings DB whenever startup OPML was empty. Regression-tested; don't "simplify" the early return away.
* **Status rows outlive articles.** The `articles_ad_lookup` trigger cascades `authorsLookup` deletes but `statuses` deletes are intentionally left out — if a feed re-adds an article after a retention sweep, NNW expects the old read/starred state to come back with it.

### gtk-rs Gotchas (Swift→Rust Pitfalls)

Swift's memory model is forgiving in ways Rust's isn't. Specifically, porting Swift code that freely captures `self` in closures will fight you here.

* **`GObject` is `!Send`.** GTK widgets cannot cross threads. Do not hold a `GtkWidget` inside a struct that will be passed to `tokio::spawn`. Move *data* across the boundary, not widgets. The pattern: background task sends `ArticleChanges` → GTK-thread handler receives it via `glib::MainContext::channel` → handler updates widgets on the main thread.
* **Use `glib::clone!` with weak refs.** A signal handler that captures `self` strongly creates a reference cycle (widget → handler closure → widget) that GTK will not clean up. Always prefer `glib::clone!(#[weak] self_ => async move { ... })` or `#[weak_allow_none]`. Strong captures (`#[strong]`) are acceptable only when you explicitly want the closure to extend the widget's lifetime and you've thought about it.
* **Tokio runtime lives outside the GTK loop.** Spawn background work with `tokio::spawn` against the runtime built in `main.rs`; spawn UI-touching work with `glib::spawn_future_local`. Never `.await` on a tokio future from a GTK signal handler — route through a channel instead.
* **Tokio I/O Requires a Reactor:** If you call Tokio network/filesystem APIs (`reqwest`, `tokio::fs`) from inside a `glib::spawn_future_local` block, they will panic with "there is no reactor running". GTK's async executor is not backed by Tokio. Always use `crate::spawn_on_runtime` to execute I/O work, then hand the result back to the GTK thread.
* **`glib::MainContext::channel` is one-way.** It delivers from a worker to the GTK loop. For GTK → worker, use the `mpsc::Sender` held by `src/database/worker.rs` (or equivalent). Don't try to make one channel work both directions.
* **Swift's `weak self` ≠ Rust's `Weak<T>`.** When porting a Swift closure that uses `[weak self]`, translate to `glib::clone!(#[weak] ...)` for GObject-rooted captures, or `Weak<RefCell<T>>` / `Arc::downgrade` for plain Rust types. Don't paper over it with `Arc` and hope.

---

## 7. Agent Operational Constraints

Non-negotiable unless the user overrides in conversation.

1. **Memory budget is supreme.** Idle: **100–300 MB** after full sync + image cache warm. Peak: **< 500 MB** across every supported operation. Any feature proposal that plausibly busts this is rejected on sight (this includes embedded WebKit, in-process image decoding of uncapped size, eager full-text extraction, etc.). Every major phase ends with a `heaptrack`/`massif` checkpoint.
2. **Never block the GTK thread.** All I/O (network, disk, SQLite writes, XML parsing, HTML sanitization) runs in tokio tasks. The main thread renders and reads — nothing else.
3. **No remote sync engines in v1.0.** No Feedbin, Miniflux, FreshRSS, CloudKit, NewsBlur, Inoreader. Local OPML account only. Do not even scaffold interfaces "in case."
4. **Neutered WebKit Instance** (shipped v1.1.0). Exactly ONE `WebKitWebView` instance lives in the article pane. JS / WebGL / WebRTC / plugins / DevTools / LocalStorage / IndexedDB / app cache / fullscreen are all OFF. Strict CSP (`default-src 'none'; img-src viaduct-img: data:; style-src 'unsafe-inline'`) blocks every external load. The `viaduct-img://` URI scheme routes images through our `ImageCache` so WebKit gets ZERO direct internet access. Link clicks intercepted via `decide-policy` and routed to `xdg-open`. Real-world session peak measured at **292 MB / 500 MB budget**. Adding additional WebViews (e.g. for popovers, second windows) is forbidden — typography fidelity comes from this single locked-down instance only.
5. **Error types:** public / library-surface errors use `thiserror` variants under `ViaductError`. `anyhow` is tolerated only in binary glue (`main.rs`, scratch bins). Do not add `anyhow::Result` to module APIs.
6. **No scope creep.** Don't refactor adjacent code while passing through. Don't add abstractions for hypothetical future callers. Three similar lines beats a premature helper.
7. **No new top-level dependencies without asking.** The crate list in `Cargo.toml` is deliberate. If you think you need another, propose it with the specific NNW symbol you're porting.
8. **Default to no comments.** Only write one when the *why* is non-obvious (hidden constraint, NNW-specific quirk being preserved, workaround for a real bug). Never leave "ported from X.swift:123" breadcrumbs — git blame + this file are enough.
9. **CI must stay green.** Before reporting a task complete: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`.
10. **Don't commit or push.** Ever, unless the user explicitly asks.

---

## 8. Quick Reference

| Thing | Where |
|---|---|
| Current phase status | `roadmap.md` |
| Shipped features | `patchnotes.md` |
| Architecture invariants | `spec.md` §2 |
| Keyboard shortcut table | `spec.md` §5 |
| Success criteria for 1.0 | `spec.md` §10, `roadmap.md` bottom |
| NetNewsWire source | `.netnewswire/` (do not re-clone) |
| Memory profiling target | idle 100–300 MB, peak < 500 MB |

When in doubt: read the NNW source in `.netnewswire/`, then ask before deviating.
