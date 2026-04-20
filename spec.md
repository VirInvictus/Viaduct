# Viaduct — Application Specification

**Version:** 1.0  
**Target:** GNOME 50+, GTK4/libadwaita  
**Language:** Rust (2024 Edition)  
**Build System:** Cargo / Meson (for Flatpak packaging)  
**License:** GPLv3

---

## 1. Mission Statement

Viaduct is a fast, native GNOME RSS reader achieving full feature-parity with NetNewsWire. It is a direct translation of the NetNewsWire architectural philosophy—strict background threading, aggressive SQLite caching, and native text rendering—into the Linux ecosystem via Rust and GTK4. 

Design philosophy: **Speed and Data Sovereignty.** Viaduct handles massive subscription lists and complex sync states without locking the UI thread. It targets an idle memory footprint of 250MB, trading ultra-minimalist asceticism for rock-solid performance, offline image caching, and multi-threaded sync reliability. 

---

## 2. Architecture

### 2.1 The Rust Engine Port

Viaduct completely isolates the network and data layers from the UI thread using Rust's async ecosystem.

```text
┌─────────────────────────────────────┐
│          Viaduct Engine (Rust)      │
│  (tokio multi-thread runtime)       │
├─────────────────────────────────────┤
│  [Sync & Fetch Coordinators]        │
│   ├─ Local OPML Engine              │
│   ├─ FreshRSS / Miniflux API        │
│   └─ Feedbin / Nextcloud API        │
├─────────────────────────────────────┤
│  [Data Layer - rusqlite]            │
│   ├─ feeds & folders                │
│   ├─ articles (HTML, unread, FTS)   │
│   └─ sync_state (cursors, tokens)   │
└──────────┬──────────────────────────┘
           │ (crossbeam channels)
    ┌──────┴────────────────────┐
    │  GTK4 Main UI Thread      │
    └───────────────────────────┘
```

**Thread Safety:** The GTK thread only ever reads from local models or sends commands down the channel. It never waits on a network socket.

### 2.2 Native Render Pipeline (No WebKit)

Web engines are memory black holes. To hit the 250MB RAM target, Viaduct uses native GTK widgets.

1. **Fetch:** Raw XML is fetched and parsed via `quick-xml`.
2. **Sanitize:** The HTML payload is stripped of scripts, iframes, trackers, and inline styles using `ammonia`.
3. **Map:** The clean HTML is parsed into `GtkTextTag` elements (bold, italic, blockquote, code) and rendered in a `GtkTextView`.
4. **Assets:** Images are downloaded asynchronously, cached to disk, and rendered inline via `GdkTexture`.
5. **Escape Hatch:** If a user needs the interactive webpage, pressing `Enter` pipes the URL to their default system browser.

### 2.3 Widget Tree

```text
AdwApplicationWindow
├── AdwHeaderBar
│   ├── [left]  AdwSplitButton (sidebar toggle)
│   ├── [left]  GtkButton (mark all read)
│   ├── [title] GtkLabel (Viaduct)
│   ├── [right] GtkToggleButton (search)
│   └── [right] GtkMenuButton (primary menu)
├── AdwOverlaySplitView
│   ├── [sidebar] GtkScrolledWindow
│   │   └── GtkListView (Smart Feeds, Folders, Subscriptions)
│   └── [content] AdwOverlaySplitView
│       ├── [sidebar] GtkScrolledWindow
│       │   └── GtkListView (Article List - Recycled)
│       └── [content] GtkScrolledWindow
│           └── GtkTextView (Article Body)
└── [bottom] GtkActionBar (Sync progress / Background tasks)
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

### 4.2 Sync Engines
Full implementation of state-machine logic to handle conflict resolution and API rate limits.
* **Local:** Default. OPML intake, direct RSS fetching.
* **Feedbin:** Full REST API sync.
* **Miniflux:** Google Reader API endpoint sync.
* **FreshRSS:** Google Reader API endpoint sync.
* **Nextcloud News:** Native API sync.

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

All state is maintained in `$XDG_DATA_HOME/viaduct/viaduct.db`.

### 6.1 SQLite Configuration
* **WAL Mode:** Write-Ahead Logging is enforced. The background fetcher can write thousands of new articles to the database while the user is actively scrolling and reading without throwing database locks or stuttering the UI.
* **FTS5:** Full-Text Search is enabled on the `articles` table for instantaneous local querying.

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
* `reqwest`: HTTP client (with TLS).
* `quick-xml`: Zero-allocation XML parsing.
* `ammonia`: Strict HTML sanitization.
* `rusqlite`: SQLite bindings.
* `crossbeam-channel`: Main/Worker thread communication.

### C/GTK Libraries (Frontend)
* `gtk4` (via `gtk4-rs`): Minimum 4.16.
* `libadwaita` (via `libadwaita-rs`): Minimum 1.7.

---

## 8. Flatpak Distribution

Viaduct is packaged as a Flatpak-first application.

* **Permissions:** Strictly locked down.
    * `network`: Required for feed fetching.
    * `xdg-run/dconf`: Required for GNOME settings.
    * No arbitrary home directory access. OPML import/export handled entirely via `org.freedesktop.portal.FileChooser`.
* **Background Daemon:** App is configured to support background execution permissions via portals, allowing it to sync on a cron schedule even when the UI is closed.

---

## 9. What Viaduct Is Not

Explicitly out of scope for v1.0 and likely forever:

* **Not a browser.** It does not embed WebKit. If an article requires Javascript to read, it belongs in Firefox.
* **Not a social network.** No sharing buttons, no Twitter integration, no Mastodon crossposting.
* **Not an algorithm.** No "suggested reads," no engagement metrics. It shows exactly what was published, in reverse-chronological order.

---

## 10. Success Criteria

Viaduct v1.0 is done when:

1. It can import a 500-feed OPML file without hanging the GTK main thread.
2. The background engine can fetch and parse 1,000 new articles while the user smoothly scrolls the list view.
3. Idle memory consumption stabilizes between 200MB and 300MB after a full sync and image cache phase.
4. Miniflux, FreshRSS, and Feedbin sync bidirectional read/star states perfectly without data loss.
5. Search finds text across all cached articles instantly using FTS5.
6. The application completely complies with GNOME HIG and libadwaita styling.
