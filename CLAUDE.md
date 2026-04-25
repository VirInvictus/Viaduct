# Viaduct — AI Agent Architecture & Guidelines

This file is the definitive reference for AI agents (Claude, Gemini, etc.) and human contributors working on **viaduct**. Project-scoped rules here **override** anything in `~/CLAUDE.md` for work inside this repo.

Before you touch code, read `README.md`, `spec.md`, `roadmap.md`, and `patchnotes.md`. They carry invariants this file does not repeat.

---

## 1. What viaduct Is

**viaduct is a port of NetNewsWire to GTK4 and Rust. That is the entire project.**

NetNewsWire is open-source, battle-tested, and has already solved every hard problem in this domain — feed parsing, date parsing, conditional GETs, article deduplication, coalesced UI updates, memory discipline. We are translating that solution from Swift/AppKit to Rust/GTK4 so Linux users can have it. **We are not designing a new RSS reader.** We are not re-litigating its architectural choices. We port first, then we make it work on Linux.

If you catch yourself thinking "I have a better idea" — you don't. Go read the corresponding Swift file in `.netnewswire/` and port *that*. The app is local-only, no sync backends, no WebKit, strict memory budget. Targets **GNOME 50+** and **libadwaita 1.7+** on Wayland.

Current version: **v0.5.0** (Phases 0–8 complete; Phase 9 Keyboard Spatial Navigation next). See `roadmap.md` for the live phase plan and `patchnotes.md` for the shipped log.

**License:** MIT. **Edition:** Rust 2024.

---

## 2. Source of Truth: `.netnewswire/`

A full clone of NetNewsWire lives at `.netnewswire/` in this repo. **It is already there — do not re-clone, re-download, or `git submodule add` it.** Treat it as read-only reference material.

> **Note on Latest Upstream Sync (April 23, 2026):**
> The `.netnewswire` reference folder has been updated to the latest upstream state (Commit `ec06277`). Key architectural and feature changes to be aware of when porting:
> *   **C-to-Swift Port (`StripHTML`):** A major refactor has moved the core HTML stripping logic from legacy C (`striphtml.c`) to a native Swift implementation (`StripHTML.swift`) within the `RSCore` module. This includes significant performance optimizations and new test suites for `CollapsingWhitespace` and `StripHTML`.
> *   **Swift 6 Integration:** Ongoing work to align `RSCore` and `RSParser` with Swift 6 concurrency and language modes.
> *   **HTML Entity Decoding:** Enhanced robustness in `RSParser` for handling complex HTML entities within the XML module.
> *   **New UI Themes:** Added *Biblioteca*, *Tiqoe Dark*, and *Verdana Revival* themes (check `Shared/Resources/`).
> *   **Sync Optimization:** Implementation of the "Do not sync unread article content" toggle, which reduces iCloud database bloat.
> *   **Enhanced Performance Testing:** New benchmarks for `ArticleStringFormatter` and `NSAttributedString` HTML rendering have been added to the test targets.

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
* `README.md`, `spec.md`, `roadmap.md`, `patchnotes.md` — authoritative design docs.
* `.github/workflows/ci.yml` — Ubuntu runner: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`. All three must stay green.

### `src/`

The crate is **lib + bin**. `src/lib.rs` is the library root (declares all module trees publicly). `src/main.rs` is the GTK application binary; auxiliary binaries live under `src/bin/`. This split lets profiling harnesses and future CLI tools share the same modules instead of duplicating them.

* `lib.rs` — library root. Just `pub mod` declarations for `database`, `error`, `models`, `network`, `parser`, `paths`, `ui`.
* `main.rs` — GTK entrypoint. Initializes tracing (`EnvFilter`, `RUST_LOG` controls verbosity), boots the global tokio runtime, calls `paths::ensure_dirs()`, constructs `LocalAccount`, launches the `adw::Application`. Uses the library via `use viaduct::…`.
* `bin/mem_check.rs` — Phase 7 memory checkpoint harness. Synthesizes a 500-feed × 10-article corpus in a tempdir XDG, runs `LocalAccount::update_feed` through the real single-writer worker, reads `VmHWM` from `/proc/self/status`, and pass/fails against the 500 MB peak budget. `cargo run --release --bin mem_check`. Current release-build peak is ~29 MB for the insert path alone.
* `paths.rs` — XDG resolution. Data → `$XDG_DATA_HOME/viaduct/` (fallback `~/.local/share/viaduct/`). Cache → `$XDG_CACHE_HOME/viaduct/` (fallback `~/.cache/viaduct/`). Resolves `local.opml`, `articles.sqlite`, `feed-settings.sqlite`, `favicons/`, `images/`.
* `models.rs` — domain types: `Feed`, `Folder`, `Article`, `ArticleStatus`, `Author`, `FeedSettings`, `ParsedItem`, `ParsedFeed`, `ArticleChanges { new, updated, deleted }`.
* `error.rs` — `ViaductError` (top-level) → `DatabaseError`, `NetworkError`, `ParseError`. All via `thiserror`. Each variant preserves its source error (`rusqlite`, `reqwest`, `quick_xml`, `serde_json`, `url::ParseError`).

### `src/database/`

Three stores, strict separation, single writer. Port of NNW's three-way split.

* `articles.rs` — `ArticlesDatabase`. Tables: `articles` (with `attachments JSON` column added by an idempotent ALTER for pre-existing DBs), `statuses`, `authors`, `authorsLookup`, FTS5 virtual `search`. Triggers: FTS index maintenance on insert/update/delete, and `articles_ad_lookup` to cascade-clean `authorsLookup` when an article is removed (status rows are deliberately NOT cascaded — NNW keeps them for reappearing articles). WAL + `synchronous=NORMAL`, `temp_store=MEMORY`, `mmap_size=30GB` (sparse mapping, not actual allocation). Public helpers: `article_id_for(feed_id, unique_id)` — MD5 of `"{feed_id} {unique_id}"` per NNW `Article.calculatedArticleID`; `parsed_to_article` which truncates `DateTime` to second precision so DB round-trips don't flag every article as "updated". Main op is `ArticlesDbOp::UpdateFeed { feed_id, items, delete_older, reply }` which produces a real `ArticleChanges` diff (new / updated / deleted + status rows for new articles; stale items >6 months default to `read=1`; orphans non-starred >30d get deleted when `delete_older=true`). Other ops: `Search`, `SearchWithSnippets` (FTS5 `snippet()` + optional feed-scope filter), `FetchStatusesByIds` (bulk status lookup for keyboard navigation, chunked at 500 IDs to stay under SQLite's parameter limit).
* `settings.rs` — `FeedSettingsDatabase`. Per-feed cache: `feed_id` (PK, string — note NNW uses `feedURL` as PK, we diverge), `feed_url`, `home_page_url`, `icon_url`, `favicon_url`, `edited_name`, `content_hash`, conditional-GET (`etag`, `last_modified`, `date_created` = when those were received), cache-control (`max_age`), `authors_json`, `folder_relationship_json`, `last_check_date`, `reader_view_always_enabled`. `delete_settings_for_feeds_not_in` early-returns on empty input (regression-tested) — do NOT "simplify" that branch back to a bare DELETE or you'll wipe every row.
* `opml.rs` — OPML on disk is the source of truth for the feed/folder hierarchy. Coalesced save (~500ms debounce), atomic temp-file + rename. `OpmlWriter::spawn` owns its own tokio task; `save(OpmlFile)` queues and awaits the next flush.
* `accounts.rs` — `LocalAccount` orchestrator owning the OPML file + both DBs. Public async API: `load_opml`, `save_opml`, `batch_insert_articles`, `upsert_statuses`, `fetch_articles_by_feed`, `fetch_unread_articles`, `fetch_starred_articles`, `fetch_today_articles`, `search_articles`, `fetch_feed_settings`, `upsert_feed_settings`, `update_feed` (the new diff path), `cleanup_orphaned_settings`.
* `worker.rs` — **single writer**: `std::thread::spawn` pulls `DbOp`s off a tokio `mpsc::Receiver` via `blocking_recv` and dispatches to `articles::handle_op` or `settings::handle_op`. The GTK thread holds only a `Sender`; it never blocks on SQLite. *No panic supervisor today — tracked in Phase 16.*

### `src/network/`

* `fetcher.rs` — `Fetcher` (reqwest, rustls-tls, HTTP/2 auto-negotiated, UA `Viaduct/1.0 (Linux; GTK4)`) + `DownloadSession` analog that coalesces duplicate in-flight URLs via `broadcast::channel` and tracks per-host cooldowns after 429. `LocalAccountRefresher::new(account: Arc<LocalAccount>, changes_sender)` — the refresher pipeline actually does parse + diff + persist: conditional-GET headers from `FeedSettingsDatabase`, short-circuits on 304 (updates `last_check_date` only), content-hash short-circuits on byte-identical bodies, calls `parser::parse` → `account.update_feed` → emits real `ArticleChanges`, persists ETag/Last-Modified/`content_hash`/`last_check_date`/`max_age` back to settings. Implements NNW's **8-day conditional-GET expiry** (catches servers that always 304; openrss.org + rachelbythebay.com are exempt), **5-hour cap** on `Cache-Control: max-age` (openrss.org exempt), and **25-hour special-case cutoff** for high-frequency feeds. Skip rules: twitter.com/x.com, cache-control freshness window, 29-min minimum between checks.
* `cache.rs` — `ImageCache` with two-tier storage: in-memory `LruCache<String, Vec<u8>>` capped at 250 entries per kind (favicons + images counted separately), disk at `$XDG_CACHE_HOME/viaduct/{favicons,images}/<md5-of-url>`. Deliberately stores `Vec<u8>` (not `GdkTexture`) so the LRU stays `Send`; decode to texture happens on the GTK main thread at the call site. Public API: `favicon(url)`, `image(url)`, `color_for(s)` (port of NNW `ColorHash`, returns `#rrggbb` from MD5 of input).

### `src/parser/`

* `xml.rs` — RSS 2.0, Atom, OPML via `quick-xml`. RSS handles: RDF (`rdf:RDF`), `<content:encoded>`, `<dc:creator>`/`<dc:date>` via unprefixed local-name match, `<guid isPermaLink="false">` attribute, relative-URL resolution against home-page URL, `</rss>`/`</RDF>` stop-sentinel, MD5 synthetic IDs. Atom handles: `in_author`/`in_source` state tracking (a `<source>` block inside an entry does NOT overwrite the entry's fields), `<author><name>`/`<email>`/`<uri>` capture into `MutableAuthor`, feed-level root author propagated to authorless entries at end of parse, `<link href>` resolution against home-page URL with `AtomLinkRel` (alternate/related/enclosure/other), `</feed>` stop-sentinel. **Does NOT yet parse**: RSS `<enclosure>`/`<media:*>`, channel `<image>`, feed `<language>`, Atom `type="xhtml"` raw inner HTML — these require `ParsedFeed`/`ParsedItem` model changes and are tracked under Phase 11 "Parser fidelity follow-ups".
* `json.rs` — JSON Feed + RSS-in-JSON via `serde_json`. MD5 synthetic IDs (not `DefaultHasher` — must stay stable across builds).
* `html.rs` — `HTMLMetadataExtractor` (finds `<link rel="alternate" type="application/rss+xml|atom+xml">` when a user pastes a bare website URL); returns an `HtmlMetadata { url_string, tags }` bag. HTML sanitization for the reading pane lives in `src/ui/article.rs` via `ammonia::clean`.
* `date.rs` — `DateParser` port (NNW `RSDateParser`). Permissive: W3C / ISO 8601, RFC 822 / `pubDate`, and the long tail of malformed real-world dates. Byte-level inspection for zero-alloc parsing; `chrono` for the final conversion.

### `src/ui/`

The GTK4 + libadwaita native view layer. Phase 5+ work lives here. GTK types are `!Send` — see §6 gotchas.

* `window.rs` — `ViaductWindow` subclass of `AdwApplicationWindow`. Built via `window.ui` GTK Builder XML. Owns `Arc<LocalAccount>`, `Arc<ImageCache>`, the sidebar `Delegate`/`Controller`/`DataSource`, `timeline_store`, `timeline_selection`, `selected_feed_id` (for the search scope toggle), and `article_display: RefCell<ArticleDisplayState>` (raw_html / extracted_html / article_url / auto_reader — single source of truth for the article pane). `wire_models()` loads OPML on startup, wires sidebar-selection → fetcher (per-feed, smart-feed, or `fetch_folder_articles` for folders) → timeline store + bulk status fetch, timeline-selection → `render_article_body`. `wire_search()` binds the search bar to `search_btn`, debounces 150ms, resolves the scope at query-fire time, and calls `account.search_articles_with_snippets(fts, feed_filter)`. `render_article_body()` is the single re-render path; toggle flips and async Reader-View completion both call it. Per-action bodies are `act_*` methods invoked by the `win.*` `gio::SimpleAction`s installed by `actions.rs`.
* `window.ui` — three-pane `AdwNavigationSplitView` scaffolding. Sidebar header bar holds `mark_all_read_btn`, `search_btn` (toggle), `menu_btn`. Timeline pane has a `GtkSearchBar` containing a hbox with `search_entry` (hexpand) + `scope_toggle` ("This feed"). Article pane's `AdwHeaderBar` carries `reader_btn` (Reader View toggle). Article body is a `GtkScrolledWindow` → `GtkTextView`.
* `actions.rs` — installs every keyboard `gio::SimpleAction` on the window's `win` action group, plus accelerators on the application. NNW's `GlobalKeyboardShortcuts.plist` keys are primary; the roadmap's friendlier aliases (Down/Up/j/k for nav, m/Enter for status/open) layer on top so both NNW and Feedly muscle memories work. Action bodies live as `act_*` methods on `ViaductWindow`; this file is wiring only.
* `shortcuts.ui` — declarative `gtk::ShortcutsWindow` for the `Ctrl+?` cheat sheet. Sections mirror `actions.rs` groups (Navigation / Status / Open / Application).
* `reader_view.rs` — local Reader View extractor. `extract(url, existing_html)` runs the `readability` crate inside `tokio::task::spawn_blocking`. NNW deviation: NNW calls hosted Mercury (`extract.feedbin.com/parser`); we don't depend on an external service. Input HTML capped at 5 MB (`INPUT_SIZE_CAP`) before extraction to keep readability's DOM allocations under the peak budget. Returns `Result<String, ReaderError>`; the extracted HTML rides the same `article::render_html` pipeline as the raw body.
* `sidebar.rs` — `SidebarDataSource`, `SidebarTreeControllerDelegate`, `setup_sidebar_list_view`. Row factory uses a `gtk::Stack` with two pages ("icon" = `gtk::Image`, "avatar" = `adw::Avatar`); folders/smart-groups show a symbolic icon, feeds show the avatar. Smart Feeds pinned at the top (Today / All Unread / Starred). `spawn_favicon_fetch` async-loads favicons via `FeedSettings.favicon_url`/`icon_url` → `ImageCache` → `GdkTexture` → `set_custom_image`, with a stale-row guard comparing avatar text to the expected feed name. Unread badges.
* `tree.rs` — `TreeController` and `TreeNode` primitives, port of NNW `RSTree`. `TreeNode` is a `glib::Object` subclass so it can live in `gio::ListModel`.
* `batch.rs` — `BatchUpdate` analog to suppress UI notification storms.
* `coalescing_queue.rs` — `CoalescingQueue` analog for throttled, deduplicated UI operations on the main thread.
* `fetch_queue.rs` — `FetchRequestQueue` analog for cancelling stale timeline fetches on rapid sidebar clicks.
* `timeline.rs` — `ArticleNode` (glib wrapper around `Article`, plus optional `snippet` for search rows and `read`/`starred` cells populated by the bulk-status fetch after each timeline load) + `setup_timeline_list_view`. `GtkListView` + `GtkSignalListItemFactory` with **strict widget recycling**. Rendering 10,000 articles must cost the same RAM as rendering 10. Title, media indicator (icon + count badge when `article.attachments` is non-empty), date, feed id, 2-line preview. Preview prefers `node.snippet()` (FTS5 excerpt) over `article.summary`/`content_text`. Returns `SingleSelection` so the window can drive article rendering and keyboard navigation from it.
* `article.rs` — reading pane. `render_html(text_view, html, Option<Arc<ImageCache>>)` runs `ammonia::clean` → `quick-xml` walk → `GtkTextTag` ranges in the `GtkTextBuffer`. Block tags: h1–h6, p, blockquote, pre/code-block, hr. Inline tags: strong/b, em/i, code (monospace), a (per-link unique `link:<href>` tag, click routed to `gio::AppInfo::launch_default_for_uri`). Lists: ul bullets, ol numbering. `<img>` with absolute `http(s)` src inserts a `TextChildAnchor` + anchored `gtk::Picture`, async-loaded via `ImageCache`, capped at 600px display width. **WebKit is forbidden.**

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
* **`glib::MainContext::channel` is one-way.** It delivers from a worker to the GTK loop. For GTK → worker, use the `mpsc::Sender` held by `src/database/worker.rs` (or equivalent). Don't try to make one channel work both directions.
* **Swift's `weak self` ≠ Rust's `Weak<T>`.** When porting a Swift closure that uses `[weak self]`, translate to `glib::clone!(#[weak] ...)` for GObject-rooted captures, or `Weak<RefCell<T>>` / `Arc::downgrade` for plain Rust types. Don't paper over it with `Arc` and hope.

---

## 7. Agent Operational Constraints

Non-negotiable unless the user overrides in conversation.

1. **Memory budget is supreme.** Idle: **100–300 MB** after full sync + image cache warm. Peak: **< 500 MB** across every supported operation. Any feature proposal that plausibly busts this is rejected on sight (this includes embedded WebKit, in-process image decoding of uncapped size, eager full-text extraction, etc.). Every major phase ends with a `heaptrack`/`massif` checkpoint.
2. **Never block the GTK thread.** All I/O (network, disk, SQLite writes, XML parsing, HTML sanitization) runs in tokio tasks. The main thread renders and reads — nothing else.
3. **No remote sync engines in v1.0.** No Feedbin, Miniflux, FreshRSS, CloudKit, NewsBlur, Inoreader. Local OPML account only. Do not even scaffold interfaces "in case."
4. **No WebKit. Ever.** Article bodies render into a `GtkTextView` via `GtkTextTag`. Interactive pages open via `xdg-open` (Enter key).
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
