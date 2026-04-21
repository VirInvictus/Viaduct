# Viaduct — AI Agent Architecture & Guidelines

This file is the definitive reference for AI agents (Claude, Gemini, etc.) and human contributors working on **viaduct**. Project-scoped rules here **override** anything in `~/CLAUDE.md` for work inside this repo.

Before you touch code, read `README.md`, `spec.md`, `roadmap.md`, and `patchnotes.md`. They carry invariants this file does not repeat.

---

## 1. What viaduct Is

**viaduct is a port of NetNewsWire to GTK4 and Rust. That is the entire project.**

NetNewsWire is open-source, battle-tested, and has already solved every hard problem in this domain — feed parsing, date parsing, conditional GETs, article deduplication, coalesced UI updates, memory discipline. We are translating that solution from Swift/AppKit to Rust/GTK4 so Linux users can have it. **We are not designing a new RSS reader.** We are not re-litigating its architectural choices. We port first, then we make it work on Linux.

If you catch yourself thinking "I have a better idea" — you don't. Go read the corresponding Swift file in `.netnewswire/` and port *that*. The app is local-only, no sync backends, no WebKit, strict memory budget. Targets **GNOME 50+** and **libadwaita 1.7+** on Wayland.

Current version: **v0.3.1** (Phases 0–4 complete; Phase 5 UI skeleton in progress). See `roadmap.md` for the live phase plan and `patchnotes.md` for the shipped log.

**License:** GPLv3. **Edition:** Rust 2024.

---

## 2. Source of Truth: `.netnewswire/`

A full clone of NetNewsWire lives at `.netnewswire/` in this repo. **It is already there — do not re-clone, re-download, or `git submodule add` it.** Treat it as read-only reference material.

When you need to implement a feature, port from this local tree. Do not invent bespoke logic.

Key paths to consult:

| viaduct target | NetNewsWire source |
|---|---|
| `src/parser/xml.rs`, `src/parser/json.rs`, `src/parser/html.rs` | `.netnewswire/Modules/RSParser/` |
| `src/parser/date.rs` (`RSDateParser` port) | `.netnewswire/Modules/RSParser/RSParser/Dates/` |
| `src/database/articles.rs` | `.netnewswire/Modules/ArticlesDatabase/` and `.netnewswire/Modules/Articles/` |
| `src/database/settings.rs`, `src/database/opml.rs`, `src/database/accounts.rs` | `.netnewswire/Modules/Account/` (look for `LocalAccountDelegate`, `OPMLFile`, `FeedMetadataFile`) |
| `src/network/fetcher.rs` | `.netnewswire/Modules/RSWeb/`, plus `LocalAccountRefresher` inside `Account` |
| Coalescing / BatchUpdate primitives | `.netnewswire/Modules/RSCore/` (`CoalescingQueue`) and `DatabaseQueue` in `RSDatabase` |
| Feed discovery from a website URL | `.netnewswire/Modules/FeedFinder/` |

`.gitignore` may or may not exclude `.netnewswire/` depending on the branch; in either case, never commit changes inside it.

---

## 3. Codebase Map

### Top-level

* `Cargo.toml` — deps. Notables: `gtk4` v4.16, `libadwaita` v1.7, `tokio` (full features), `rusqlite` (bundled + FTS5), `reqwest` (rustls-tls), `quick-xml`, `serde_json`, `ammonia`, `crossbeam-channel`, `thiserror`, `chrono`, `md-5`, `tracing`. `anyhow` is present but reserved for binary glue only.
* `README.md`, `spec.md`, `roadmap.md`, `patchnotes.md` — authoritative design docs.
* `.github/workflows/ci.yml` — Ubuntu runner: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`. All three must stay green.

### `src/`

* `main.rs` — entrypoint. Initializes tracing (`EnvFilter`, `RUST_LOG` controls verbosity), calls `paths::ensure_dirs()`, launches the `adw::Application`.
* `paths.rs` — XDG resolution. Data → `$XDG_DATA_HOME/viaduct/` (fallback `~/.local/share/viaduct/`). Cache → `$XDG_CACHE_HOME/viaduct/` (fallback `~/.cache/viaduct/`). Resolves `local.opml`, `articles.sqlite`, `feed-settings.sqlite`, `favicons/`, `images/`.
* `models.rs` — domain types: `Feed`, `Folder`, `Article`, `ArticleStatus`, `Author`, `FeedSettings`, `ParsedItem`, `ParsedFeed`, `ArticleChanges { new, updated, deleted }`.
* `error.rs` — `ViaductError` (top-level) → `DatabaseError`, `NetworkError`, `ParseError`. All via `thiserror`. Each variant preserves its source error (`rusqlite`, `reqwest`, `quick_xml`, `serde_json`, `url::ParseError`).
* `bin/`, `test_glib.rs` — scratch/experimental binaries. Safe to ignore unless you're specifically asked about them.

### `src/database/`

Three stores, strict separation, single writer. Port of NNW's three-way split.

* `articles.rs` — `ArticlesDatabase`. Tables: `articles`, `statuses`, `authors`, `authorsLookup`, FTS5 virtual table `search`, delete-cascade trigger on article removal. WAL mode; pragmas `synchronous=NORMAL`, `temp_store=MEMORY`, `mmap_size`.
* `settings.rs` — `FeedSettingsDatabase`. Per-feed cache and overrides: `feedURL`, `feedID`, `homePageURL`, `iconURL`, `faviconURL`, `editedName`, `contentHash`, conditional-GET (`etag`, `lastModified`), `cacheControl` (`maxAge`, `dateCreated`), `authors` JSON, `folderRelationship` JSON, `lastCheckDate`, `readerViewAlwaysEnabled`.
* `opml.rs` — OPML on disk is the source of truth for the feed/folder hierarchy. Coalesced save (~500ms debounce), atomic temp-file + rename.
* `accounts.rs` — `LocalAccount` orchestrator owning the OPML file + both DBs.
* `worker.rs` — **single writer**: one dedicated tokio task owns both `rusqlite::Connection` handles. All writes arrive via `mpsc`. The GTK thread holds only a `Sender`; it never blocks on SQLite.

### `src/network/`

* `fetcher.rs` — `reqwest` client (rustls-tls, HTTP/2, per-host concurrency cap, UA string) + `DownloadSession` analog (coalesces duplicate in-flight URLs, maintains an errored-feed cooldown list) + `LocalAccountRefresher` port. Conditional-GET path reads ETag/Last-Modified from `FeedSettingsDatabase`; short-circuits on 304. 429 handling with exponential backoff honoring `Retry-After`. 25-hour `specialCaseCutoffDate` for high-frequency feeds.
* `cache.rs` — async favicon/image downloads, disk-cached under `$XDG_CACHE_HOME/viaduct/`.

### `src/parser/`

* `xml.rs` — RSS 2.0, Atom, OPML via `quick-xml` (zero-allocation). RSS covers the `media:*` namespace for enclosures.
* `json.rs` — JSON Feed + RSS-in-JSON via `serde_json`.
* `html.rs` — `HTMLMetadataExtractor` (finds `<link rel="alternate" type="application/rss+xml|atom+xml">` when a user pastes a bare website URL); HTML sanitization via `ammonia` (strip scripts, iframes, inline styles, trackers).
* `date.rs` — `RSDateParser` port. Permissive: W3C / ISO 8601, RFC 822 / `pubDate`, and the long tail of malformed real-world dates. Byte-level inspection for zero-alloc parsing; `chrono` for the final conversion.

### `src/ui/`

The GTK4 + libadwaita native view layer. Phase 5+ work lives here.

* `window.rs` — root `AdwApplicationWindow` and nested `AdwNavigationSplitView` scaffolding (three-pane: sidebar → timeline → article body).
* `sidebar.rs` — feeds/folders list bound to a `gio::ListModel` backed by the OPML tree. Smart Feeds pinned (Today / All Unread / Starred). Unread badges.
* `tree.rs` — `TreeController` and `TreeNode` primitives, porting the `RSTree` module from NetNewsWire. Maps domain models into `glib::Object` items for `gio::ListModel`.
* `batch.rs` — `BatchUpdate` analog to suppress UI notification storms.
* `coalescing_queue.rs` — `CoalescingQueue` analog for throttled, deduplicated UI operations on the main thread.
* `fetch_queue.rs` — `FetchRequestQueue` analog for cancelling stale timeline fetches on rapid sidebar clicks.
* `timeline.rs` — article list via `GtkListView` + `GtkSignalListItemFactory`. **Strict widget recycling.** Rendering 10,000 articles must cost the same RAM as rendering 10. Custom `gio::ListModel` backed by `ArticlesDatabase` paging.
* `article.rs` — reading pane. Sanitized HTML → native `GtkTextTag` ranges inside a `GtkTextView`. **WebKit is forbidden.**

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

**viaduct is local-only. We are not porting any remote-account or sync code.** Brandon doesn't use those services and isn't going to maintain code for them. This is a hard scope boundary, not a "we'll get to it later."

Explicitly skip the following directories/modules when porting:

* `.netnewswire/Modules/CloudKitSync/` — iCloud sync. Skip entirely.
* `.netnewswire/Modules/NewsBlur/` — NewsBlur API client. Skip.
* `.netnewswire/Modules/SyncDatabase/` — exists to support remote sync. Skip.
* `.netnewswire/Modules/Secrets/` — holds credentials for remote services. Skip.
* Anything inside `Modules/Account/` related to `FeedbinAccountDelegate`, `FeedlyAccountDelegate`, `FreshRSSAccountDelegate`, `NewsBlurAccountDelegate`, `InoreaderAccountDelegate`, `BazQuxAccountDelegate`, `TheOldReaderAccountDelegate`. Port only `LocalAccountDelegate` and the shared account scaffolding it needs.

When porting a file that mixes local-account logic with remote-sync logic, take the local branches and drop the rest. Do not leave stubs, `todo!()` placeholders, or "future sync" interfaces — cut the code out cleanly. If a generic abstraction exists solely to accommodate both local and remote accounts, collapse it to the local-only shape.

If you're unsure whether something is local-only or remote-sync, ask. Don't port "just in case."

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
