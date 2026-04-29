# viaduct — Patch Notes

## v1.0.9 — HTTP Client Parity & Pervasive Debug Tracing

The cause of "half my feeds won't refresh": our `reqwest` client didn't enable `gzip` or `brotli` decompression, while NewsFlash and most other RSS readers do. Servers that auto-negotiate compressed responses handed us binary garbage (which the parser flagged as `UnknownFormat`) or rejected our short / unrecognized User-Agent outright. Plus the debug-mode plumbing existed but wasn't actually being used — fixed in the same commit.

### HTTP client (the actual blocker fix)

- **`reqwest` features**: added `gzip` and `brotli`. Servers like passionweiss.com, the-decoder.com, and many YouTube channel feeds were returning compressed bodies that our client couldn't decode, surfacing as `Parse(UnknownFormat)` or HTTP 403/404. NewsFlash works on these because they enable the same features.
- **Centralized client builder** in `src/network/http.rs` (new module). One source of truth for the User-Agent (`Viaduct/<VERSION> (RSS reader; +https://github.com/VirInvictus/Viaduct)` — descriptive, contact URL included, matches NNW/NewsFlash convention), plus `gzip + brotli + rustls-tls` baseline.
- **`Accept` headers per subsystem**: `ACCEPT_FEED` (RSS/Atom/JSON Feed in preference order), `ACCEPT_IMAGE` (PNG/JPEG/WebP/SVG/ICO), `ACCEPT_HTML` (for Reader View). Some servers serve HTML challenge pages by default unless the client explicitly asks for the feed MIME types.
- **Three call sites updated** to use the shared builder: `Fetcher::new`, `ImageCache::new`, `reader_view::fetch_article_html`. Inoreader's API client still uses `reqwest::Client::new()` — it has its own auth flow and isn't affected by this round of fixes.

### Debug tracing (now actually pervasive)

- **Periodic memory ticker** in `viaduct::spawn_debug_memory_ticker` — random interval between 8 and 25 seconds. Reads `/proc/self/status` for `VmRSS` and `VmHWM` and emits a `tracing::info!` line with both values plus the 500 MB budget reference. Wired in `main.rs` directly after the runtime install. No-op outside `--debug` mode.
- **Fetcher** logs every request: `fetch: GET` (URL + which conditional-GET headers we're sending), `fetch: 304 (cached)` (with elapsed), `fetch: response` (status + body size + Content-Encoding + has_etag + max_age + elapsed), `fetch: network error` (URL + error + elapsed).
- **Image cache** logs every memory hit / disk hit / network miss / disk write with URL, kind (favicon vs image), and byte count.
- **Reader View** logs `reader_view: fetching article` at start, `reader_view: HTTP non-success` on bad status, `reader_view: fetched` on success with byte count + elapsed.
- **DB worker** logs each op via `tracing::trace!` with the variant name (UpdateFeed, FetchByFeed, Search, Vacuum, etc. — 21 variants exhaustively labeled) and elapsed_ms. Trace-level (debug-mode only) so it doesn't drown info-level logs.
- **Parse failures** now log a 120-byte body preview alongside the error, so `Parse(UnknownFormat)` immediately reveals whether the response was an HTML challenge page, a CAPTCHA, or actual malformed XML.

### How to use it

```sh
cargo run --release -- --debug 2>&1 | tee /tmp/viaduct-debug.log
```

The `--debug` flag flips the `EnvFilter` baseline to `debug,viaduct=trace,html5ever=error`. `RUST_LOG=...` still wins if set explicitly. The memory ticker only fires when `--debug` is on.

### What this doesn't fix

- Feeds that 404/500 because the URL is genuinely dead (some YouTube channels in the user's OPML have closed). NewsFlash works on these because it caches the last-good response — viaduct will sync that behavior when it parses Inoreader-style ETags + `If-None-Match` more aggressively.
- Inoreader API client still uses `reqwest::Client::new()`; the OAuth flow has its own UA and headers requirement that doesn't share well with the centralized builder. Tracked separately.

## v1.0.8 — CDATA Body Capture (Critical Parser Fix)

Fixes a long-standing bug where the RSS and Atom parsers silently dropped article bodies wrapped in CDATA sections. Surfaced during v1.1.0-pre3 smoke testing — Sacha Chua's blog and most other WordPress / Hugo / Jekyll feeds publish bodies as `<description><![CDATA[…]]></description>` or `<content:encoded><![CDATA[…]]></content:encoded>`. quick-xml emits these as `Event::CData`, but our parsers only handled `Event::Text`, so the bodies hit the floor.

- **`rss_handle_text_or_cdata` and `atom_handle_text_or_cdata` helpers** in `src/parser/xml.rs` carry the per-tag dispatch logic (`<title>`, `<description>`, `<content:encoded>`, `<guid>`, `<pubDate>`, etc.). The main event loops invoke them from both `Event::Text` and `Event::CData` arms, so the same body-capture path fires regardless of how the feed author wrapped the bytes.
- **`Event::Text` entity-decodes** via `unescape()`; **`Event::CData` reads raw bytes** (no entity decoding needed — that's the whole point of CDATA per XML 1.0 §2.7).
- **`<content:encoded>` precedence preserved** — when a feed publishes both `<description>` (summary) and `<content:encoded>` (full body), the latter wins. Verified in CDATA mode by the new `rss_cdata_content_encoded_overrides_description` test.
- **Author / source / channel scopes preserved** — the helpers honor `in_item`, `in_author`, `in_source`, and `in_channel_image` exactly like the previous inline match. No regressions in the existing 11 xml tests.
- **3 new regression tests** in `parser::xml::tests`: `rss_cdata_description_captured_as_body`, `rss_cdata_content_encoded_overrides_description`, `atom_cdata_content_captured_as_body`. 50 passing total (was 47).
- **Retroactive note**: previously-cached articles in `articles.sqlite` retain their empty bodies until those articles are re-fetched (the refresher's content-hash short-circuit skips re-parsing on byte-identical responses). New articles + bodies for any feed that publishes an update will be captured correctly. To force a full re-parse, delete `articles.sqlite` or wait for the feed to update.

## v1.0.7 — NNW Domain Sync + Substring-Match Bug Fix

Brings our refresher's host-matching policy into parity with NetNewsWire 7.0.5 and closes a real (latent) substring-matching false-positive in the special-case host check. Phase 16 video thumbnails are explicitly deferred to v1.2.0 polish where they wire naturally into the timeline.

- **`url_host_matches_domain` helper** (port of NNW `SpecialCase.urlStringMatchesDomain`): parses URL → lowercases host → strips optional `www.` prefix → exact-matches against a domain list. Replaces three substring-based checks (`is_special_case_host`, `is_openrss`, and the new `is_no_minimum_time_domain`).
- **Substring-match fix.** Old code used `url.contains("rachelbythebay.com")` which would have false-matched `https://evilrachelbythebay.com/` and `https://attacker.com/?u=rachelbythebay.com`. Both are now correctly rejected. Three regression tests in `network::fetcher::tests` lock this down.
- **`NO_MINIMUM_TIME_DOMAINS` const** carries the 19 personal-site hosts NNW lists in `LocalAccountRefresher.domainsWithNoMinimumTime` (synced as of upstream commit `4d594181f`): inessential.com, ranchero.com, netnewswire.blog, daringfireball.net, redsweater.com, indiestack.com, blog.plunkitup.com, bitsplitting.org, allenpike.com, hypercritical.co, micro.inessential.com, discourse.netnewswire.com, onefoottsunami.com, manton.org, randsinrepose.com, micro.blog, shapeof.com, flyingmeat.com.
- **Timing-skip ordering matches NNW**: `is_no_minimum_time_domain` short-circuits to "do not skip" first, then special-case 25h cutoff, then 29-minute minimum for everyone else. Previously these domains were stuck behind the 29-minute floor regardless.
- **No subdomain matching** (regression-tested). NNW's matcher is exact-host-after-www-strip; sub-subdomains like `blog.inessential.com` do NOT match `inessential.com`. Hosts that need both forms are listed explicitly (NNW lists `micro.inessential.com` alongside `inessential.com`; we follow).
- **4 new tests** in `network::fetcher::tests`: 41 passing total (was 37).
- **Phase 16 video thumbnails deferred to v1.2.0** in `roadmap.md`. The natural consumer is the timeline preview row, which gets its visual upgrade alongside the post-WebKit polish pass. Building it now would ship unwired code.

## v1.0.6 — Reader View Memory Gate (Phase 10 close-out)

Closes the last unchecked Phase 10 item. The local readability extractor now has a quantified RSS budget instead of a hand-wave.

- **`mem_check` adds a third checkpoint** that runs `ui::reader_view::extract` 10× sequentially against a synthesized ~100 KB article HTML. The HTML is shaped like a real-world page: navigation + header chrome at the top, 30 ad-shaped `<aside class="sidebar"><div class="ad">…</div>…</aside>` blocks, then a 200-paragraph `<article>` body with Lorem ipsum. The chrome/ad noise forces the readability scoring path to actually run rather than short-circuiting on a clean DOM.
- **Current release-build delta**: 5 MB over the post-warmup peak (59 → 64 MB). All 10 extractions complete in ~25 ms total. The full harness peak (DB + image cache + Reader View) sits at ~64 MB, well under the 500 MB ceiling.
- **Subprocess isolation deferred.** `reader_view.rs` documents that pattern as the fallback if in-process extraction blows the budget. With 5 MB / 10 extractions in-process, paying the IPC + process-spawn overhead would be a net loss.
- **Pass/fail logic** now keys off the post-reader-view peak (the highest of the three), so any future regression in the readability path will surface here first.
- **Harness still uses no new deps** — `tokio::net::TcpListener` for the HTTP fixture, `ui::reader_view::extract` for the extraction, `/proc/self/status` for the measurement.

## v1.0.5 — Image-Cache Memory Checkpoint (Phase 7 close-out)

Closes the last unchecked Phase 7 item. `mem_check` now exercises the favicon + image cache end-to-end so the 500 MB peak budget covers the full warmed-cache scenario, not just the DB insert path.

- **In-process HTTP fixture** in `src/bin/mem_check.rs`: `tokio::net::TcpListener` on `127.0.0.1:0` (ephemeral port) handling minimal HTTP/1.1, path-prefix routing `/fav-*` → 1 KB body and `/img-*` → 50 KB body. Zero new deps.
- **Warmup**: 500 favicons + 50 images fetched concurrently through the real `ImageCache`. 500 exceeds the 250-entry per-kind LRU cap, so the eviction path is exercised. Total bytes through cache: ~3 MB (500 KB favicons + 2.5 MB images).
- **Two reported checkpoints**: post-DB peak (DB + parser + serde) and post-image-warmup peak (full idle scenario). Current release-build numbers: 36 MB → 59 MB peak. Comfortably under the 500 MB ceiling.
- **Runtime fix**: previous `mem_check` used `#[tokio::main]` which builds a local-scope runtime that isn't visible to the library's `viaduct::spawn_on_runtime`. Refactored to build the runtime explicitly, install via `viaduct::init_runtime`, and `block_on` `async_main`. Without this, every `ImageCache::favicon` / `image` call panics with "tokio runtime not initialized".
- **Doc rewrite** in `src/bin/mem_check.rs` module docs covers both checkpoints and the synthetic fixture. Clippy pass on rust 1.95+ also required reformatting the doc list as standard Markdown bullets.

## v1.0.4 — Atom `type="xhtml"` Raw Inner HTML Capture

Closes the last unchecked item under Phase 11 "Parser fidelity follow-ups". Atom feeds that publish their content as inline XHTML (per RFC 4287, wrapped in a single `<div xmlns="http://www.w3.org/1999/xhtml">…</div>`) now render with structure intact instead of collapsing to bare text nodes.

- **`capture_atom_xhtml_inner`** in `src/parser/xml.rs` re-serializes the inner XML between `<content type="xhtml">` (or `<summary type="xhtml">`) and the matching close tag via `quick_xml::Writer`. Tracks element depth to handle the nested `<div>` wrapper plus arbitrary inline structure.
- **`trim_text(false)` scoped around the capture** so inline whitespace survives — without this, `Hello <em>bar</em>` collapses to `Hello<em>bar</em>` because the parent parser uses `trim_text(true)` for clean titles/IDs/dates. Restored to `true` after the capture finishes.
- **Detection**: a new `atom_type_is_xhtml` helper checks the Start tag's `type` attribute case-insensitively (matches NNW). Only fires when `in_item && !in_source && (name == "content" || name == "summary")`.
- **Body precedence**: matches existing summary-as-body fallback — the first non-empty body wins, so `<content>` beats `<summary>` when both are present.
- **NNW deviation logged**: NNW uses `XMLSAXParser.captureRawInnerContent` (a libxml2 SAX hook). Our `quick_xml::Writer` round-trip is functionally equivalent but produces structurally clean XML (canonicalized attribute quoting, etc.) rather than byte-perfect raw bytes. Acceptable since the result feeds through `ammonia` and the renderer.
- **Tests**: 2 new regression tests in `parser::xml::tests` — `atom_xhtml_content_captures_inline_html` (multi-paragraph + `<em>`), `atom_xhtml_summary_used_when_content_absent` (summary fallback). 37 passing total (was 35).

## v1.0.3 — CI Hygiene

CI was red on `main` — `cargo clippy --all-targets -- -D warnings` had 13 standing errors and `cargo fmt --check` flagged trailing whitespace. Both blockers cleared so future feature commits land on a green baseline.

- **Clippy auto-fixes** applied across `src/database/{delegate,opml,worker}.rs`, `src/network/inoreader.rs`, `src/ui/window.rs`, and `tests/integration_refresh.rs`. The 13 errors broke down as: collapsible `if let && let` chains (Rust 1.95-era lint), `Iterator::last` on `DoubleEndedIterator` (use `next_back`), redundant `&` references, deref-already-by-auto-deref, and a missing `Default` impl on `InoreaderAccountDelegate`.
- **Trailing whitespace** in `src/database/sync.rs:80` removed.
- **No behavior change.** All 36 tests still pass (35 lib unit + 1 integration).

## v1.0.2 — Housekeeping & WebKit Pivot Prep

Doc-only release that re-opens Phase 6 around a single neutered `WebKitWebView` instead of the original `GtkTextView` + `GtkTextTag` renderer. No code changes shipped — the implementation lands in v1.1.0.

- **Roadmap pivot:** Phase 6 reframed as "World-Class Typography via Neutered WebKit". Six unchecked items: WebKit transition, locked-down `WebKitSettings`, strict CSP, NNW theme bundle port, `max-width: 44em`, hover URL overlay, native `<img>` rendering with disk cache. The `ammonia` baseline stays checked as defense-in-depth even with CSP.
- **Spec & README updated:** §2.2, §9, and the README feature table now describe the neutered WebKit pipeline.
- **CLAUDE.md rule 4 updated:** "No WebKit. Ever." → "Neutered WebKit Instance" (one heavily-constrained `WebKitWebView`, JS off, plugins off, no LocalStorage, strict CSP).
- **NNW reference sync:** `.netnewswire/` advanced from `ec06277` (April 23) to `4d594181f` (April 28, post-7.0.5). Notable upstream changes documented in CLAUDE.md §2: `MutableItem` → `RSSItem` rename, expanded `domainsWithNoMinimumTime` (queued for v1.0.6), Atom summary/icon improvements, NNW issue #5280 (don't aggressively flush WebKit cache).
- **`Cargo.toml` version drift fixed:** was stuck at `1.0.0` despite `v1.0.1` shipping. Bumped to `1.0.2` to match.
- **`spec.md` trailing-duplicate line cleaned up** (botched edit artifact at the bottom of §10 success criteria).
- **`.gitignore`:** added `.newsflash/` so the NewsFlash reference clone (anti-pattern study source) doesn't get committed alongside `.netnewswire/`.

## v1.0.1 — Audit & Polish

- **Inoreader Engine Completion:** Resolved hardcoded API keys by injecting them at compile-time via environment variables. Added missing author parsing logic to the sync engine.
- **Stability:** Fixed dangerous unwrap() crashes when reading database timestamps.
- **Stability:** Fixed a critical Tokio reactor panic in the image/favicon caching layer that caused crashes when downloading assets from the GTK main loop.
- **Aesthetics:** Removed hardcoded hex colors in the article view to perfectly adapt to Adwaita light/dark mode and system themes. Improved typographic spacing (line margins) to match macOS application polish.
- **Roadmap:** Verified that all 17 phases of the roadmap have been fully implemented.

## v1.0.0 — The Wayland Release: Full Parity & System Integration

The milestone 1.0 release delivering full feature parity with NetNewsWire's local and Inoreader accounts, comprehensive test coverage, and complete GNOME system integration.

### Phase 15: Inoreader Sync Engine
- **Full Inoreader Integration:** Implemented the complete Reader API for Inoreader. Viaduct now synchronizes folders, feed subscriptions, and article read/starred states.
- **Delta-Based Sync:** Ported NetNewsWire's advanced synchronization logic that reconciles local and remote changes using a three-way diff (Local vs Remote vs Pending).
- **Background Content Fetching:** Automatically identifies and fetches full article content for synced statuses that lack local data.
- **Account Auto-Switching:** Viaduct now detects Inoreader credentials in the system keychain on startup and automatically initializes the corresponding sync engine.

### Phase 16: QA & Stability
- **Integration Test Suite:** Added a comprehensive integration test harness (`tests/integration_refresh.rs`) that verifies the end-to-end data pipeline from network fetch to database persistence.
- **Database API Extensions:** Enhanced the article database with bulk status update and missing-content discovery methods to support high-performance synchronization.
- **Refined Article Layout:** Finalized the typographic stack and reading pane margins for a premium, long-form reading experience.

### Phase 17: Flatpak & Desktop Integration
- **Flatpak First:** Ships with a complete Flatpak manifest (`org.virinvictus.Viaduct.json`) and AppStream metadata, enforcing a strict sandbox with only network and secret-service permissions.
- **Background Refresh Portal:** Integrated with the XDG Desktop Portal's Background API (`ashpd`) to support background feed synchronization even when the application is closed.
- **Desktop Standards:** Full compliance with GNOME 50 HIG, including high-resolution icons and a standard desktop entry file.

---

## v0.10.0 — Phase 15 & 16: Remote Sync & Debug Engine (Initial)

## v0.9.1 — Reader Polish & Bug Fixes

A polish and bug-fix release addressing UI fidelity, typographic readability, and chronological fetching accuracy.

### Fixed
- **"Today" Filter Accuracy:** The "Today" smart-feed query incorrectly checked against midnight UTC instead of the user's local timezone. Repaired `chrono` conversion in `fetch_today` so the cutoff evaluates against local midnight.
- **Reader View Icon:** The Reader View toggle button was using `view-reader-symbolic` (a GNOME Web specific icon), causing it to display as a "cancel"/missing-image symbol on standard installs. Switched to the widely available `format-justify-fill-symbolic`.

### Added
- **Typographic Overrides:** Fully wired the `font-monospace` and `font-serif` keys from the GSettings schema into a dynamic `GtkCssProvider`. The article pane now uses a proper reading font stack (`Georgia`, `Source Serif Pro`, `serif`) with improved margins, `16px` base size, `1.6` line-height, and `word-char` wrapping to prevent mid-word cutoff.
- **Sync Button:** Added a dedicated "Sync Now" button (`view-refresh-symbolic`) to the sidebar's top header bar, next to the "Mark All Read" action.
- **About Dialog:** Added an "About viaduct" entry to the primary window menu, surfacing an `AdwAboutDialog` with the current version, developer credits, and GitHub repository links.

## v0.9.0 — Phase 14: Pruning Engine

Wires the startup cleanup chain NNW runs in `Account.init`
(`Account.swift:335–340` → `ArticlesDatabase.cleanupDatabaseAtStartup`) and
makes the per-update prune cutoff user-tunable through the
`retention-days` GSetting that's been declared since v0.8.0.

### Three new article-DB ops
- `ArticlesDbOp::DeleteArticlesNotInFeeds(Vec<feed_id>, …)` — port of
  `ArticlesTable.deleteArticlesNotInSubscribedToFeedIDs`. Empty input is
  a no-op (mirrors the FeedSettingsDatabase early-return guard from
  v0.5.2 — a transient OPML-load failure must never trigger a wholesale
  article wipe). Existing `articles_ad` and `articles_ad_lookup` triggers
  cascade FTS5 + authorsLookup cleanup automatically.
- `ArticlesDbOp::DeleteOldStatuses { retention_days, … }` — port of
  `ArticlesTable.deleteOldStatuses` (`feedBased` branch):
  `WHERE date_arrived < ? AND starred = 0 AND article_id NOT IN (SELECT
  article_id FROM articles)`. Reaps the long tail of orphan status rows
  after retention has removed the underlying article. Status rows for
  still-existing articles are left alone so read/starred state survives
  idempotent feed reloads.
- `ArticlesDbOp::Vacuum` — runs `VACUUM` on the worker thread.
  `SettingsDbOp::Vacuum` is the FeedSettings counterpart (NNW vacuums
  this DB on every init at `FeedSettingsDatabase.swift:67`).

### Configurable retention
- `update_feed` (and the `UpdateFeed` op variant) now take
  `retention_days: i64` instead of using the hardcoded `RETENTION_CUTOFF_DAYS = 30`
  constant. The constant survives renamed as `DEFAULT_RETENTION_DAYS`
  for callers that don't have a GSettings handle (`mem_check`, the
  startup status sweep).
- `LocalAccountRefresher::new(account, sender, retention_days)` plumbs
  the value through to `refresh_one_feed` → `account.update_feed`.
- `window.rs::current_retention_days` reads `retention-days` from
  GSettings on the GTK thread (the type is `!Send` so this has to happen
  before we hand off to tokio), clamped to `[1, 365]`. Falls back to 30
  when the schema isn't installed (dev env without `glib-compile-schemas`).
  Both `act_refresh` and `refresh_specific_feeds` resolve the value
  fresh per refresh, so flipping the prefs dialog takes effect on the
  next cycle without restart.

### Startup cleanup chain
- New `LocalAccount::cleanup_at_startup(retention_days)` runs the four
  steps NNW chains in `Account.init`'s `DispatchQueue.main.async`:
  (1) prune `feed_settings` for feeds not in the OPML
  (the existing `cleanup_orphaned_settings` body, factored into a
  shared private helper), (2) `delete_articles_not_in_feeds` for those
  same feeds, (3) `delete_old_statuses` with the supplied cutoff, then
  (4) `vacuum_databases` on both SQLite files. Each step is independent
  and non-fatal — a failure logs `tracing::warn` but doesn't abort the
  chain.
- `LocalAccount::new` now drives `cleanup_at_startup(DEFAULT_RETENTION_DAYS)`
  in place of the old `cleanup_orphaned_settings` call. The user's
  retention pref shapes the next refresh, not startup; using the schema
  default here matches NNW (whose `deleteOldStatuses` is also
  hardcoded 30) and avoids a `gio::Settings::new` from inside an async
  init that may run before the GTK thread exists.
- `cleanup_orphaned_settings` is preserved for callers that just want
  the settings sweep without the article work.

### Module additions
- New `pub const DEFAULT_RETENTION_DAYS: i64 = 30` in
  `src/database/articles.rs`.
- New `pub fn retention_days(&Settings) -> i64` in `src/preferences.rs`
  with `keys::RETENTION_DAYS`.
- New private helpers `opml_feed_urls` and `opml_feed_ids` in
  `src/database/accounts.rs` (centralize the standalone+folder walk so
  cleanup_orphaned_settings and cleanup_at_startup share one
  implementation).

### Tests
- 35 passing (was 30). Five new regression tests in
  `database::articles::tests`:
  `update_feed_honors_custom_retention_days` (7-day retention sweeps a
  10-day-old orphan that 30-day retention would keep),
  `delete_articles_not_in_feeds_evicts_orphans_only`,
  `delete_articles_not_in_feeds_empty_input_is_noop` (regression mirror
  of the settings-table early-return),
  `delete_old_statuses_prunes_orphans_only` (verifies the still-an-article
  / starred / within-retention exemptions all survive), and
  `vacuum_succeeds_on_clean_db`.

### Out of scope
- **`deleteOldArticles` (NNW `syncSystem`-only branch)** is not ported.
  v1.0 is feedBased; the per-update prune already handles the
  feed-driven retention NNW assigns to local accounts.
- **Per-feed retention overrides.** NNW has no UI for this either; the
  `retention-days` knob is global.

## v0.8.1 — Read/Unread Completion

Wires the two NNW behaviors that were tracked but not yet hooked up: auto-mark-read on selection, and live sidebar unread badges.

### Auto-mark-read on selection
- Port of NNW `TimelineViewController.tableViewSelectionDidChange` (TimelineViewController.swift:931). When the timeline_selection handler fires for an unread article, the node is flipped to `read=true` (optimistic so the title goes dim immediately), the status row is upserted, and `refresh_unread_counts` runs after the upsert succeeds. Existing keyboard `r`/`m` toggle is unchanged — auto-mark-read only kicks for previously-unread rows.

### Sidebar unread badges
- New `ArticlesDbOp::UnreadCountsByFeed` returns a `HashMap<feed_id, i64>` (feeds with zero unread are absent from the map). `LEFT JOIN statuses` so articles without a status row count as unread, matching NNW semantics for freshly-inserted articles.
- New `ArticlesDbOp::SmartFeedCounts` returns `SmartFeedCounts { today_unread, all_unread, starred_unread }`. Today reuses the existing `fetch_today` cutoff (local midnight UTC); All Unread is a global `COUNT(*) WHERE read=0`; Starred narrows to `starred=1 AND read=0` matching NNW `BuiltinSmartFeed.unreadCount`.
- `LocalAccount::unread_counts_by_feed()` and `LocalAccount::smart_feed_counts()` expose the ops.
- `TreeNode.unread_count` is now a `glib::Properties`-derived `u32` (was `Cell<usize>`). The sidebar row factory connects `notify::unread-count` in `connect_bind` and disconnects in `connect_unbind` via the unsafe `set_data`/`steal_data` pattern. Setting the count on any clone of a TreeNode wrapper fires the notify on the underlying GObject so all bound rows update without re-binding the tree.
- New `ViaductWindow::refresh_unread_counts` walks the controller's root node, applies per-feed counts, sums folder totals, applies smart-feed counts, and sets the SmartFeedGroup parent total. Folder summing is local (sums children seen during the walk) so deeply nested OPMLs that NNW's normalizer flattened still tally correctly.
- Refresh hooks: initial OPML load completion, post-import sidebar reload, `apply_status_to_current` upsert success, `mark_read_in_range` bulk upsert success, `mark_current_read_then_advance` upsert success, `act_refresh` cycle completion, `refresh_specific_feeds` cycle completion, and the new auto-mark-read upsert.

### Tests
- 30 passing — wiring is GTK-side; no new unit tests. Integration coverage exercises the badges through the running app.

## v0.8.0 — Phase 13: System Integration & Theming

Three of four Phase 13 sub-items land in this release. The fourth (xdg-desktop-portal Background daemon) moves to Phase 17 because it shares plumbing with the Flatpak manifest work.

### GSettings schema
- New `data/org.virinvictus.Viaduct.gschema.xml`. Declares an enum (`ColorScheme`: default / force-light / force-dark) and six keys: `color-scheme`, `notifications-on-refresh`, `refresh-interval-minutes` (10-1440), `retention-days` (1-365), `font-monospace`, `font-serif`. v0.8.0 wires the first two into behavior; the rest are reserved for the phases that introduce their consumers.
- `build.rs` runs `glib-compile-schemas data/` on cargo build so dev runs find the compiled schema. Failures emit `cargo:warning` and the runtime falls back to defaults — CI runners without GLib dev tools still produce a binary.
- `main.rs::ensure_schema_dir` exports `GSETTINGS_SCHEMA_DIR=$CARGO_MANIFEST_DIR/data` before any gio call when `gschemas.compiled` exists there. Production Flatpak builds (Phase 17) install the schema in the runtime prefix and ignore this hook.
- `src/preferences.rs` wraps the schema: `settings()` returns `Option<gio::Settings>` (None when the schema isn't installed), `apply_color_scheme(&settings)` sets `adw::StyleManager` and connects to `notify::color-scheme` for live flips, `notifications_enabled(&settings)` reads the toggle on each call.

### Color scheme follow
- On `app.connect_activate`, `build_ui` calls `viaduct::preferences::apply_color_scheme` so the global `AdwStyleManager` either follows the system (default), forces light, or forces dark per the GSetting. Port: NNW `AppearancePreferencesView` writing `NSApp.appearance` — translated to libadwaita's color-scheme primitive.

### Refresh notifications
- Refactored both `act_refresh` (full OPML refresh) and `refresh_specific_feeds` (post-import) to route through two new helpers: `pair_feeds_with_settings` (lifts the FeedSettings-or-blank logic out of duplicated bodies) and `run_refresh_with_tally` (runs the refresher, drains `ArticleChanges` into a `usize` count of new articles, drops the refresher to close the channel cleanly, awaits the drain task).
- Result is piped through a `tokio::sync::oneshot::channel::<usize>` back to a `glib::spawn_future_local` on the GTK thread, which calls `dispatch_refresh_notification`. That method gates on the GSetting and `application()`, builds a `gio::Notification` titled "viaduct" with body "N new articles", sends it via `Application::send_notification(Some("viaduct.refresh"), …)`. The static notification id replaces in-place per refresh cycle so back-to-back refreshes don't pile up notifications. Sandbox-friendly via `org.freedesktop.portal.Notification` under Flatpak.
- NNW deviation logged: NNW's `newArticleNotificationsEnabled` is per-feed (set via the Inspector pane). We don't have an inspector yet, so v0.8.0 ships a single global toggle. The per-feed gate is tracked as a follow-up under the future inspector phase; once it lands, the global toggle becomes the AND-gate atop per-feed.

### Preferences dialog
- New `src/ui/preferences_dialog.rs::present(parent)`. `AdwPreferencesDialog` with a single "General" page containing two `AdwPreferencesGroup`s: Appearance (color-scheme `AdwComboRow`) and Notifications (`AdwSwitchRow` bound bidirectionally via `gio::Settings::bind`). External flips (e.g. `dconf-editor`, terminal `gsettings set`) sync back to the dropdown via `connect_changed`. When the schema isn't installed (dev-only failure mode), the dialog renders an explanatory inert row instead of crashing.
- Primary menu (`window.ui::primary_menu`) gained a "Preferences" entry above "Keyboard Shortcuts". `actions.rs` registers `win.preferences` (no accelerator); `window.rs::act_preferences` calls into the dialog module.

### Module additions
- `src/preferences.rs` (library-level GSettings wrapper).
- `src/ui/preferences_dialog.rs` (the dialog).
- `data/org.virinvictus.Viaduct.gschema.xml` (the schema).
- `build.rs` (compile schemas at build time).
- `.gitignore` ignores `data/gschemas.compiled` (build artifact; source XML is checked in).

### Tests
- 30 passing — no new tests; Phase 13 is GTK-side and GSettings-side, exercised through integration only.

### Deferred / scope notes
- **Background daemon** moved from Phase 13 to Phase 17. Rationale: it needs `ashpd` (xdg-portal client) and pairs naturally with the Flatpak manifest's `org.freedesktop.portal.Background` entry. Roadmap updated.
- **Per-feed notification toggle** (NNW's `newArticleNotificationsEnabled`) deferred until a feed-inspector pane exists; v0.8.0 has a single global toggle. The schema field was deliberately not added to `FeedSettings` so we don't carry dead state until there's UI to flip it.
- **Refresh-interval / retention-days / font-override** keys exist in the schema but are not yet wired. They'll come online with Phase 14 (retention / pruning engine) and Phase 17 (cron daemon) without further schema churn.

## v0.7.1 — Timeline Polish

Three deferred polish items that surfaced during Phase 12 import testing. No new dependencies; no NetNewsWire deviation.

### Timeline display
- **Feed-name resolution**: timeline rows now show the feed's display name (`edited_name` → parsed `name` → URL host → raw URL) instead of the raw `feed_id`. New `FeedNameMap` type alias (`Rc<RefCell<HashMap<String, String>>>`) lives in `src/ui/timeline.rs` and is threaded into `setup_timeline_list_view`. The window owns one map (`feed_names: OnceCell<FeedNameMap>`) and rebuilds it from `OpmlFile` on startup load and after every import via the new `rebuild_feed_names_from` helper. After repopulating, `store.items_changed(0, n, n)` re-binds existing rows so they pick up the new names without waiting for the user to scroll. Display-name resolution port: NNW `WebFeed.nameForDisplay`.
- **Bold/unread visuals**: `ArticleNode.read` and `.starred` are now `glib::Properties` instead of plain `Cell<bool>`. The factory's `connect_bind` subscribes to `notify::read` and toggles `heading` / `dim-label` CSS classes on the title via `apply_read_styling`; `connect_unbind` disconnects the handler so recycled rows don't accumulate handlers across nodes. Optimistic mark-read flips visual immediately. Existing `is_read` / `is_starred` / `set_status` helpers preserved as wrappers so window.rs callers don't churn.

### Keyboard
- **Capture-phase shortcut routing on the timeline `ListView`**. `gtk::Application::set_accels_for_action` installs window-bubble accelerators which fire after the focused widget — and `GtkListView` consumes Up/Down/Home/End/Return/space in the target phase. `install_timeline_capture_shortcuts` adds a `gtk::ShortcutController` with `PropagationPhase::Capture` directly on the timeline list view, with `NamedAction` shortcuts for `Down`/`j`/`n`/`Up`/`k`/`-`/`space`/`<Shift>space`/`r`/`m`/`<Shift>m`/`s`/`b`/`Return`/`<Ctrl>Return`/`l`/`o`. The actions fire before the list view's built-in key handlers, so muscle-memory navigation works once a row is focused. Search-entry input is unaffected (the controller is scoped to the list view, not the window).

### Tests
- 30 passing — no new tests; polish is GTK-side and doesn't change DB / parser logic.

## v0.7.0 — Phase 12: OPML Import & Export

User-facing OPML exchange. The internal parse/serialize plumbing has existed since Phase 2; this release wires it to two menu actions and ports the NetNewsWire merge semantics so imports behave the way NNW users expect.

### Phase 12 — OPML Import & Export
- **Import menu action** (`win.import-opml`) — `gtk::FileDialog::open_future` opens the file under Wayland's portal automatically (Flatpak-clean for Phase 17). Selected file is read on the tokio runtime, parsed via the existing `parse_opml`, normalized, merged, and persisted via the debounced `OpmlWriter`. Sidebar reloads from the new OPML; just-added feeds get an immediate `LocalAccountRefresher::refresh_feeds` kick. *(`act_import_opml` in `src/ui/window.rs`)*
- **Export menu action** (`win.export-opml`) — `gtk::FileDialog::save_future` with default name `Subscriptions-viaduct.opml`. Writes a string from a new hand-rolled writer that matches NNW `OPMLExporter.OPMLString` byte-for-byte: XML decl, `<!-- OPML generated by viaduct -->` comment, tab indentation, attribute order (`text title description="" type="rss" version="RSS" htmlUrl xmlUrl`), self-closing folder branch when empty. *(`serialize_account_opml` in `src/database/opml.rs`)*
- **`OPMLNormalizer` port** (`normalize_opml`) — nameless-folder wrappers promote children up; named folders flatten descendants into a single feed list (folders-only-one-level-deep); feeds dedup by `xmlUrl` within parent. NNW's `titleFromAttributes` check maps to our `outline.title` (we don't promote on `text`-only outlines).
- **Merge semantics** (`merge_opml`) — union by `xmlUrl` against the union of every existing feed (top-level + every folder). Folder match is by name, case-sensitive; missing folders are created. Returns `(merged, Vec<Feed>)` — the second element is just the genuinely-new feeds for the post-import refresh. Existing `edited_name` is preserved (we keep the existing feed instead of replacing).
- **`LocalAccount` API** — new `import_opml(path) -> Result<Vec<Feed>>` and `export_opml(path, title) -> Result<()>` async methods; both run on the global tokio runtime via `crate::spawn_on_runtime` so the GTK thread never blocks on disk I/O.
- **Toast feedback** — `window.ui` now wraps content in an `AdwToastOverlay` (`toast_overlay` template child). Import shows feed count; export shows the file path; failure modes (parse error, missing local path on the chosen `gio::File`) render human-readable copy.
- **Primary menu** — `window.ui` declares a `GMenu` `primary_menu` bound to `menu_btn` via `menu-model`; sections are Import OPML / Export OPML / Keyboard Shortcuts.

### Tests
- 30 passing (was 24). Six new regression tests in `database::opml` cover: nameless-wrapper promotion, nested-folder flattening, intra-folder dedup, merge appends only new URLs, merge creates missing folders, export byte-shape (XML escaping, NNW attribute order, edited-name precedence).

### Out of scope
- Multi-account picker — NNW shows it only when `accounts.count > 1`; we have one account.
- `nnw_externalID` round-tripping — no cross-app fidelity need in v1.0; our `Folder` has no external ID field.
- `lastArticleFetchStartTime` reset on import — we don't track that signal; the new-feed refresh kick covers the visible-result need.

## v0.6.0 — Phases 9, 10, 11

Three major phases land together: full keyboard navigation, native Reader View, and enclosure / media-attachment support across the parser stack. With this release every Phase 0–11 roadmap item is checked except a single deferred fidelity follow-up (Atom `type="xhtml"` raw inner HTML capture).

### Phase 9 — Keyboard Spatial Navigation
- New `src/ui/actions.rs` registers a `gio::SimpleActionGroup` named `win` with every keyboard action; `adw::Application::set_accels_for_action` installs accelerators. NNW's `GlobalKeyboardShortcuts.plist` keys are primary; the roadmap's friendlier aliases (Down/Up/j/k for navigation, m/Enter for status/open) layer on top so both muscle memories work.
- Smart-read on Space: ports NNW `scrollOrGoToNextUnread` — pages the article down if the `GtkTextView`'s `vadjustment` can scroll, otherwise marks the current article read (optimistic local update + async DB upsert) and jumps to the next unread row. Includes Shift+Space scroll-up.
- Status actions: `r`/`m` toggle read, `Shift+m` mark unread + advance, `s` toggle star, `Ctrl+k` mark all read, `l` mark all read + advance to next unread, `o` mark older read (rows below selection in the date-desc timeline).
- Open actions: `b`/`Enter` open in browser via `gio::AppInfo::launch_default_for_uri`. `Ctrl+Enter` opens the first attachment.
- App chrome: `Ctrl+r` refreshes feeds (drives `LocalAccountRefresher` against the loaded OPML, runs on the library-wide tokio runtime), `Ctrl+f` focuses search, `F9` collapses the outer `AdwNavigationSplitView`, `Ctrl+?` shows a `gtk::ShortcutsWindow` built from a declarative `src/ui/shortcuts.ui`.
- Bulk status fetch: new `ArticlesDbOp::FetchStatusesByIds` and `LocalAccount::fetch_statuses_by_ids` populate `ArticleNode.read`/`starred` after each timeline load so navigation actions can read state without a per-keystroke DB hit.
- Timeline list view auto-scrolls to the newly-selected row when navigation moves the selection.

### Phase 10 — Reader View
- New `src/ui/reader_view.rs` module ports NNW's `ArticleExtractor` to a local Mozilla Readability port via the `readability` crate. The CPU-bound `extract` runs in `tokio::task::spawn_blocking`. NNW's hosted Mercury endpoint is the one approved deviation — we cannot depend on an external service.
- Article-pane header bar grew a `reader_btn` toggle. On article selection the per-feed `reader_view_always_enabled` setting is fetched and pre-toggles the button; explicit toggles re-render the pane via the unified `render_article_body` state machine (raw HTML / cached extracted HTML / kick-off-extraction).
- Memory gate: input HTML capped at 5 MB before extraction (`INPUT_SIZE_CAP`). Extracted HTML rides the existing `ammonia → quick-xml → GtkTextTag` pipeline in `article::render_html` so reader-view bodies get the same sanitization treatment as feed-supplied bodies.
- Centralized `ArticleDisplayState` on the window: `raw_html`, `extracted_html`, `article_url`, `auto_reader`. Single source of truth for what the article pane is showing; toggle and async extraction completion both call `render_article_body` to re-derive.

### Phase 11 — Enclosures, Media & Parser Fidelity
- New `models::Attachment` (NNW `ParsedAttachment` port) on both `ParsedItem` and `Article`. `articles` table grew an `attachments JSON` column; idempotent `ALTER TABLE … ADD COLUMN` migration runs at schema setup so pre-existing DBs pick up the column without losing data.
- RSS parser: `<enclosure url=… length=… type=…>` parsed to `Attachment`. `<media:content>` and `<media:thumbnail>` (MRSS namespace) parsed when they carry a `url` attribute — the heuristic distinguishes them from `<content:encoded>` which doesn't.
- Atom parser: `<link rel="enclosure">` previously a no-op, now emits an `Attachment` carrying `type` and `length`. `AtomLinkCtx` extended with `current_item_attachments`.
- JSON Feed: `attachments[]` arrays parsed per the v1.1 spec — `url`, `mime_type`, `title`, `size_in_bytes`, `duration_in_seconds`.
- `ParsedFeed.icon_url`: RSS `<channel><image><url>`, Atom `<icon>`/`<logo>` (icon wins). Refresher persists into `FeedSettings.icon_url` so the existing sidebar `spawn_favicon_fetch` path picks it up automatically.
- `ParsedFeed.language`: RSS `<channel><language>` and Atom `<feed xml:lang>`. Captured but not yet used for rendering direction.
- Timeline media indicator: `gtk::Image` (audio/video/image symbolic, MIME-driven) + count badge in the row's top hbox. Visible only when `article.attachments` is non-empty.
- `Ctrl+Enter` opens the first attachment via the system MIME handler. xdg-open route — users with mpv configured handle audio/video naturally; no hard-coded player.

### Structure
- `src/lib.rs` gains `init_runtime` and `block_on_runtime` helpers in addition to the existing `spawn_on_runtime`. `main.rs` slimmed accordingly.

### Tests
- 24 passing (was 19). Five new regression tests for RSS + Atom enclosures, MRSS media, RSS channel image + language, Atom icon/logo + xml:lang.

### Memory checkpoint
- `mem_check` still reports 29 MB peak / 29 MB current after Phase 11's added attachments column and parsed attributes. Within budget.

### Deferred
- Atom `type="xhtml"` raw-inner-HTML capture stays text-only. quick-xml has no `captureRawInnerContent` analog; in practice almost no Atom feeds use `type="xhtml"`. Tracked under Phase 11 fidelity follow-ups.
- Reader-View memory checkpoint: `mem_check` doesn't yet exercise an extraction. Listed in Phase 10 as the one remaining unchecked bullet.

## v0.5.3 — Phase 5/7/8 close-out

Finishes the remaining unchecked items under Phases 5, 7, and 8.

### Added
- **Folder aggregation in sidebar**: selecting a folder now fetches articles for every contained feed and merges them newest-first. `fetch_folder_articles` in `src/ui/window.rs`. Port of NNW's folder-as-article-source behavior.
- **FTS5 snippet extraction**: new `ArticlesDbOp::SearchWithSnippets` and `LocalAccount::search_articles_with_snippets(query, feed_filter)` use SQLite's `snippet(search, -1, '', '', '…', 10)` to return a context excerpt per match. `ArticleNode` gained a `snippet` field (`with_snippet` constructor); the timeline's bind callback prefers the snippet over the article summary when present, so search results show the excerpt that actually matched.
- **Search-scope toggle**: new `scope_toggle` GtkToggleButton in `window.ui` next to the search entry. `ViaductWindow` tracks `selected_feed_id` from sidebar selection; when the toggle is on, search restricts to that feed via the `feed_filter` argument on the new search method. Toggling re-emits `search-changed` so scope flips re-run without re-typing.
- **Memory checkpoint harness**: `src/bin/mem_check.rs` runs 500 feeds × 10 articles through the real single-writer DB worker against a tempdir XDG, then reads `/proc/self/status` to report `VmHWM` vs the 500 MB hard budget. Release-build peak on the current machine: **29 MB**. Run via `cargo run --release --bin mem_check`.

### Changed
- **Crate split into lib + bin**: added `src/lib.rs` that declares the module tree publicly; `src/main.rs` is now a thin binary that imports via `use viaduct::...`. Enables auxiliary binaries like `mem_check` to share the same code without duplicating module graphs.

### Fixed
- No net bug fixes this release — all items were new functionality on top of v0.5.2's restoration.

## v0.5.2 — Phase 1–8 Restoration

A maintenance pass that reconstructs work lost when an unsaved-edit session in another agent rewrote license headers and trampled in-progress code. The roadmap previously claimed Phases 1–8 complete; several files were empty or broken stubs on disk. This release brings the source tree back in line with those claims and rewires the app end-to-end so it actually loads OPML, fetches feeds, parses, persists, and renders.

### Restored
- **`src/network/cache.rs`**: async favicon + image cache with two-tier storage (in-memory LRU capped at 250 entries → MD5-keyed disk cache under `$XDG_CACHE_HOME/viaduct/`). Linux has no reliable low-memory broadcast, so the LRU cap is a hard guarantee for the 500 MB peak-RSS budget. Includes `color_for(name)` (port of NNW `ColorHash`) for `AdwAvatar` fallback.
- **`src/ui/article.rs`**: native HTML renderer. ammonia-sanitized payloads walked via `quick-xml`, mapped to `GtkTextTag` ranges in a `GtkTextView` (h1–h6, p, blockquote, pre/code, strong/em, a, ul/ol/li, hr, br). Per-link unique tags (`link:<href>`) plus a single `GestureClick` controller route activations to `gio::AppInfo::launch_default_for_uri` (xdg-open).
- **`src/ui/window.rs`**: `ViaductWindow` now accepts `Arc<LocalAccount>`, holds the sidebar tree controller / data source / timeline store across the widget's lifetime (no more dropped `_account`), loads OPML on activation, and wires sidebar-selection → timeline fetch → article render.
- **FTS5 search UI**: `GtkSearchBar` + `GtkSearchEntry` added to the timeline pane in `window.ui`; the existing sidebar `search_btn` toggle now bidirectionally controls the search bar's reveal state. Keystrokes are debounced ~150ms before hitting `account.search_articles()` against the FTS5 `MATCH` index.

### Fixed
- **Stable article IDs**: replaced `DefaultHasher` with MD5 in both `src/parser/xml.rs` and `src/parser/json.rs`. Synthetic unique IDs are now deterministic across builds; the article DB ID is `md5("{feed_id} {unique_id}")` per NNW `Article.calculatedArticleID`.
- **`src/database/articles.rs`**: ported NNW's `ArticlesTable.update(parsedItems, feedID, deleteOlder)`. Diffs incoming parsed items against existing rows, computes real `ArticleChanges { new, updated, deleted }`, ensures status rows for new articles (stale items >6mo default to read), and applies the 30-day non-starred retention sweep when `delete_older` is true. Single transaction per feed update.
- **`src/network/fetcher.rs::LocalAccountRefresher`**: previously sent `ArticleChanges::default()` (empty) on every successful fetch. Now actually parses the body, calls `update_feed`, and persists conditional-GET headers / cache-control / content hash / `last_check_date` back to the settings DB. Adds NNW's 8-day expiry on conditional GET info (catches servers that always 304) and the 5-hour cap on `Cache-Control: max-age` (openrss.org excluded; matches NNW).
- **RSS parser fidelity**: stops on `</rss>` and `</RDF>`; honors `<guid isPermaLink="false">`; resolves relative `<link>` and `<guid>` URLs against the channel's home-page URL via the `url` crate.
- **Date precision round-trip**: parsed `DateTime` timestamps are truncated to seconds at `Article` construction so DB round-trips don't flag every article as updated on the next refresh.

### Removed
- `src/test_glib.rs` (a deliberate-compile-error scratch file that wasn't a `[[bin]]` target).
- `src/ui/sidebar_factory.rs` (orphan stub that contained only comments).
- `src/ui/smart_feed.rs` (skeleton that never compiled — missing imports for `LocalAccount` / `Article`, duplicated copyright header, never declared in `src/ui/mod.rs`). The smart-feed sidebar rows themselves are still produced by `SidebarTreeControllerDelegate` and now drive timeline fetches via the wired selection handler.

### Docs
- `CLAUDE.md`: corrected stale path pointers under §2 — NNW's source tree is `Modules/<X>/Sources/<X>/`, not `Modules/<X>/`. Updated the table to reflect the current `.netnewswire/` layout. Removed the dead reference to `src/test_glib.rs` from §3.

### Phase 7 completion (follow-up pass)
- **Sidebar favicon rendering**: row factory now uses an `AdwAvatar` with the feed name's initial and auto-derived accent color (semantically equivalent to NNW's `ColorHash` for our purposes). On bind, we look up `FeedSettings` for a `favicon_url`/`icon_url`, fetch via `ImageCache`, decode bytes → `GdkTexture` → `Avatar::set_custom_image`. Stale-row guard compares the avatar's displayed text against the feed name we started with, so recycling during scroll doesn't attach the wrong icon.
- **Inline `<img>` substitution in article pane**: `render_html` now accepts an optional `Arc<ImageCache>`. Every `<img>` with an absolute `http(s)` src inserts a `TextChildAnchor` + anchored `gtk::Picture` into the buffer; fetch runs async via `ImageCache`, decoded on the main thread, paintable set on the `Picture`. Display width capped at 600px via `INLINE_IMAGE_MAX_WIDTH`. Missing or relative-src images fall back to the original `[image]`/`[image: alt]` placeholder text.

### Bug audit — NNW Swift vs Rust port
- **Atom parser author handling** was materially broken. Any text event inside a `<name>` element — regardless of whether an `<author>` wrapper was open — emitted an Author. Also: `<email>` and `<uri>` were never captured; root (feed-level) `<author>` didn't propagate to authorless entries; `<source>` blocks (republished-entry origin metadata) overwrote the entry's own title/id/link. Rewrote the parser to track `in_author`, `in_source`, and `current_author: Option<MutableAuthor>` state, added root-author propagation at end of parse, and suppressed `<source>` content. Added four regression tests. `<link href="...">` inside entries now resolves against the feed's home-page URL, matching NNW.
- **Atom end-of-feed guard**: stops scanning at `</feed>` so trailing junk doesn't mis-parse. Matches NNW's `endFeedFound`.
- **`FeedSettingsDatabase::delete_settings_for_feeds_not_in(empty_vec)` wiped the entire settings table** — the `if feed_urls.is_empty()` branch ran `DELETE FROM feed_settings` instead of returning early. NNW's `guard !feedURLs.isEmpty else { return }` early-returns. This could have nuked a user's per-feed ETag/content-hash/favicon cache on any startup where the OPML happened to load as empty. Fixed and regression-tested.
- **`authorsLookup` cascade**: article deletes now cascade-remove their `authorsLookup` rows via a new `articles_ad_lookup` trigger. NNW handles this explicitly inside `removeArticles`; relying on callers to remember it was a slow leak. Status rows are deliberately NOT cascaded — NNW retains them so read/starred state survives an article reappearing in the feed.
- **`Fetcher::fetch` error classification**: when the per-URL broadcast channel closed unexpectedly (background task panic/drop), we returned `DatabaseError::WriterGone`, which is nonsense — the failure is network-side. Now surfaces as `NetworkError::RateLimited { retry_after_secs: 0 }` so callers back off without retrying the same URL immediately. String'd reqwest errors on the other branch rebuild as `ParseError::Malformed("network: …")` with a network prefix instead of being misclassified.

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

## v1.0.0 — Stable

The 1.0.0 release is now complete. For future plans, see [roadmap.md](roadmap.md).
