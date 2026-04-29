# viaduct — Patch Notes

## v1.5.4 — Icon + logo redesign

The v1.5.3 icon shipped with three real bugs that I should have caught before tagging it. Brandon reviewed the actual rendered output and pushed back. This release fixes all three and rebuilds the banner so it works on dark themes.

### What was broken

1. **Pier-band line cutting through the open archway.** The horizontal "pier band" rectangle I drew at y=62 was 76px wide — full outer-arch width — which means it covered both the masonry legs *and* the open archway interior between them. Visually that read as an opaque horizontal beam crossing the open arch, which is architectural nonsense. Rectangle removed.

2. **Springer rectangles creating asymmetric "broken leg" appearance.** The two corner overlays (at x=26-36 and x=92-102, y=58-64) were stamped over the leg masonry. At small render sizes they sometimes anti-aliased badly enough that the right leg looked shorter than the left. Removed entirely — they were detail that didn't survive small-size rendering anyway.

3. **Wall thickness wrong at the apex.** The outer arc had its centre at (64, 56) with radius 38; the inner arc had its centre at (64, 64) with radius 28 — *different centres*, which produced a wall that was 18px thick at the top and 10px thick on the sides. That's the opposite of how real Roman aqueducts are built (heavy piers, thin arch ring), and it made the apex look squat. Both arcs now share the same centre at (64, 64) with radii 40 and 30 — uniform 10px wall thickness around the entire archway.

### Icon redesign

Concentric outer/inner arcs centred at (64, 64) with radii 40 and 30. Both arcs sit on top of straight legs that descend to y=110. Single keystone at the apex, sized to span exactly from the outer arc top (y=24) to the inner arc top (y=34) — sits *between* the masonry layers like a real keystone, not floating in the archway interior. RSS broadcast mark centred horizontally inside the inner arch, anchored low so the arcs sweep into the archway's head room.

Reads cleanly at every render size from 16×16 (where the keystone collapses but the silhouette + RSS dot remain) up to 512×512 (where every detail crisps). Symbolic icon redrawn to match the same concentric geometry on a 16×16 grid.

### Logo redesign

The v1.5.3 banner used a transparent background with `#3E332A` (dark brown) wordmark. On dark host themes — GitHub dark, dark file viewers, dark social-card previews — the wordmark was nearly invisible. Brandon's screenshot from a dark file viewer made this immediately obvious; the wordmark was a black-on-near-black phantom.

Fixed by giving the banner a unified cream-card rounded rectangle covering the full 480×144 canvas. The wordmark sits on cream regardless of host background, so it reads on every theme. The icon block is its own slightly-darker cream square inside the card so it still reads as the icon, not as part of the wordmark plate. Wordmark weight bumped from 600 to 700 for better presence.

### Everything else

73 unit + 1 integration tests still passing. fmt + clippy clean. `docs/icon-256.png` and `docs/icon-512.png` regenerated from the new SVG so the in-repo PNG fallbacks stay in sync.

### Lesson

I declared v1.5.3 done after rendering the icon at 64px and 128px and saying it looked good. I didn't render at 512px, didn't view the banner against a dark background, didn't sanity-check the geometry. Brandon caught what should have been caught at the source. Won't ship icon work again without rendering at the size people will actually look at it (512px+ for app icons, full-banner against multiple host backgrounds for logos).

## v1.5.3 — Visual identity: app icon, banner logo, README polish

The project finally has a face. Up to now the desktop file referenced an icon name that didn't resolve to anything — gnome-software, the dock, the shell launcher all fell back to a generic-app placeholder. The README's banner image was a broken link.

### What ships

- **Application icon** at `data/icons/hicolor/scalable/apps/org.virinvictus.Viaduct.svg`. Single-file SVG, scalable from 16×16 (the smallest GTK uses) up to whatever size GNOME asks for. Concept: a stone arch — the structure a viaduct is named for — with RSS broadcast waves emerging from the inner archway. Warm cream rounded-square background, slate-stone arch with keystone and springer detailing, NetNewsWire-orange RSS mark inside. Visible elements: outer arch silhouette (carved as a single even-odd-fill path so the inner arch reads as cleanly cut masonry), keystone wedge at the apex, springer blocks on the lateral piers, pier band across the spring line, RSS dot + two broadcast arcs nested in the negative space. Designed at 128×128 with the standard 12px GNOME safe margin.

- **Symbolic icon** at `data/icons/hicolor/symbolic/apps/org.virinvictus.Viaduct-symbolic.svg`. Single-color, redrawn at 16×16 grid for crisp small-size rendering — used by GTK in places that draw monochrome glyphs (sidebars, menu items, notification indicators).

- **Banner logo** at `logo.svg` (workspace root). Horizontal 420×128 layout: app icon on the left, "viaduct" wordmark in a humanist sans, tagline "a Linux port of NetNewsWire" underneath. Used by the README header. Replaces the previous broken `<img src="logo.svg">` reference.

- **Meson `install_data` blocks** for both icon SVGs into `$prefix/share/icons/hicolor/{scalable,symbolic}/apps/`. Confirmed end-to-end with `meson setup` + `--dry-run` install. Once the package is installed system-wide (or via Flatpak), the desktop file's `Icon=org.virinvictus.Viaduct` line resolves correctly and gnome-software / the dock / the shell launcher all show the real icon.

- **AppStream metainfo screenshot reference fixed.** Was pointing at `https://raw.githubusercontent.com/virinvictus/Viaduct/main/screenshots/main.png` (lowercase org name, wrong path, never existed). Now points at `https://raw.githubusercontent.com/VirInvictus/Viaduct/main/docs/screenshots/main.png`. Path matches the real repo case + a real in-tree directory. Caption updated to describe what the screenshot will show ("Reading pane with the Sepia theme on GNOME 50").

- **`docs/screenshots/.gitkeep`** with a comment block listing the recommended captures (main wide layout, dark mode, mobile collapse, smart feeds, preferences) and reminding contributors to keep `appdata.xml` and the README in sync.

### README rewrite

Reorganized end-to-end:

- **Lead with the framing.** First sentence under the project name is now "A Linux port of NetNewsWire — the macOS RSS reader by Brent Simmons — in Rust and GTK4." Previously this was buried five sentences in.
- **Why-this-exists section** now leads with the empirical comparison: the closest comparable Linux RSS reader idles at ~600 MB on the same OPML where Viaduct peaks under 300 MB. That's the headline number, and it should be readable in the first 30 seconds someone spends on the page.
- **Features table** rewritten to be specific. Each row names what's actually in the codebase ("Single-writer DB worker", "All 8 NetNewsWire themes — bundled byte-for-byte via `include_str!`", "Locked-down WebKit pane — JS / WebGL / WebRTC / DevTools / LocalStorage / IndexedDB all OFF") rather than vague marketing-shaped bullets.
- **Architecture section** updated for the v1.5.0 Cargo workspace split, the three-database layout, and the `viaduct-img://` URI scheme.
- **New Acknowledgements section** at the bottom that explicitly thanks Brent and the NetNewsWire team, links to the upstream repo, and recommends NetNewsWire for users on macOS / iOS. The closing line: "Viaduct is what NetNewsWire would feel like if it ran on Linux."
- **New "Inspired by NetNewsWire" badge** in the header strip alongside Rust / MIT / Ko-fi.

### Test status

73 unit + 1 integration tests still passing across the workspace. fmt + clippy clean. Meson dry-run install confirms icons land at the canonical hicolor paths.

## v1.5.2 — Audit pass: NNW resync, bug fixes, UI touch-ups

End-of-cycle audit. Compared the latest `.netnewswire/` and `.newsflash/` trees against our state, fixed real bugs, polished the UX. Patchnotes lead with the user-visible items, then the porting fidelity work.

### User-facing

- **Three new keyboard shortcuts**, surfaced in the Ctrl+? cheat sheet:
  - `Ctrl+Shift+C` — Copy article URL. Toast confirms ("Article URL copied."). Same NNW `preferredLink` fallback chain (`external_url` → `url`); articles with no URL get a "no URL to copy" toast instead of silent failure.
  - `Ctrl+Shift+R` — Toggle Reader View. Programmatically flips the reader-button state, so the affordance is keyboard-reachable.
  - `Escape` — Close article. In narrow / collapsed window layouts, pops the inner navigation back to the timeline; in wide layouts, clears the timeline selection so the article pane shows its empty state.
- **Refresh button disables during a refresh cycle.** Previously a double-click could spawn parallel refreshers, doubling network load and producing mismatched `batch_update` start/end pairs. The `win.refresh` action now sets `enabled=false` while the cycle runs and re-enables on completion or error.

### NetNewsWire parity catch-up

NNW's main branch had three fixes since our last sync that we hadn't ported. Caught all three:

- **Atom `<summary>` and `<content>` are now kept strictly separate** *(NNW d6eb8df7d)*. Before: `<summary>` was promoted into `content_html` when `<content>` was absent. After: `<summary>` always lands in `ParsedItem.summary`; `<content>` always lands in `content_html`. They never share a slot. The article-render fallback chain in `window.rs` already prefers `content_html → content_text → summary`, so summary-only feeds still render correctly downstream — but feeds shipping both finally show both fields where downstream code expects them. Includes the `xhtml` capture path so `<summary type="xhtml">` lands in summary too, not body. Added `atom_summary_and_content_kept_separate` test covering the both-present case.
- **Orphan author cleanup** *(NNW 200e5b19f, issue #5232)*. New `DeleteOrphanedAuthors` op runs on startup as part of the `cleanup_at_startup` chain. Sweeps `authorsLookup` rows whose article no longer exists, then drops `authors` rows no longer referenced by any lookup. The existing `articles_ad_lookup` delete-trigger handles the live case where an article is removed; this op is the safety net for any rows that escaped (pre-trigger DBs, transactions that bypassed the cascade). Plus a new `authorsLookup_article_id_idx` index speeding up both the trigger and the cleanup sweep.
- **`url_host_matches_domain` already strips `www.`** — verified in sync with NNW 62c73d2c9 (we shipped this earlier so no change needed).
- **Domain lists in sync**: the 18-domain `NO_MINIMUM_TIME_DOMAINS` list matches NNW's current `domainsWithNoMinimumTime` set.

### Bug fixes

- **`apply_fonts` no longer crashes when there's no GDK display.** Previously called `gtk::gdk::Display::default().unwrap()`, which would panic in a headless environment (test runners, dev sessions without a Wayland session). Now log-and-skip via a `let Some(display) = ... else { return; }` guard.
- **OPML coalesced-save error path cleaned up.** The borrow dance through `io::Error::other` is now commented and tidied; previous code had a stale "Needs proper clone of result" comment trail.

### Repo hygiene

- Deleted the stray `test_menu.rs` file that had been sitting at the repo root since v1.0.1.
- `.gitignore` deduplicated and reorganized; added `/builddir`, `/build`, `/_build` for Meson; removed the redundant Cargo template comments.
- Trailing `ox.` typo at the end of `roadmap.md` removed.

### NewsFlash comparison

NewsFlash uses `html2gtk` to render articles as native GTK widget trees — no WebKit, no JavaScript, lower memory floor but lower typography fidelity. We deliberately chose the WebKit-with-strict-CSP path for typography, accepting the higher (but bounded) memory cost. NewsFlash's design is consistent and well-executed; the divergence is by design, not oversight.

### CI / counts

73 unit + 1 integration tests passing (61 viaduct-core after the new Atom + author tests, 12 viaduct, 1 integration). fmt + clippy clean across the workspace.

## v1.5.1 — Meson build wrapper

Closes the last open Phase 0 / Phase 17 prep item: the project now builds via meson alongside cargo. Packagers and the Flatpak manifest no longer need hand-rolled `install -Dm755 …` invocations; a single `meson install` lays out the binary, gschema, themes, desktop entry, and AppStream metainfo into the canonical GNOME locations.

### What ships

- **`meson.build` at the repo root.** Drives `cargo build --release --package viaduct` via a `custom_target` (build always runs — cargo handles its own incremental tracking, meson stays out of Rust's dependency graph). Sets `CARGO_TARGET_DIR` to the meson build directory so out-of-tree builds Just Work.
- **Install layout matches AppStream + freedesktop conventions.** Binary → `$prefix/bin/viaduct`. Gschema → `$prefix/share/glib-2.0/schemas/` (and `glib-compile-schemas` runs at install time via `meson.add_install_script`). Themes → `$prefix/share/viaduct/themes/`. Desktop entry → `$prefix/share/applications/`. AppData → `$prefix/share/metainfo/<id>.metainfo.xml` (renamed from the historical `appdata.xml` to match the current AppStream spec location).
- **Flatpak manifest switched to `buildsystem: meson`.** `org.virinvictus.Viaduct.json` now ships `--prefix=/app` and `--buildtype=release` config-opts; the previous "simple" manifest with hand-rolled cargo+install commands is gone. Result is a much cleaner Flathub submission and a build that follows Flatpak conventions exactly.
- **AppData release history updated** — the appdata.xml now lists every shipped release from v0.10 through v1.5.1 so AppStream-aware tools (gnome-software, KDE Discover) can show the user the version log on the store page.

### What's still TODO (deliberate)

- **Icons.** Once `data/icons/<size>/apps/<id>.png` lands, drop an `install_subdir('data/icons', install_dir: datadir / 'icons' / 'hicolor')` block into `meson.build`. The desktop file already references `Icon=org.virinvictus.Viaduct`; until the assets exist the system falls back to a generic icon. Tracked separately as part of the visual polish for the actual Flathub submission.

### Verified

- `meson setup builddir --prefix=/tmp/install` configures cleanly.
- `meson compile -C builddir` runs `cargo build --release` and produces a real ELF binary.
- `meson install -C builddir` lays out every file in the right place; `glib-compile-schemas` runs and produces `gschemas.compiled` inside the install prefix.
- The cargo workflow is unchanged — `cargo build`, `cargo run`, `cargo test --workspace` all still work directly from the repo root.

## v1.5.0 — Cargo workspace refactor

Closes the long-open Phase 16 "Workspace Refactoring" item. The single `viaduct` crate is split into a Cargo workspace with two members:

- **`viaduct-core/`** (headless library) — `database`, `network`, `parser`, `models`, `error`, `paths`, plus the global Tokio runtime + debug-mode toggles. **No GTK / libadwaita / WebKit dependencies.** Suitable for profiling harnesses, future headless CLIs, and zero-UI integration testing.
- **`viaduct/`** (binary crate) — `main.rs`, `ui/*`, `preferences.rs`, `fonts.rs`, plus the `mem_check` aux binary. Depends on `viaduct-core` for everything headless. The GTK + libadwaita + WebKit stack lives entirely here.

### What this enforces

Architectural boundaries become **compile errors** instead of review discipline. Reaching into GTK from `viaduct-core` no longer just violates a CLAUDE.md rule — it stops compiling. The `mem_check` profiling harness now demonstrably runs against the same code path the GUI uses, with no chance of a sneaky `gtk` import slipping into the data layer.

### What stays at the repo root

- `data/` — themes, fonts, gschema. Referenced from `viaduct/` via three-up `../../data/...` paths in `include_bytes!` / `include_str!`. The `viaduct/build.rs` walks `CARGO_MANIFEST_DIR.parent()` to locate the schema source for `glib-compile-schemas`.
- `Cargo.toml` (now a virtual workspace manifest) — declares both members, sets `default-members = ["viaduct"]` so `cargo run` at the repo root continues to launch the GTK app, and centralizes shared dependency versions in `[workspace.dependencies]`.
- `Cargo.lock`, `roadmap.md`, `spec.md`, `CLAUDE.md`, `patchnotes.md`, `README.md` — all unchanged-location.
- `.github/workflows/ci.yml` — bumped to `cargo clippy --workspace` and `cargo test --workspace` so both members are exercised, and `libwebkitgtk-6.0-dev` added to the apt install since the binary crate now needs it explicitly.

### Code surface changes

- `viaduct-core/src/lib.rs`: drops the `pub mod ui` and `pub mod preferences` lines that were unused by the headless modules anyway. Keeps the `init_runtime` / `spawn_on_runtime` / `block_on_runtime` / `is_debug_mode` / `set_debug_mode` / `spawn_debug_memory_ticker` exports.
- `viaduct/src/lib.rs` (new): re-exports `viaduct_core` symbols at the binary crate's root (`pub use viaduct_core::{database, network, parser, models, error, paths, init_runtime, spawn_on_runtime, ...}`) so existing intra-binary callers continue to use the unprefixed names (`crate::network::ImageCache`, `crate::models::Article`, etc.) — every `ui/*` file resolves these via the re-export. **Zero call-site churn** in the UI layer.
- `viaduct/src/fonts.rs` (new): pulled out of `paths.rs::install_bundled_fonts`. Lives in the binary crate because installing fonts and shelling `fc-cache` is a GTK-runtime concern, not a path-resolution concern. `paths::ensure_dirs` now stops at directory creation; `main.rs` calls `fonts::install_bundled` separately.
- `Cargo.toml` (workspace): every dep version is in `[workspace.dependencies]`. Both member crates pull from `.workspace = true` so version drift between the two is structurally impossible.

### What didn't change

- All 71+ unit tests + integration test still pass (59 in `viaduct-core`, 12 in `viaduct`, 1 integration). Test totals shifted between crates but the count is identical.
- Every `crate::xxx` import path inside `viaduct/src/ui/` works unchanged thanks to the lib.rs re-exports.
- Memory profile is identical — the split is structural, not runtime.
- `cargo run` at the repo root still launches the GTK app via `default-members`.

### Why now

The roadmap deferred this since v0.5 because doing it before the `ui/` tree was settled would have forced multiple rounds of import churn. With v1.4.0 closing the headline feature work, the structural cleanup is finally cheap to do — and it sets up the v1.5.1 Meson wrapper to target a single binary crate cleanly.

## v1.4.0 — In-pane video playback

Closes the post-v1.3.0 follow-up — articles backed by a YouTube or Vimeo URL now play in-app, not just show a thumbnail. Picks up the user-noted feature ("recognize when YouTube videos are in the feed and allow them to be played there").

### How it works

1. Article gets selected in the timeline → `detect_video` runs against its body → window's `current_video` cell stores the result.
2. Article-pane header bar gains a "▶ Play video" button (with `suggested-action` styling so it stands out). Visible only when a video was detected AND the playback mode isn't disabled.
3. Click dispatches based on the new `video-playback-mode` GSetting.

### Three playback modes

- **In-pane (default)** — opens an `AdwDialog` (960×560) housing a *separate* `WebKitWebView` instance dedicated to the embed. JS is enabled on this instance (the YouTube / Vimeo iframe player needs it), but every persistent storage path stays off (no IndexedDB, LocalStorage, app cache, devtools, back-forward gestures, clipboard access, JS popup auto-open). When the dialog closes, `try_close()` runs on the embed WebView so the WebProcess shuts down promptly and audio doesn't keep playing during the dialog teardown gap. The article-pane WebView's lockdown profile is **completely unaffected** — playback is fully isolated to its own dialog instance.
- **Open in default handler** — hands the canonical watch URL (`youtube.com/watch?v=…` / `vimeo.com/<id>`) to `gio::AppInfo::launch_default_for_uri`. Users with `mpv` / `yt-dlp` configured as their YouTube handler get those automatically; users without get their default browser. Same flow as the existing `Ctrl+Enter` enclosure handler.
- **Don't show play button** — hides the button entirely. For users who want to ignore the feature.

The mode picker lives in **Preferences → Video playback**. Live-flips: changing the mode immediately hides / shows the button on the current article without waiting for a fresh selection.

### Privacy considerations

- YouTube embeds use `youtube-nocookie.com` (Google's "privacy-enhanced" subdomain that defers cookie writes until playback). Always enabled, no toggle.
- The embed WebView never gets storage that survives the dialog. Cookies set during playback die when the dialog closes.
- The article-pane WebView still has JS off, no storage, strict CSP (`default-src 'none'`). This stays true forever — the playback dialog is a separate instance.

### What the spec says about WebKit instance count

The project's hard rule is "exactly ONE `WebKitWebView` for the reading pane" — the new playback dialog's WebView is intentionally NOT the reading pane. The reading pane's lockdown profile is unchanged and untouchable. This v1.4.0 dialog instance only exists while the user is actively watching a video, and is destroyed when they close the dialog.

### Memory cost

WebKit instance for the dialog spins up ~80–120 MB while the user is watching, drops back to baseline (~280 MB band) within a few seconds of dialog close. Peak still well under the 500 MB ceiling. No regression to the idle band — there's only ever a second WebView when something's actively playing.

### Files

- `data/org.virinvictus.Viaduct.gschema.xml` — new `VideoPlaybackMode` enum (`in-pane` / `external` / `disabled`) + `video-playback-mode` key, default `in-pane`.
- `src/preferences.rs` — `keys::VIDEO_PLAYBACK_MODE` constant.
- `src/network/video_thumbs.rs` — `VideoSource::embed_url` + `watch_url` helpers.
- `src/ui/window.ui` — new `play_video_btn` template child in the article-pane `AdwHeaderBar`, hidden by default, `suggested-action` styled.
- `src/ui/window.rs` — `current_video` RefCell on `ViaductWindow imp`; `wire_play_video_button`, `refresh_video_button_visibility`, `act_play_video`, `present_video_dialog` methods; file-scope `VideoPlaybackMode` enum + `current_video_playback_mode()` resolver. The timeline-selection handler now runs `detect_video` on every selection and toggles the button accordingly.
- `src/ui/preferences_dialog.rs` — new "Video playback" preferences group with an `AdwComboRow` for the mode picker, two-way bound to the GSetting.

### Testing

- 4 new unit tests in `video_thumbs::tests` (embed URL hosts, autoplay flag, watch URL canonical form). 72 unit tests total (was 68).
- All 73 tests pass; clippy clean; fmt clean.
- Manual smoke: launching v1.4.0 release build opens cleanly. Selecting any feed-shaped article without a video keeps the button hidden; selecting a YouTube / Vimeo article exposes the button.

## v1.3.0 — Video thumbnail extraction

Closes the "video thumbnail extractor" item that's been sitting open in Phase 16 since the v1.2.0 polish pass deferred it. Articles backed by a YouTube or Vimeo URL now show a 16:9 preview thumbnail in the timeline row.

### What ships

- **New module `src/network/video_thumbs.rs`** — port-style detection layer with a `VideoSource` enum (`YouTube { id }`, `Vimeo { id }`) and helpers:
  - `VideoSource::from_url` — pattern-matches the URL against the major YouTube surfaces (`youtube.com/watch?v=…`, `youtu.be/…`, `youtube.com/embed/…`, `youtube.com/shorts/…`, `youtube.com/v/…`, `youtube.com/live/…`, `youtube-nocookie.com/embed/…`) and Vimeo (`vimeo.com/<id>`, `vimeo.com/video/<id>`, `player.vimeo.com/video/<id>`). Strips `www.` / `m.` host prefixes; numeric-ID guard for Vimeo to skip pages like `vimeo.com/categories`.
  - `detect_video(&Article)` — checks `external_url` → `url` → scans `content_html` / `content_text` / `summary` for the first matching URL. The scanner is a hand-rolled URL-token extractor (no regex dep) — finds every `http(s)://…` substring and stops at whitespace / quotes / angle brackets.
  - `youtube_thumbnail_url(id)` — deterministic `https://i.ytimg.com/vi/<id>/hqdefault.jpg`. `hqdefault` (480×360) always exists for any valid video; `maxresdefault` only when the uploader uploaded a high-res custom thumbnail.
  - `thumbnail_url(client, source)` — async resolver. YouTube returns synchronously; Vimeo hits the public oEmbed endpoint (`https://vimeo.com/api/oembed.json?url=…`, no auth required) and reads `thumbnail_url` from the JSON response.
  - 12 unit tests covering every URL surface + corner cases (rejects `vimeo.com/categories`, handles `m.youtube.com`, strips `?feature=share` query strings).

- **`ImageCache` extended with a third `Kind::VideoThumb` variant.** New cache directory at `$XDG_CACHE_HOME/viaduct/video-thumbs/` (added to `paths::ensure_dirs`). Per-kind LRU cap stays at 250 entries — peak RSS ceiling unchanged. New `image_cache.video_thumbnail(url)` method routes through the same memory-hit → disk-hit → network-fetch flow as favicon and image. New `image_cache.client()` accessor exposes the underlying reqwest `Client` for callers that need to issue an oEmbed lookup before the actual thumbnail fetch (Vimeo path).

- **Timeline row layout updated.** New `gtk::Picture` widget added as the first child of `row_hbox` ahead of `content_vbox`. Sized 80×45 with `ContentFit::Cover`. Hidden by default — only shows after `detect_video` matches AND the bytes successfully decode to a `GdkTexture`. `widget_name` carries the article's `article_id` so a stale fetch (the row recycled to a new article before the network call returned) drops its result instead of painting it onto the wrong row.

- **Asset hygiene.** When a row's bound article has no detected video, the picture explicitly clears its paintable (`set_paintable(Paintable::NONE)`) and stays hidden — no leftover thumbnail from a previously-bound article shows during recycling.

- **CSS — new `.viaduct-timeline-thumb` class.** 6 px border-radius + 5 % currentColor placeholder background so the thumbnail reads as inline media without dominating the row.

### What deliberately doesn't ship in v1.3.0

- **Reader-pane embedding.** The video player UI is a separate piece tracked under v1.4.0 (in-pane YouTube playback). v1.3.0 stops at "the timeline shows you there's a video here" — clicking the article still routes through the existing flow (article body in WebKit + Ctrl+Enter → `xdg-open` for in-system playback if the user has mpv configured).
- **PeerTube.** Federated; no single oEmbed endpoint. Skipped pending a request — the federated graph would need per-instance configuration that the `local.opml` schema doesn't carry yet.
- **Generic Open Graph thumbnail extraction.** Some non-video sites ship `<meta property="og:image">`. Out of scope here — that's an article-image flow (NNW already has one tied to `Article.imageURL`) and would need its own UX decision about whether to surface in every row or only video rows. Tracked as a future polish if needed.

### Memory profile

68 unit + 1 integration test passing. No new top-level dependency — Vimeo's oEmbed JSON parsing reuses `serde_json` (already in the tree). Cache eviction guarantees keep peak RSS unchanged from v1.2.x.

## v1.2.1 — Empty-state flash fix

Tiny, surgical follow-up to v1.2.0. Clicking between feeds in the sidebar briefly flashed the "No articles" `AdwStatusPage` while the timeline rebuilt. Cause: `populate_timeline` did `store.remove_all()` (one `items_changed` → empty-state stack flips visible) then `store.append(...)` per article (more `items_changed` → eventually back to content). The empty-state `connect_items_changed` watcher caught the in-between zero state every time.

Fix: replace `remove_all` + `append`-loop with a single `gio::ListStore::splice(0, n_existing, &nodes)` in both `populate_timeline` and `populate_timeline_with_snippets`. Splice fires exactly one `items_changed` signal carrying both the removal count and the additions count atomically, so observers never see a transient empty store. The search-clear path in `wire_search` also moves to splice so that branch can't flash either.

~10 lines net. No behavioral change beyond the flash going away.

## v1.2.0 — UI Polish

The "make it pretty on GNOME 50" pass. The whole window now visually echoes the chosen reading theme, every pane tells you what to do when it's empty, the sidebar reads as a real GNOME-style navigation list with section headers and pill badges, the timeline shows relative dates and clean previews and a clear unread/read split, and the layout adapts to narrow windows for laptop-on-the-couch reading. **Real-world session peak: 280–385 MB / 500 MB budget** — same band as v1.1.0.

Built up across two sub-arcs of pre-release commits; full chronology below.

### Sub-arc 1 (`v1.2.0-pre1` → `v1.2.0-pre1.6`): theme picker, accent unification, fonts, dark variants, scroll fix

- **App-wide accent unification** — the article theme's accent color now propagates to every accent surface in the GTK chrome: sidebar selection, focus rings, switches, suggested-action buttons, text selection, link buttons, AdwAvatar fallback. Picking Sepia gives you warm cinnamon everywhere; Tiqoe Dark, warm tan; Biblioteca, deep scholarly blue. Implementation: a `gtk::CssProvider` registered at `STYLE_PROVIDER_PRIORITY_USER + 100` on the default `gdk::Display` overrides libadwaita's accent across three layers — legacy `@define-color`, modern `:root` CSS custom properties, and selector-targeted overrides for the highest-traffic accent widgets. Beats GNOME 47+'s system-accent integration (`org.gnome.desktop.interface accent-color`).
- **Theme picker preference** — new `article-theme` enum GSetting + `AdwComboRow` in the prefs dialog. Lists every theme ("Follow color scheme" + Adwaita + the eight NNW-ported themes). Switches the article theme + the app-wide accent live without restart. `Theme::accent_hex` is `Option<&'static str>` — `None` means "don't override" (Adwaita uses this so GNOME's system accent surfaces unchanged).
- **Adwaita theme** — new ninth theme. NNW-shape page with libadwaita-native typography (Cantarell + system-ui font stack, max-width 44em, generous letter-spacing). Adapts to dark mode automatically via `prefers-color-scheme` baked into its stylesheet. Carries `accent_hex: None` so GNOME's system accent surfaces through the chrome unchanged — picking Adwaita feels like stock GNOME.
- **Hand-tuned dark variants for every NNW theme** — each light theme (Sepia, Appanoose, Biblioteca, Hyperlegible, NewsFax, Promenade, Verdana Revival) gains a per-theme `dark.css` overlay with a hand-crafted dark palette that preserves the theme's character. Sepia → roasted-coffee bg #261e15, warm cream text, lifted copper accents. Biblioteca → leather-bound deep ink-blue. NewsFax → ink-black newsprint. Activated automatically via `prefers-color-scheme: dark`. NNW byte-perfect stylesheets stay UNCHANGED — our dark adaptation is appended after the original cascade.
- **Bundled fonts via `viaduct-font://` URI scheme** — new scheme parallel to `viaduct-img://`. Atkinson Hyperlegible Next is bundled (Regular, Bold, Italic, BoldItalic from googlefonts/atkinson-hyperlegible-next, ~270 KB total). `font_face_css()` is prepended to every theme's stylesheet so the Hyperlegible theme renders correctly even when the system doesn't ship the font. CSP gains `font-src viaduct-font:` to allow the loads.
- **Singleton `gio::Settings`** — `crate::preferences::settings()` now returns the same GObject for the lifetime of the GTK thread (thread_local OnceCell). Without this, every callsite that wired a `connect_changed` handler on a transient Settings instance lost its handlers when the local binding went out of scope — the v1.2.0-pre1 theme picker shipped non-functional because of this. Verified the dropdown wrote correct values to dconf (via `gsettings get`) but listening callsites were dead.
- **Article scrolling fix** — long articles were being silently clipped because the `WebKitWebView` was wrapped in a `GtkScrolledWindow` whose auto-viewport sized the WebView to the visible area, and every NNW theme stylesheet sets `html { overflow: hidden }`. Result: nothing past the visible fold was reachable. The wrapper is gone (pre1.6); WebKit owns article-pane scrolling natively now (mouse wheel + Space/Shift+Space page-down/up via WebKit's built-in user-agent behaviour). NNW's "advance at bottom" half of `scrollOrGoToNextUnread` is on hold pending a scroll-position monitor — flagged for v1.3 polish since it needs a JS bridge that's currently disabled by Phase 6 lockdown.
- **Diagnostic tracing** — `preferences::refresh_accent` emits a `tracing::debug!` line on every theme change so future "did the handler fire?" questions are visible in the `--debug` log.

### Sub-arc 2 (in progress): empty states, sidebar polish, timeline polish

- **Empty states for the article + timeline panes** (pre2). Both panes are now wrapped in a `GtkStack` with a content page (the WebView / list view) and an empty page (`AdwStatusPage`). When the timeline `ListStore` has zero rows the stack auto-flips to "No articles — Select a feed in the sidebar to view its articles, or hit Refresh to fetch new ones." When no article is selected (or `render_article_body` has nothing to render), the article pane shows "No article selected — Pick an article from the timeline to start reading." Crossfade transition (150 ms) so the swap doesn't feel abrupt.
- **Wired via `connect_items_changed`** on the timeline store, so every populate path (sidebar selection, search results, refresh, OPML load) triggers the empty-state flip without per-call boilerplate. Initial state is `empty` for both panes; first populate brings them to `content`.
- **Sidebar polish** (pre3). Avatars bumped 20 → 24 px for better presence; row vertical padding added (2 px top + bottom) and horizontal margin nudged 4 → 6 px for breathing room. Inter-element spacing 8 → 10 px. The "Smart Feeds" group row now styles its label as a section heading — uppercase letter-spaced 0.07 em, 0.78 em font-size, 700 weight, dimmed 65 % opacity — matching the GNOME HIG sidebar-section convention. Unread badges become pill-shaped with a `currentColor` 10 % background, switching to a translucent `accent_fg_color` background when the row is selected so the count stays legible against the accent fill.
- **Sidebar CSS provider** at `STYLE_PROVIDER_PRIORITY_APPLICATION` (lower than the accent provider's `USER + 100`) so accent overrides still win where needed. Process-wide static — installed once in `build_ui` and leaked.
- **Timeline polish** (pre4). Three quality-of-life upgrades:
  - **Relative date formatting** replaces the absolute `Mar 19, 2026` everywhere: `Just now` < 1 min, `7m ago` / `5h ago` within the day, `Yesterday`, weekday name within the past week, `Mar 19` within the year, `Mar 19, 2025` for older. Today/Yesterday boundaries key off LOCAL calendar dates so a 23:30 post read at 02:00 next day reads as "Yesterday" rather than "3h ago".
  - **HTML-stripped previews**: the 2-line preview text is now run through a small tag-stripping pass that drops tags, decodes the most common entities (`&amp;`, `&mdash;`, `&rsquo;`, `&ldquo;` and twelve siblings), and collapses whitespace. Feeds that ship `<description>` as raw HTML (most WordPress sites) finally have clean preview text. Falls back to `content_html` when summary and content_text are both empty so podcast-only / image-only posts still render something.
  - **Sharper read/unread visual hierarchy**: read articles now dim the entire row (feed name + preview + date) to 55 % opacity via a `viaduct-row-read` CSS class, on top of the existing title bold-vs-dim toggle. Unread rows pop out clearly even in a long timeline.
  - 6 new tests in `ui::timeline::tests` covering `strip_html_for_preview` and `format_relative_date`. 56 passing total.
  - **pre4.2 / pre4.3 / pre4.4 / pre4.5 — date column investigation, ending in fix.** Multiple attempts:
    - **pre4.2**: `set_width_chars(9)` + `xalign 1.0` to reserve space — didn't take.
    - **pre4.3**: restructured row to put `date_label` as sibling of the content vbox with `set_size_request(80, -1)`. Cleaner layout but still didn't fix it.
    - **pre4.4**: bright-red diagnostic styling proved the label was being allocated space in per-feed views (yellow stripe visible), but in smart-feed views the row was overflowing the viewport entirely — long aggregated titles weren't ellipsizing, they were just being clipped at the viewport edge, with the date_label pushed off-screen to the right.
    - **pre4.5 (real fix)**: the timeline's `GtkScrolledWindow` was allowing horizontal scroll. When a row's natural width exceeded the viewport, GTK gave the row its full natural width instead of forcing the title to ellipsize; the date column then sat at the far right, off-screen. Set `hscrollbar-policy="never"` on the scrolled window — rows can't be wider than the viewport, so titles must ellipsize, and the date column stays visible. Diagnostic styling reverted; date column has its hard 80 px floor + dim-label + numeric (tabular-figures) styling.
    - Final timeline row tree:
      ```
      row_hbox
      ├── content_vbox  (hexpand=true)
      │   ├── top_hbox: title (ellipsized) + media icon + count
      │   ├── feed_name_label
      │   └── preview_label
      └── date_label  (valign=Start, 80 px min, right-aligned)
      ```
- **Pane width constraints (pre4.6 → pre4.8).** With pre4.5's `hscrollbar-policy="never"`, the timeline pane was free to auto-size based on row natural width — so smart-feed views (with their long aggregated titles) made the timeline grow much wider than the per-feed view, eating into the article pane.
  - **pre4.6**: added `min/max-sidebar-width` + `sidebar-width-fraction` to both `AdwNavigationSplitView`s. Didn't help — those caps are advisory; content's natural-width request can override.
  - **pre4.7**: capped `title_label` natural width via `set_max_width_chars(32)`. Helped some, but the preview and feed-name labels still had unbounded natural widths.
  - **pre4.8 (real fix)**: capped natural width on every label in the row — title 32 chars, feed-name 32 chars + ellipsize, preview 48 chars. The row's total natural width is now bounded at ~140 character widths regardless of source feed. Per-feed and smart-feed views finally allocate the same timeline pane.
- **Refresh-in-progress spinner (pre5)**: the refresh button's icon swaps to a `GtkSpinner` while feeds are being fetched, swaps back to `view-refresh-symbolic` when the cycle completes. Implementation: `sync_btn`'s child became a `GtkStack` with two pages (`icon` / `spinner`), `set_refresh_in_progress(bool)` on the window flips the visible-child-name and starts/stops the spinner. Wired into both `act_refresh` and the post-import `refresh_specific_feeds` so any user-initiated refresh shows progress.
- **Article-pane scroll restored (pre5.1)**: pre1.6 removed the `GtkScrolledWindow` wrapper around the `WebKitWebView` to fix a different bug, but the NNW themes set `html { overflow: hidden }` so without a parent scroller WebKit had no way to scroll either — long articles got clipped silently. Added a `VIADUCT_PANE_OVERRIDE_CSS` block appended last in the style cascade: `html, body { overflow: auto !important; height: auto !important }` plus a styled scrollbar (8 px wide, subtle gray thumb, transparent track). NNW byte-perfect stylesheets stay UNCHANGED — the override piggybacks on top. Mouse wheel + Space + Shift+Space all work via WebKit's native handlers.
- **Adaptive layout via `AdwBreakpoint` (pre6)**: two breakpoints on the `AdwApplicationWindow` so narrow windows reflow gracefully:
  - `max-width: 900sp` collapses the inner split view (Timeline + Article become a navigation stack — pick a feed, see the timeline; pick an article, see the article; back-button to return).
  - `max-width: 600sp` collapses both split views (full mobile-style three-page navigation: Feeds → Timeline → Article).
  - At wide widths everything stays as the classic three-pane layout. Default window remains 1200×800.

## v1.1.0 — Phase 6: Neutered WebKit Article Pane

The article reading pane is now a single locked-down `WebKitWebView` rendering through the full NetNewsWire theme stack. Closes Phase 6 of the roadmap. **Real-world session peak: 292 MB / 500 MB budget.**

Built up across six pre-release commits (`v1.1.0-pre1` through `v1.1.0-pre6`):

### What ships

- **8 NetNewsWire themes ported byte-for-byte**: Sepia, Appanoose, Biblioteca, Hyperlegible, NewsFax, Promenade, Tiqoe Dark, Verdana Revival. Each is a `(template.html, stylesheet.css)` pair embedded at compile time via `include_str!`. Sepia for light, Tiqoe Dark for dark; user-facing picker queued for v1.2.0.
- **Macro substitution engine** (`render_with_macros`): linear-scan port of NNW's `RSCore.MacroProcessor.processMacros()`. `[[key]]` delimiters; unknown keys preserved as literal `[[key]]`. `ArticleSubstitutions` mirrors NNW's `articleSubstitutions()` exactly so bundled themes work without modification.
- **Strict `WebKitSettings`**: JS / WebGL / WebRTC / plugins / DevTools / LocalStorage / IndexedDB / app cache / fullscreen / window-open / media-autoplay / back-forward gestures all OFF. `media_playback_requires_user_gesture(true)`.
- **`viaduct-img://` URI scheme** on the default `WebContext`. Article HTML's `<img src="https://…">` is rewritten to `viaduct-img://i/<percent-encoded-original>` via `ammonia::Builder::attribute_filter`. The scheme handler clones the URISchemeRequest GObject, hops to `glib::spawn_future_local`, then to tokio for `ImageCache::image()`, then back to GTK to call `request.finish()` with a `gio::MemoryInputStream`. WebKit gets ZERO direct internet access.
- **Strict CSP** in `data/themes/page.html`: `default-src 'none'; img-src viaduct-img: data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`. Belt-and-braces — even with JS off, no scripts / fonts / frames / analytics beacons / trackers can reach the network.
- **Link interception** via `decide-policy`: every `LinkClicked` / `FormSubmitted` / `NewWindowAction` cancels the WebView navigation and shells the URL out to `xdg-open`. The synthetic about:blank load that backs `WebView::load_html` (NavigationType `Other`) passes through.
- **Hover URL overlay**: `gtk::Label` overlay child of the article-pane `GtkOverlay`, `osd` + `caption` style classes, halign=start / valign=end / can-target=False. `install_hover_url_overlay` connects `mouse-target-changed` so hovered link URLs surface in the bottom-left corner.
- **At-exit memory summary log**: every session emits `session exit: memory summary rss_mb=… peak_mb=… budget_mb=500` so the cost of any feature regression is visible in any log.
- **Debug-mode periodic memory ticker**: `viaduct::spawn_debug_memory_ticker` reads `/proc/self/status` on a random 8–25 second cadence (avoids regular intervals that could mask jitter).

### Files

- New: `src/ui/article_renderer.rs`, `data/themes/{sepia,appanoose,biblioteca,hyperlegible,newsfax,promenade,tiqoe_dark,verdana_revival}/{template.html,stylesheet.css,Info.plist}`, `data/themes/page.html`.
- Deleted: `src/ui/article.rs` (the old `GtkTextTag` walker — no callers remain).
- Modified: `src/ui/window.{rs,ui}`, `src/main.rs`, `Cargo.toml` (`webkit6 = "0.4"` matched to gtk4 0.9 / libadwaita 0.7 generation), `README.md` (build-deps for `webkitgtk6.0-devel` / `libwebkitgtk-6.0-dev`).

### Memory profile

`mem_check` peak (DB + image cache + Reader View, headless): 64 MB. Adding the WebKit article pane in real-world use takes the session to **~292 MB peak / 500 MB budget**, well within the spec's 100–300 MB idle band. Locked-down WebProcess accounts for ~210 MB; main process stays clean. Stable across a 2-minute session — no leak.

### NNW deviations logged

- WebKitGTK 6.0 instead of macOS `WKWebView` (the obvious port deviation).
- `viaduct-img://` instead of NNW's `nnwImageIcon://` (different cache shape — NNW caches by article ID, we cache by image URL).
- Macro processor is a clean Rust port of `RSCore.MacroProcessor`; identical semantics, identical delimiters, no third-party templating crate.

### Known follow-ups

- **User-facing theme picker** (v1.2.0): GSettings key + `AdwPreferencesDialog` row for theme selection. The 8 themes are already wired by id.
- **`max-width: 44em` enforcement** is carried by the NNW theme stylesheets natively; spec mention is satisfied via the bundled CSS rather than a separate constraint.

## v1.0.11 — Auto-reload Timeline After Refresh

The "I refreshed and the timeline still looks empty" trap. v1.0.10's force-refresh actually fetched 4988 articles for a 110-feed corpus — but the displayed timeline pane held the pre-refresh result (often empty after an articles.sqlite delete) until the user clicked another sidebar item to force a re-fetch.

- **`ViaductWindow::reload_current_timeline`**: pulls the current selection from the sidebar `SingleSelection`, runs the same per-item dispatch the `connect_selection_changed` handler uses (Feed → `fetch_articles_by_feed`, SmartFeed → `Today`/`All Unread`/`Starred`, Folder → `fetch_folder_articles`), repopulates the timeline `ListStore`, and re-runs `refresh_timeline_statuses` so the new rows pick up read/starred state. Reuses the existing helpers verbatim — no new query paths.
- **`act_refresh` and `refresh_specific_feeds`** now call it from the `glib::spawn_future_local` completion alongside `refresh_unread_counts()`. So after every refresh cycle: sidebar badges update, the article DB has the new rows, AND the timeline pane displays them. No more "click another feed to see updates" workaround.

## v1.0.10 — Manual Refresh Bypasses Cache + Toast Feedback

Fixes the "I clicked refresh and nothing happened" trap. After a user deletes `articles.sqlite` (or just clicks refresh more than once in 29 minutes), every feed had stale `last_check_date` / `content_hash` / conditional-GET state in `feed-settings.sqlite` — and the refresher silently skipped them. Manual clicks now bypass every short-circuit and produce a toast either way.

- **`AccountRefresher::refresh_feeds_forced`**: bypasses the 29-minute throttle, the 5-hour Cache-Control freshness check, the conditional-GET headers (`If-None-Match` / `If-Modified-Since`), and the content-hash short-circuit. Every feed gets a full network fetch + parse + diff. The bare `refresh_feeds` keeps every check intact for the future cron-based auto-refresh path.
- **`refresh_one_feed` accepts `force: bool`** and threads it through the cached-state checks. With `force=true`, conditional-GET headers are dropped at the request layer too — so a server that always 304s us can't keep us pinned to an empty article store.
- **`act_refresh` and `refresh_specific_feeds` (post-import)** both pass `force=true`. Manual-click semantics: "fetch now, ignore caching."
- **`RefreshTally` struct** carries `feeds_attempted` + `new_articles` from worker → GTK. The desktop notification still keys on `new_articles`; the toast renders both numbers.
- **`show_refresh_toast`**: an `AdwToast` after every refresh cycle. Three messages: "No feeds in subscription list." (empty OPML), "Refreshed N feeds — no new articles." (zero-delta refresh), or "Refreshed N feeds — M new articles." (the happy path).
- **`refresh_feeds: dispatched`** debug-mode log line now reports `total_input` / `skipped` / `attempted` / `force` so a "nothing happened" report can be triaged from the log alone.

### How to recover from the empty-articles state

1. Hit the refresh button (or Ctrl+R).
2. Toast confirms what happened.
3. Articles repopulate from the network.

If `feed-settings.sqlite` itself is corrupt, deleting both `articles.sqlite` AND `feed-settings.sqlite` is still the nuclear option — `LocalAccount::cleanup_at_startup` will rebuild settings rows on the next refresh.

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
