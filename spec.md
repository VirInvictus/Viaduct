# viaduct: Application Specification

**Version:** 1.9.1  
**Target:** GNOME 50+, GTK4 ≥ 4.16, libadwaita ≥ 1.7, WebKitGTK 6.0 *(libadwaita is on its way out; §12 holds the locked post-libadwaita design, which Phase 20 implements)*  
**Language:** Rust (2024 Edition)  
**Build System:** Cargo workspace (`viaduct-core` + `viaduct`) / Meson wrapper for Flatpak packaging  
**License:** MIT

---

## 1. Mission Statement

viaduct is a fast, native GNOME RSS reader achieving full feature-parity with NetNewsWire's **local and Inoreader accounts**. It is a direct translation of the NetNewsWire architectural philosophy (strict background threading, aggressive SQLite caching, and native text rendering) into the Linux ecosystem via Rust and GTK4.

Design philosophy: **Speed and Data Sovereignty.** viaduct handles massive subscription lists without locking the UI thread. It targets idle RAM of **100–300 MB** and a hard peak ceiling of **500 MB**, trading ultra-minimalist asceticism for rock-solid performance and offline image caching. Other remote sync engines are out of scope; the app supports local accounts and Inoreader sync.

---

## 2. Architecture

### 2.1 The Rust Engine Port

viaduct completely isolates the network and data layers from the UI thread using Rust's async ecosystem.

```text
┌─────────────────────────────────────┐
│          viaduct Engine (Rust)      │
│  (tokio multi-thread runtime)       │
├─────────────────────────────────────┤
│  [Refresh Coordinator]              │
│   ├─ DownloadSession (reqwest)      │
│   ├─ Parser pool (quick-xml/serde)  │
│   └─ LocalAccount orchestrator      │
├─────────────────────────────────────┤
│  [Data Layer]                       │
│   ├─ ArticlesDatabase (rusqlite)    │
│   │    articles + statuses + FTS5   │
│   ├─ FeedSettingsDatabase (rusqlite)│
│   │    ETag / icon / overrides      │
│   └─ OPML file (feeds + folders)    │
└──────────┬──────────────────────────┘
           │ (tokio mpsc + glib channel)
    ┌──────┴────────────────────┐
    │  GTK4 Main UI Thread      │
    └───────────────────────────┘
```

**Thread Safety:** A dedicated writer task owns both SQLite connections. The GTK thread only ever reads from local models or sends commands down the channel; it never waits on a network socket or a database write.

### 2.2 Neutered WebKit Render Pipeline

Unconstrained web engines are memory black holes. viaduct ships **exactly one** `WebKitWebView` instance for the article reading pane, configured to give WebKit zero direct internet access and zero scripting surface.

1. **Fetch:** Raw XML is fetched and parsed via `quick-xml`. CDATA-wrapped bodies are captured via the same path as `Event::Text`.
2. **Sanitize + rewrite:** The HTML payload runs through `ammonia::Builder` with `viaduct-img` added to `url_schemes`. An `attribute_filter` rewrites every `<img src="https://…">` to `viaduct-img://i/<percent-encoded-original>`. Scripts / iframes / inline styles / trackers are stripped in the same pass.
3. **Theme + render:** Sanitized body is fed into one of 8 NetNewsWire-ported `.nnwtheme` bundles (Sepia / Appanoose / Biblioteca / Hyperlegible / NewsFax / Promenade / Tiqoe Dark / Verdana Revival) via a port of NNW's `MacroProcessor` (`[[key]]` substitution). The themed result is wrapped in a page template carrying a strict CSP meta tag.
4. **Lockdown profile** applied to the WebView at construction:
    * **JavaScript: off** (runtime + HTML5 inline `<script>` markup).
    * **WebGL / WebRTC / plugins / DevTools: off.**
    * **HTML5 LocalStorage / IndexedDB / app cache: off.**
    * **`media_playback_requires_user_gesture(true)`**: no autoplay.
    * **`javascript_can_open_windows_automatically(false)`**: belt-and-braces.
    * **Back-forward gestures, fullscreen: off.**
5. **CSP enforcement** in the page wrapper:
    `default-src 'none'; img-src viaduct-img: data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`
6. **`viaduct-img://` URI scheme handler** routes every image lookup through our `ImageCache` (memory LRU → disk → network). WebKit can render images, but every byte travels through the cache, and no other origin can load anything.
7. **Link interception:** `decide-policy` cancels every `LinkClicked` / `FormSubmitted` / `NewWindowAction` and shells the URL out to `xdg-open` (system browser). `Other` / `Reload` / `BackForward` allowed through so `load_html`'s synthetic about:blank works.
8. **Hover URL overlay:** `mouse-target-changed` updates a `gtk::Label` overlay (osd + caption) in the bottom-left so the user can preview link destinations.
9. **Memory:** see §11 for the full budget + measured numbers + the architectural floor analysis. The locked-down WebProcess + the GTK4 / libadwaita / WebKitGTK shared libraries together pin a ~150 MB anon floor inside the main process that no Rust-side allocator tuning can reach (`#[global_allocator] = mimalloc` only redirects Rust allocations; the C side keeps its own glibc heap). The single-WebView constraint is the biggest knob we *do* control here; every additional `WebKitWebView` would add ~100–150 MB.

### 2.3 Widget Tree

```text
AdwApplicationWindow
├── AdwBreakpoint (max-width 900sp → inner_split_view.collapsed)
├── AdwBreakpoint (max-width 600sp → both split_views.collapsed)
└── AdwToastOverlay
    └── AdwNavigationSplitView outer_split_view (220–360 px sidebar)
        ├── [sidebar] AdwNavigationPage "Feeds"
        │   └── AdwToolbarView
        │       ├── AdwHeaderBar
        │       │   ├── [start] mark_all_read_btn
        │       │   ├── [start] sync_btn (GtkStack: refresh icon ⇄ spinner)
        │       │   ├── [end]   search_btn (toggle)
        │       │   └── [end]   menu_btn (primary menu)
        │       └── GtkScrolledWindow
        │           └── GtkListView sidebar_list_view
        │               (TreeListModel: Smart Feeds, Folders, Subscriptions)
        └── [content] AdwNavigationSplitView inner_split_view (320–480 px sidebar)
            ├── [sidebar] AdwNavigationPage "Timeline"
            │   └── AdwToolbarView
            │       ├── AdwHeaderBar + GtkSearchBar
            │       └── GtkStack timeline_stack
            │           ├── content: GtkScrolledWindow (hscrollbar=never)
            │           │            → GtkListView (recycled, capped natural width)
            │           └── empty:   AdwStatusPage "No articles"
            └── [content] AdwNavigationPage "Article"
                └── AdwToolbarView
                    ├── AdwHeaderBar + reader_btn
                    └── GtkStack article_stack
                        ├── content: GtkOverlay
                        │              ├── WebKitWebView article_web_view
                        │              └── GtkLabel url_overlay (hover preview)
                        └── empty:   AdwStatusPage "No article selected"
```

---

## 3. UI Specification

### 3.1 Sidebar (Feeds & Folders)

Displayed via `AdwOverlaySplitView`. Populated via a `gio::ListModel` bound to the `feeds` table.
* **Smart Feeds:** Pinned at the top (Today, All Unread, Starred).
* **Folders:** Expandable tree nodes.
* **Badges:** Unread counts display dynamically next to feeds and folders.

### 3.2 Article List (Timeline)

The middle pane. This is the primary memory trap for poorly written readers.
* **Strict Recycling:** Uses `GtkListView` with `GtkSignalListItemFactory`. The app only creates enough widgets to fill the vertical height of the screen. As the user scrolls, the widgets are recycled and repopulated from the database. 
* **Data Model:** Displays Title, Source, Date, and a 2-line text preview.

### 3.3 Main View Area (Article Body)

* **Typography:** Adheres to system fonts or user-defined monospace/serif overrides.
* **Media Enclosures:** Audio/video attachments are displayed as discrete UI blocks at the top or bottom of the article, with an action button to send the URL to a system media player (e.g., `mpv`).

---

## 4. Feature Parity (NetNewsWire Standard)

### 4.1 Smart Feeds
Virtual feeds generated dynamically via SQLite queries, automatically updating as the database changes.
* **Today:** Articles published in the last 24 hours.
* **All Unread:** Global unread aggregate.
* **Starred/Saved:** User-flagged articles retained indefinitely.

### 4.2 Local Account (Only Account in v1.0)
viaduct ships a single account type in v1.0: **Local**. OPML intake, direct RSS/Atom/JSON Feed fetching, all state stored on disk under `$XDG_DATA_HOME/viaduct/`.

Remote sync engines (Feedbin, Miniflux, FreshRSS, CloudKit, NewsBlur, Inoreader) are explicitly out of scope for v1.0. They may be added post-1.0, but only if they can be implemented without compromising the local-first architecture or the RAM budget.

### 4.3 Reader View (Optional, RAM-Gated)
A local Readability-style extractor for truncated feeds. Runs on-demand only (hotkey or toolbar), never eagerly, and is gated by the 500 MB peak-RAM ceiling. If the extractor can't hit that budget running in-process, it either runs in a short-lived subprocess or is cut from v1.0. NetNewsWire's Reader View calls a remote Mercury service, which is not an option here.

---

## 5. Keyboard Shortcuts

Standard desktop accelerators, prioritizing spatial navigation without forcing a modal Vim state.

| Action | Shortcut |
|--------|----------|
| Smart Read (Scroll down, jump to next unread) | Space |
| Move down list | j, Down |
| Move up list | k, Up |
| Toggle Read/Unread | m |
| Star/Save Article | s |
| Open in Browser | Enter |
| Fetch/Sync Now | Ctrl+R |
| Focus Search | Ctrl+F |
| Toggle Sidebar | F9 |

---

## 6. Storage & State Persistence

All state lives under `$XDG_DATA_HOME/viaduct/`:

* `local.opml`: feed + folder hierarchy (coalesced save, ~500 ms debounce, atomic temp-file + rename).
* `articles.sqlite`: `articles`, `statuses`, `authors`, `authorsLookup`, FTS5 `search`.
* `feed-settings.sqlite`: the per-feed cache (ETag, Last-Modified, Cache-Control, favicon URLs, edited names, authors JSON, folder-relationship JSON, last-check date, per-feed Reader View preference).

Image and favicon caches live under `$XDG_CACHE_HOME/viaduct/`.

### 6.1 SQLite Configuration
* **WAL Mode:** Write-Ahead Logging is enforced on both databases. The background fetcher can write thousands of new articles while the user actively scrolls without throwing database locks or stuttering the UI.
* **Single writer:** A dedicated thread owns the write connection to each database and serializes all writes; the GTK thread holds only a `Sender` and never blocks on SQLite. The writer (and the sync worker) run their receive loop under a panic supervisor that restarts on a panic, so one bad op can't take the DB layer down for the session.
* **Read pool:** Articles reads (timeline fetches, search, unread counts) go to a small pool of read-only connections rather than queuing behind the writer. WAL lets these readers run concurrently with the single writer, so a long write never stalls the timeline.
* **FTS5:** Full-Text Search is enabled on the `articles` table for instantaneous local querying. (NetNewsWire uses FTS4; we modernize.)

### 6.2 The Pruning Engine
To enforce the memory and disk footprint, the database is regularly vacuumed.
* Articles older than 30 days are automatically deleted.
* Starred/Saved articles are excluded from pruning.
* Unread status does not save an article from pruning; if it hasn't been read in a month, it is dropped.
* `VACUUM` runs at startup only on launches where the prune step actually removed rows, so a steady-state launch pays no full-file rewrite. A cheap `wal_checkpoint(TRUNCATE)` runs every startup regardless to keep the WAL bounded.

---

## 7. Dependencies

### Rust Crates (Backend)
* `tokio`: Async runtime.
* `reqwest`: HTTP client (with `rustls-tls`, `gzip`, `brotli`).
* `quick-xml`: Zero-allocation XML parsing.
* `ammonia`: Strict HTML sanitization with `viaduct-img://` allowlisted via `Builder::add_url_schemes`.
* `rusqlite`: SQLite bindings (bundled, FTS5).
* `crossbeam-channel`: Main/Worker thread communication.
* `readability`: Local Reader View extraction.
* `oo7`: libsecret credential storage (Inoreader OAuth tokens).

### C/GTK Libraries (Frontend)
* `gtk4` (via `gtk4-rs`): Minimum 4.16.
* `libadwaita` (via `libadwaita-rs`): Minimum 1.7. *(Removed by Phase 20; see §12.)*
* `webkitgtk-6.0` (via `webkit6` 0.4): Minimum 2.42; the article reading pane runs a single neutered instance (see §2.2).

---

## 8. Flatpak Distribution

viaduct is packaged as a Flatpak-first application.

* **Permissions:** Strictly locked down.
    * `network`: Required for feed fetching.
    * `xdg-run/dconf`: Required for GNOME settings.
    * No arbitrary home directory access. OPML import/export handled entirely via `org.freedesktop.portal.FileChooser`.
* **Background Daemon:** App is configured to support background execution permissions via portals, allowing it to sync on a cron schedule even when the UI is closed.

---

## 9. What viaduct Is Not

Explicitly out of scope for v1.0 and likely forever:

* **Not a browser.** It uses a heavily neutered WebKit instance purely for typography, with Javascript strictly disabled. If an article requires Javascript to read, it belongs in Firefox.
* **Not a social network.** No sharing buttons, no Twitter integration, no Mastodon crossposting.
* **Not an algorithm.** No "suggested reads," no engagement metrics. It shows exactly what was published, in reverse-chronological order.

---

## 10. Success Criteria

viaduct v1.0 is done when:

1. It can import a 500-feed OPML file without hanging the GTK main thread.
2. The background engine can fetch and parse 1,000 new articles while the user smoothly scrolls the list view.
3. Idle memory after a warm refresh + image-cache warm stays in the 400–500 MB band; peak under realistic load (15–30 min cadence, mostly conditional-GET 304s) stays under 600 MB. Peak under continuous force-refresh stress (every 5 min × 130 feeds × force=true bypassing 304s) plateaus at ~540 MB. **See §11 for the post-mortem on the original 100–300 MB target.**
4. FTS5 search returns results in under 50 ms against a 50,000-article corpus.
5. The application fully complies with GNOME HIG and libadwaita 1.7 styling. *(Superseded by §12 once Phase 20 lands: the design language becomes viaduct's own. The app keeps running correctly under GNOME, which is a behavior promise, not a styling one.)*
6. A Flathub-accepted Flatpak build runs in a strict sandbox (network permission only; OPML I/O through portals).

---

## 11. Memory Budget: Post-Mortem (v2.6.x)

The original criterion #3 promised "idle 100–300 MB, peak < 500 MB." The v2.6.3 → v2.6.18 diagnostic chain found that target unreachable under the chosen toolkit. This section documents what was learned, what's recoverable, and what isn't.

### Measured numbers (130-feed corpus, AMD Ryzen 7 PRO 360, post-v2.6.18)

| Workload | RSS plateau | Peak |
|---|---|---|
| Idle, warm | 380–460 MB | — |
| Realistic 30-min cadence (mostly 304s after v2.6.20 fix) | ~430 MB | ~480 MB |
| Continuous force-refresh @ 5 min, 38 cycles, 3 hours | 425–470 MB | **538 MB** |

### What the floor is made of

`/proc/self/smaps_rollup` breakdown at steady state:

| Class | Size | Source |
|---|---|---|
| **anon** (mimalloc) | ~160 MB peak commit | Rust allocations: parse trees, ArticleNode, channels, glib closures |
| **anon** (non-mimalloc) | ~150 MB | C-side allocations from GTK / GLib / WebKit / libxml2; `#[global_allocator]` only redirects Rust |
| **file** | ~100 MB | SQLite mmap (capped 64 MB), binaries, fonts, installed `.so`s |
| **shmem** | ~0–5 MB | WebKit IPC buffers (the WebProcess proper is a separate process and not counted in our RSS) |
| **stacks + misc** | ~80–120 MB | 12 tokio + 3 fixed threads × ~8 MB virtual stack + kernel-side overhead |

The non-mimalloc anon block is the architectural floor. GTK / WebKit / GLib pull in heavy C libraries that allocate from glibc malloc independently of our global allocator choice. We cannot squeeze them without changing toolkits.

### The wins, in order of impact

| Version | Change | Δ peak |
|---|---|---|
| v2.6.18 | tokio worker_threads(4) + max_blocking_threads(8) | **−215 MB** (746 → 531) |
| v2.6.11 | SQLite mmap_size 30 GB → 64 MB + journal_size_limit | **−100 MB** |
| v2.6.20 | force=false on periodic refresh (304 short-circuit) | **−40 to −90 MB** under realistic load |
| v2.6.12 | mimalloc as global allocator (replacing glibc) | **−30 to −50 MB** |
| v2.6.14 | mi_collect(true) per cycle + WebKit cache off + MIMALLOC_PURGE_DELAY=100 | **−20 to −40 MB** |
| v2.6.15 | Skip timeline repopulate while hidden | **−20 to −30 MB** |
| v2.6.9 | Bound refresh-cycle concurrency (semaphore = 8) | **−25 MB** in-cycle peak |
| v2.6.5 | Disk-cache sweep (favicons/images/video-thumbs) | bounded growth, no plateau Δ |

### What was NOT achievable, and why

Hitting "idle 100–300 MB" requires removing the GTK4 + WebKit + GLib + libxml2 baseline, which is non-negotiable for the typography fidelity goal (NetNewsWire-byte-perfect themes via `WebKitWebView`). NewsFlash's html2gtk approach hits ~250–400 MB by rendering articles as native GTK widgets; possible, but it throws away the theme system and every shipped NNW theme bundle.

### Hard rules going forward

- **Single WebView.** Adding a second `WebKitWebView` adds ~100–150 MB. The video-playback-mode preference deliberately uses a transient dialog WebView that's destroyed on close to avoid this.
- **`#[global_allocator] = mimalloc::MiMalloc`** stays. Switching back to glibc malloc loses ~30–50 MB at peak and drops mi_collect's aggressive return.
- **`MIMALLOC_PURGE_DELAY=100`** stays.
- **tokio thread caps** stay (worker=4, max_blocking=8).
- **SQLite mmap_size = 64 MB** + **journal_size_limit = 64 MB** stay.
- **Periodic refresh = force=false** stays. Manual refresh = force=true is the only path that bypasses conditional-GET.

### When to revisit

If a future v2.x adds something that pushes peak past 600 MB on the realistic 30-min-cadence test, treat it as a regression. The v2.6.16 `/proc/self/smaps_rollup` breakdown on `diag: refresh cycle pre/post` lines is permanent; every refresh logs anon/file/shmem deltas, so the next leak surfaces fast.

---

## 12. Design System: post-libadwaita (Phase 20 target)

**Status: decisions locked, not yet implemented.** This section is the contract Phase 20b–20e build toward; §2.3's widget tree and §7's dependency list still describe the shipped v2.8.x app and are rewritten when the toolkit cut lands. Colophon's Phase 6 is the pilot and its patterns are the default; every divergence below is deliberate and reasoned.

### 12.1 Toolkit stance

GTK4 (≥ 4.16, for `:root` CSS custom properties) and WebKitGTK stay. **libadwaita goes**: the stylesheet, the adaptive widgets, and `adw::StyleManager` are replaced by plain GTK4 widgets plus an application stylesheet viaduct owns outright. GTK4 is Wayland-native and WebKitGTK is orthogonal to adwaita entirely. No new dependency is required by any item here.

Two rules bind throughout, unchanged: **no regression for a GNOME user** (behavior, not look) and **no breach of the §11 memory ceilings**. A widget-and-CSS migration should be memory-neutral; any item that escalates gets its own `heaptrack`/`massif` checkpoint before landing.

### 12.2 Decoration and layout

- **Window buttons stay** on the outermost header bar. Colophon chose `show-title-buttons=false`; we decline, because removing a GNOME user's close button is a behavior regression rather than a restyle. Tiling users hide them with `gtk-decoration-layout=""`, which Phase 19's CSD audit already covers. No code path depends on a compositor-drawn titlebar; every pane is CSD already, which is the posture a tiling WM wants.
- **Three per-pane `GtkHeaderBar`s stay**, replacing `AdwHeaderBar` / `AdwToolbarView`. Colophon merged its two bars into one; we decline, because its two bars were both chrome for a single content view, whereas our three carry genuinely pane-specific cargo (sidebar: mark-all-read, sync, search, menu; timeline: the search bar; article: reader toggle, appearance, share, play-video) that must follow its pane when panes collapse.
- **The nested `AdwNavigationSplitView` pair becomes a nested `GtkPaned` pair.** Sidebar and timeline widths persist in GSettings, saved on close rather than on `notify::position` (which fires every pixel of a drag).
- **Auto-collapse is preserved, width-driven.** The two `AdwBreakpoint`s (900sp → inner collapsed; 600sp → both collapsed) have no `GtkPaned` equivalent, so the behavior is hand-built: watch allocation width and flip pane visibility at the same thresholds, with manual toggles alongside. This is the one deliberate departure from Colophon's "the app never reshuffles its own layout on resize" rule. Two reasons: dropping it would regress existing narrow-window behavior, and viaduct is three-pane where the pilot was two, so three panes at a quarter tile (~480 px) is unusable rather than merely cramped. A `GtkPaned` collapses to the surviving child on its own and holds the position for the return trip, so the mechanism is visibility, not re-parenting.

### 12.3 Dark/light resolution

`adw::StyleManager` / `adw::ColorScheme` are replaced by a synchronous `ReadOne` on `org.freedesktop.portal.Settings` over gio D-Bus at startup, plus a `SettingChanged` subscription. Carried over verbatim from the pilot, each point earned: 1000 ms timeout; a `Read` fallback for portals predating `ReadOne` (its reply wraps the value in a second variant layer); the connection held so the subscription outlives startup; a default that degrades to a working theme rather than failing when no portal answers; and **only an explicit `1` means dark**, matching `AdwStyleManager`. Fixed themes force polarity via `set_gtk_application_prefer_dark_theme` so stock-widget internals (text selection, spinners) follow a palette the owned sheet cannot reach.

Take the pilot's **weak-ref redraw registry** with it: the migration there exposed a real leak where per-widget `connect_dark_notify` closures accumulated on the singleton and were never disconnected.

**The eight NNW article themes are byte-for-byte cargo and do not change.** Their resolution path does: `select_for_dark_mode` (`article_renderer.rs`) and the Adwaita theme's `prefers-color-scheme` handling key off `AdwStyleManager::is_dark()` today and must read the portal instead. The Adwaita article theme's `accent_hex: None` currently lets the libadwaita system accent through; once libadwaita is gone that accent has no source, so the Adwaita theme needs an explicit stance rather than inheriting one.

### 12.4 The owned stylesheet

A single `theme.rs`: one `const STRUCTURE: &str` plus a `format!` interpolating the palette into `:root` `--c-*` custom properties. No `.css` file, no build script, no codegen. Flat, square, hard borders, no shadows, denser spacing. Existing adwaita class *names* are kept (`heading`, `dim-label`, `card`, `boxed-list`, `suggested-action`, `flat`) with owned definitions, so `.ui` churn is zero. Coverage must include the GTK-default gaps that read as foreign once adwaita's sheet is gone: menu popovers, tooltips, scrollbars, text selection, focus outline.

Two non-negotiables, both inherited as lessons rather than preferences:

- **Load at `STYLE_PROVIDER_PRIORITY_USER + 1`, never `APPLICATION`.** A global `~/.config/gtk-4.0/gtk.css` loads at USER (800) and outranks APPLICATION (600), silently half-overriding the in-app sheet. This hid in the pilot for a long time because a Kanagawa Dragon system skin over a Dragon default looks correct. Brandon's desktop carries exactly such a skin, so viaduct would hit it. Remove the previous provider before adding the new one on every apply.
- **Scope the focus ring from day one.** The pilot shipped `*:focus-visible { outline: … }`; pressing a bare modifier (a tiling workspace-switch chord) flips GTK into focus-visible mode and flashes the accent across every widget in the focus chain. It **does not reproduce in a screenshot**, which is why the pilot's own verification missed it and a sibling app found it later. Write it scoped to `button, entry, spinbutton, switch, checkbutton, check, dropdown, scale` with `outline-offset: -1px`. Lists and grids show position through the selection background and need no outline.

House rule, unit-tested: **no `font-family` anywhere in the sheet.** The bundled fonts (`fonts.rs`: Inter, SourceSerif4, JetBrainsMono, Atkinson Hyperlegible) are unchanged.

### 12.5 Widget replacements

`viaduct-core` is adw-free (its only references are doc comments), so nothing below can reach parsing, networking, or the database. The surface is 26 `adw::` types across 17 Rust files and 6 `.ui` files, entirely within `viaduct/src/`.

| adw type | Replacement |
|---|---|
| `Application`, `ApplicationWindow` | `gtk::` equivalents (`content` property becomes `child`) |
| `NavigationSplitView` ×2, `NavigationPage`, `Breakpoint` ×2 | nested `GtkPaned` + width-driven collapse (§12.2) |
| `ToolbarView`, `HeaderBar` | three `GtkHeaderBar`s (§12.2) |
| `WindowTitle` | ellipsizing `GtkLabel` as `title-widget` |
| `Bin` ×5 | `gtk::Widget` + `BinLayout`, **unparenting children in `dispose()`** |
| `Toast`, `ToastOverlay` | `GtkOverlay` + auto-hiding `GtkRevealer`, newest-wins, `can-target=false` |
| `StyleManager`, `ColorScheme` | portal `Settings` (§12.3) |
| `PreferencesDialog`/`Page`/`Group` | `gtk::Window` (`transient_for` + `modal`) + `GtkListBox.boxed-list` |
| `ActionRow`, `ComboRow` | owned rows module (`row`/`value_row`; ComboRow = `row` + `GtkDropDown` suffix) |
| `Dialog` ×5 | `gtk::Window` modal + a shared `close_on_escape` helper |
| `StatusPage` | inline title + description composite |
| `AboutDialog` | `gtk::AboutDialog` |
| **`Avatar`** | **net-new**: sidebar favicon with an initials + hashed-colour fallback (`ImageCache::color_for` already ports NNW `ColorHash`) |
| **`SwitchRow`, `EntryRow`, `SpinRow`, `PreferencesRow`** | **net-new** row flavours extending the owned rows module |
| **`AlertDialog` + `ResponseAppearance`** | **net-new**: `gtk::AlertDialog` for the delete-feed confirm and rename-feed prompt, including the destructive styling |

`AdwOverlaySplitView` appears only in a `window.rs` doc comment as a hypothetical and is not in use. `Clamp` and `Banner` are unused, so the pilot's `clamp.rs` does not transfer.

Two behaviours adw gave for free and must be rebuilt: **Escape handling** (plain `gtk::Window` has none; reconcile with the existing `close-article` Escape action in `actions.rs`) and **`Bin` child teardown** (each subclass unparents in `dispose()` or GTK warns at finalize).

### 12.6 Packaging

**The Flatpak stays on `org.gnome.Platform`. Evaluated and declined, not deferred:** GTK4 ships in the GNOME runtime and does **not** ship in `org.freedesktop.Platform`, so moving would mean building and maintaining GTK4 as manifest modules purely to drop a name from the runtime id. Our manifest never listed libadwaita as a module (the runtime carries it), so there is nothing to remove. Revisit only if a gtk4 freedesktop BaseApp or extension appears.

Dropped from the manifests: the workspace `adw` dependency, `viaduct/Cargo.toml`'s `adw.workspace = true`, and `libadwaita-1-dev` from CI. `APP_ID` must keep matching the `.desktop` basename and the Flatpak app-id on both build paths so Hyprland `windowrulev2` matching stays stable.

Version: the pilot took a **major bump for a release with zero feature changes**, on the grounds that the design language and the GNOME dependency story both changed. The same argument applies here.
