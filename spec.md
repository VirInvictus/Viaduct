# viaduct — Application Specification

**Version:** 1.0.0  
**Target:** GNOME 50+, GTK4/libadwaita  
**Language:** Rust (2024 Edition)  
**Build System:** Cargo / Meson (for Flatpak packaging)  
**License:** GPLv3

---

## 1. Mission Statement

viaduct is a fast, native GNOME RSS reader achieving full feature-parity with NetNewsWire's **local and Inoreader accounts**. It is a direct translation of the NetNewsWire architectural philosophy—strict background threading, aggressive SQLite caching, and native text rendering—into the Linux ecosystem via Rust and GTK4.

Design philosophy: **Speed and Data Sovereignty.** viaduct handles massive subscription lists without locking the UI thread. It targets idle RAM of **100–300 MB** and a hard peak ceiling of **500 MB**, trading ultra-minimalist asceticism for rock-solid performance and offline image caching. Other remote sync engines are out of scope for v1.0 — the app supports local accounts and Inoreader sync.

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

**Thread Safety:** A dedicated writer task owns both SQLite connections. The GTK thread only ever reads from local models or sends commands down the channel — it never waits on a network socket or a database write.

### 2.2 Neutered WebKit Render Pipeline

Unconstrained web engines are memory black holes. viaduct ships **exactly one** `WebKitWebView` instance for the article reading pane, configured to give WebKit zero direct internet access and zero scripting surface.

1. **Fetch:** Raw XML is fetched and parsed via `quick-xml`. CDATA-wrapped bodies are captured via the same path as `Event::Text`.
2. **Sanitize + rewrite:** The HTML payload runs through `ammonia::Builder` with `viaduct-img` added to `url_schemes`. An `attribute_filter` rewrites every `<img src="https://…">` to `viaduct-img://i/<percent-encoded-original>`. Scripts / iframes / inline styles / trackers are stripped in the same pass.
3. **Theme + render:** Sanitized body is fed into one of 8 NetNewsWire-ported `.nnwtheme` bundles (Sepia / Appanoose / Biblioteca / Hyperlegible / NewsFax / Promenade / Tiqoe Dark / Verdana Revival) via a port of NNW's `MacroProcessor` (`[[key]]` substitution). The themed result is wrapped in a page template carrying a strict CSP meta tag.
4. **Lockdown profile** applied to the WebView at construction:
    * **JavaScript: off** (runtime + HTML5 inline `<script>` markup).
    * **WebGL / WebRTC / plugins / DevTools: off.**
    * **HTML5 LocalStorage / IndexedDB / app cache: off.**
    * **`media_playback_requires_user_gesture(true)`** — no autoplay.
    * **`javascript_can_open_windows_automatically(false)`** — belt-and-braces.
    * **Back-forward gestures, fullscreen: off.**
5. **CSP enforcement** in the page wrapper:
    `default-src 'none'; img-src viaduct-img: data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`
6. **`viaduct-img://` URI scheme handler** routes every image lookup through our `ImageCache` (memory LRU → disk → network). WebKit can render images, but every byte travels through the cache, and no other origin can load anything.
7. **Link interception:** `decide-policy` cancels every `LinkClicked` / `FormSubmitted` / `NewWindowAction` and shells the URL out to `xdg-open` (system browser). `Other` / `Reload` / `BackForward` allowed through so `load_html`'s synthetic about:blank works.
8. **Hover URL overlay:** `mouse-target-changed` updates a `gtk::Label` overlay (osd + caption) in the bottom-left so the user can preview link destinations.
9. **Memory:** real-world session peak measured at **292 MB / 500 MB budget**. Locked-down WebProcess sits ~210 MB; main process stays clean.

### 2.3 Widget Tree

```text
AdwApplicationWindow
├── AdwHeaderBar
│   ├── [left]  AdwSplitButton (sidebar toggle)
│   ├── [left]  GtkButton (mark all read)
│   ├── [title] GtkLabel (viaduct)
│   ├── [right] GtkToggleButton (search)
│   └── [right] GtkMenuButton (primary menu)
├── AdwNavigationSplitView (responsive; collapses on narrow windows)
│   ├── [sidebar] GtkScrolledWindow
│   │   └── GtkListView (Smart Feeds, Folders, Subscriptions)
│   └── [content] AdwNavigationSplitView
│       ├── [sidebar] GtkScrolledWindow
│       │   └── GtkListView (Article List — recycled via GtkSignalListItemFactory)
│       └── [content] GtkOverlay
│           ├── GtkScrolledWindow → WebKitWebView (article body)
│           └── GtkLabel url_overlay (hover URL preview, bottom-left)
└── [bottom] GtkActionBar (refresh progress / background tasks)
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

* `local.opml` — feed + folder hierarchy (coalesced save, ~500 ms debounce, atomic temp-file + rename).
* `articles.sqlite` — `articles`, `statuses`, `authors`, `authorsLookup`, FTS5 `search`.
* `feed-settings.sqlite` — per-feed cache: ETag, Last-Modified, Cache-Control, favicon URLs, edited names, authors JSON, folder-relationship JSON, last-check date, per-feed Reader View preference.

Image and favicon caches live under `$XDG_CACHE_HOME/viaduct/`.

### 6.1 SQLite Configuration
* **WAL Mode:** Write-Ahead Logging is enforced on both databases. The background fetcher can write thousands of new articles while the user actively scrolls without throwing database locks or stuttering the UI.
* **Single writer:** A dedicated `tokio` task owns both connections and serializes all writes; the GTK thread holds only a `Sender`.
* **FTS5:** Full-Text Search is enabled on the `articles` table for instantaneous local querying. (NetNewsWire uses FTS4; we modernize.)

### 6.2 The Pruning Engine
To enforce the memory and disk footprint, the database is regularly vacuumed.
* Articles older than 30 days are automatically deleted.
* Starred/Saved articles are excluded from pruning.
* Unread status does not save an article from pruning; if it hasn't been read in a month, it is dropped.
* `VACUUM` is run periodically on application startup to reclaim blocks.

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
* `libadwaita` (via `libadwaita-rs`): Minimum 1.7.
* `webkitgtk-6.0` (via `webkit6` 0.4): Minimum 2.42; the article reading pane runs a single neutered instance — see §2.2.

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
3. Idle memory consumption stabilizes between 100 MB and 300 MB after a full refresh and image-cache warm; peak never exceeds 500 MB across any supported operation.
4. FTS5 search returns results in under 50 ms against a 50,000-article corpus.
5. The application fully complies with GNOME HIG and libadwaita 1.7 styling.
6. A Flathub-accepted Flatpak build runs in a strict sandbox (network permission only; OPML I/O through portals).
