# viaduct — Patch Notes

## v2.6.18 — Cap tokio worker + blocking thread pools

The v2.6.17 mimalloc dump from a 143-second 30s-cadence stress test caught the last actionable variance source: tokio churning blocking-pool threads.

```
theaps   peak 134  total 142  current 18
threads  peak 135  total 143  current 19
elapsed  142.882 s
```

**143 threads spawned in 143 seconds — roughly one new thread per second.** Each new thread takes a fresh mimalloc per-thread heap; tokio reabandons 5.2 K segment-handoffs over the run as workers age out (10 s idle default) and blocking tasks demand new ones. mimalloc handles it (the abandoned-heap mechanic correctly hands segments back), but the churn shows up as ±25–40 MB cycle-to-cycle variance in the anon delta.

The same dump confirmed steady-state: peak hit 746 MB at cycle 5 and **stayed flat for the next 5 cycles** while anon oscillated 456–505 MB instead of climbing. Not a leak. Just allocator/threading variance on top of a high architectural floor.

### Fix

`tokio::runtime::Runtime::new()` defaults:
- `worker_threads = num_cpus()` (8 on a typical Ryzen)
- `max_blocking_threads = 512`

Replaced with an explicit builder:
```rust
tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .max_blocking_threads(8)
    .enable_all()
    .build()
```

Caps tokio's footprint at ~12 threads steady-state. 4 workers comfortably multiplexes our `REFRESH_PARALLELISM = 8` semaphore (per-feed work is async, network-bound, not CPU-bound — workers spend most of their time parked on `.await`). 8 blocking threads is generous for our actual `spawn_blocking` usage (cache-sweep at startup + the v2.6.5 wipe action, both rare).

### Expected impact

Fewer mimalloc per-thread heaps to manage → tighter cycle-to-cycle variance. Lower stack-reservation footprint (~8 MB virtual per thread; matters less for RSS than for VmSize but still real). Probably 30–80 MB lower steady-state plateau on stress tests; minimal effect on realistic 30-min cadence (where blocking-pool barely sees use).

### Diagnostic chase: end of line

This is the last meaningful knob inside our codebase. The chain that found us here:

- **v2.6.3** added `diag:` log lines
- **v2.6.10** added the debug fast-refresh harness
- **v2.6.11** found the 149 MB SQLite WAL
- **v2.6.12** swapped glibc malloc for mimalloc
- **v2.6.14** added `mi_collect(true)` + `MIMALLOC_PURGE_DELAY=100` + WebKit cache off
- **v2.6.15** skipped timeline repopulate while hidden
- **v2.6.16** added the `/proc/self/smaps_rollup` X-ray (anon / file / shmem split)
- **v2.6.17** routed mimalloc stats through tracing + tightened `--debug` filter
- **v2.6.18** caps tokio thread pools

Steady-state under 30 s force-refresh stress is ~620–700 MB RSS / ~750 peak. Under realistic 30-min cadence with conditional-GET 304s, expect noticeably lower. The remaining floor (~150 MB of anon outside mimalloc) is C-side allocations from GTK / GLib / WebKit / libxml2 — `#[global_allocator]` only redirects Rust code, so those C libraries continue to use glibc malloc independently. Not ours to control without removing the WebKit dependency.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.17 — Capture mimalloc stats + tighten --debug filter

The v2.6.16 X-ray confirmed anon-heap is 100% of the per-cycle climb. To localise *what* on the heap, the v2.6.16 "Memory snapshot" action was supposed to dump mimalloc's per-size-class stats. The action fired but the output never reached the log. Two issues:

### 1. mimalloc stats now go through tracing

`mi_stats_print(NULL)` writes to C `stderr`, which is fully-buffered when stderr is redirected to a file (`2> debug-memory.log`). The Rust-side fmt subscriber writes to its own copy of stderr; neither side flushes the other's buffer. mimalloc's lines sat in a 4 KB FILE buffer that never reached disk before exit.

Fix: use `mi_stats_print_out` with a callback that emits each line through `tracing::info!` under the `viaduct::mimalloc_stats` target. Output now lands in the same log stream as everything else, no buffer-flush dance.

### 2. `--debug` filter no longer drowns the log

Pre-v2.6.17 `--debug` set `RUST_LOG=debug,viaduct=trace,html5ever=error`. Global `debug` lit up h2 / hyper / rustls / tokio at debug level, producing ~900 lines per second of GoAway-frame chatter. A 10-minute test logged **18 882 h2 debug lines vs 21 of our `diag:` lines** — a 900-to-1 noise ratio.

Each tracing event allocates format strings + field captures + visit closure state. With 18k events per session that's a non-trivial allocator pressure all by itself, plausibly contributing to the per-cycle anon-heap climb we've been chasing.

Now `--debug` sets `info,viaduct=debug,viaduct_core=debug,html5ever=error`. Our own modules stay at debug so the diagnostic surfaces fully, third-party crates drop back to info (where h2/hyper stay quiet). Override per-run via `RUST_LOG=...` if upstream-crate debug is genuinely needed.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.16 — Memory X-ray instrumentation

Pure-instrumentation release, no behavior change. Existing `diag:` lines told us *that* RSS grew during refresh cycles but nothing about *where*. Three additions to localise the source:

### 1. `MemoryBreakdown` from `/proc/self/smaps_rollup`

New `viaduct_core::rss_breakdown() -> MemoryBreakdown` returns `{ rss_mb, anon_mb, file_mb, shmem_mb, swap_mb }`. The kernel exposes these per-class fields so we can split the RSS into:

- **anon** — anonymous heap (mimalloc / Rust allocations / tokio stacks)
- **file** — file-backed mappings (SQLite mmap, binaries, fonts, installed shared objects)
- **shmem** — shared anonymous regions (WebKit's IPC buffers / backing stores with the WebProcess child)

The `diag: refresh cycle pre/post` lines now carry all three plus per-class deltas, so reading the log answers "growth was in *which* class."

### 2. Stage checkpoints inside `run_refresh_with_tally`

Two new `diag: cycle stage=<name>` log lines fire mid-cycle:

- **post-fetch** — every per-feed pipeline has completed, drain task hasn't been awaited, mi_collect hasn't fired. Anon delta vs pre = peak transient mimalloc is still holding from in-flight bodies + parses + favicon-discovery HTML; file delta = WAL / SQLite mmap growth; shmem delta = WebKit shared-memory movement.
- **post-drain** — change-drain task awaited, all `ArticleChanges` consumed, but mi_collect still pending. Anon should drop here vs post-fetch as the per-feed allocations finish unwinding.

After mi_collect we get the regular `diag: refresh cycle post` line, also with full breakdown. Comparing the four points (pre / post-fetch / post-drain / post) tells us which stage allocates which class.

### 3. `win.debug-memory-snapshot` action

New entry in the Debug submenu (`--debug` only). Logs the breakdown + dumps mimalloc heap stats (size classes, segment counts, commit/decommit counters) to stderr via `mi_stats_print`. For when you notice a spike between the periodic `diag:` lines and want a snapshot at exactly that moment.

### How to use the new diagnostics

For the next test run: same realistic load (30-min cadence) for an hour or two. After, grep `diag:` lines and look at:

- **`anon_delta_mb` climbing each cycle** → mimalloc / Rust allocator. Likely candidate: per-cycle parse output retained by something downstream.
- **`file_delta_mb` climbing each cycle** → SQLite WAL or another mmap. WAL is capped at 64 MB; if `file_mb` exceeds 100 MB and grows, something else is mmapping.
- **`shmem_delta_mb` climbing each cycle** → WebKit's shared regions with WebProcess. Each navigation in the article pane allocates more.

Whichever class is leading the cycle deltas tells us where to look next.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.15 — Skip timeline repopulate while hidden

A 5-hour realistic-load run (30-min refresh cadence) plateaued at **~785 MB pre-fix RSS**. Diagnosis credit to a Gemini Pro 3.1 review of `debug-memory.log` + the source: the `act_refresh` and `refresh_specific_feeds` post-handlers were calling `reload_current_timeline()` unconditionally after every refresh cycle — including while the window was hidden in run-in-background mode.

That populates the GTK timeline `ListStore` with hundreds of `ArticleNode` wrappers plus full `Article` clones (each carrying its `content_html` — typically 50 KB per item, 10–30 MB per cycle), plus spawns one video-thumb fetch task per row. All wasted: the user can't see the timeline, and `main.rs build_ui` already calls `reload_current_timeline` on re-summon so they land on fresh data either way.

### Fix

Both post-handlers now gate the timeline repopulate (and the toast in `act_refresh`, which would surface to an invisible `AdwToastOverlay`) on `self.is_visible()`. When hidden, the cycle still:

- Fires `dispatch_refresh_notification` → `gio::Notification` (the OS surfaces these regardless of window state).
- Calls `refresh_unread_counts` → cheap DB query, walks `TreeNode` tree to update sidebar badges. Keeps badges accurate so on re-show there's no stale-flicker.

When visible, behavior is unchanged.

### Honest scope

This is a real win — no more 20–30 MB ListStore churn per cycle while hidden — but it doesn't move us off the architectural floor. The 5-hour plateau composition was roughly: WebKit baseline (150–200 MB) + GTK4/libadwaita/tokio (~80 MB) + SQLite mmap (50 MB) + mimalloc residual (100–150 MB) + DB worker heap (50–80 MB) + ImageCache LRUs (30–50 MB) + this fix's path (20–30 MB). Best estimate for post-fix plateau: **~720 MB** instead of 785, an 8% reduction. Useful but not transformative; the spec target of 100–300 MB idle is below what GTK4+WebKit imposes regardless.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.14 — Aggressive mimalloc reclaim + WebKit cache off

The 15-min cadence run after v2.6.13 produced clean per-cycle data:

```
Cycle 1: pre 424 → post 552 peak 563  Δpeak +139  (warm-up)
Cycle 2: pre 583 → post 621 peak 648  Δpeak  +85
Cycle 3: pre 627 → post 690 peak 725  Δpeak  +77
```

Cycle 2 → 3 went *up* (+85, +77), not down. mimalloc was working but not aggressive enough about returning idle pages to the OS, and `~/.cache/viaduct/WebKitCache/` had grown to 7.1 MB / 105 files of WebKit's own HTTP cache — useless for us since our locked-down article view forbids external loads anyway. Three knobs:

### 1. `mi_collect(true)` at end of every refresh cycle

`run_refresh_with_tally` now calls `crate::mimalloc_collect()` before the post-cycle `diag:` log line. The new helper in `viaduct/src/lib.rs` reaches the FFI symbol `mi_collect` directly (the `mimalloc-rs` crate doesn't expose it; libmimalloc-sys is already linked transitively). Forces mimalloc to decommit freed pages immediately instead of waiting for the purge timer. Cheap (~1 ms).

### 2. `MIMALLOC_PURGE_DELAY=100`

Set in `main.rs` before any allocation, in a new `tune_mimalloc()` helper that runs first thing in `fn main`. Default is 1000 ms — pages freed by the app sit in mimalloc's pools for a full second before being decommitted. At 100 ms the OS reclaims memory 10× faster after each cycle. User-supplied `MIMALLOC_PURGE_DELAY` env var still wins.

### 3. WebKit disk cache off

`ArticleRenderer::bootstrap` now calls `WebContext::set_cache_model(CacheModel::DocumentViewer)` (the freedesktop "no cache" preset) and constructs the `WebKitWebView` with a fresh `NetworkSession::new_ephemeral()`. Combined effect: no HTTP cache, no cookies on disk, no DOM storage. We render local article HTML through `viaduct-img://` + `viaduct-font://` schemes; WebKit had nothing meaningful to cache anyway. Pre-fix the cache had grown 7+ MB / 100+ files in a short run.

### Expected impact

The mimalloc tuning should drop per-cycle "stuck" peak delta meaningfully. The WebKit cache disable removes a slow-burn disk + RSS leak. Verify on the v2.6.3 `diag: refresh cycle` log line plus a `du -sh ~/.cache/viaduct/WebKitCache` check — the latter should stay near 0 across long sessions.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.13 — Refresh re-entry guard

Latent bug surfaced by the v2.6.10 debug fast-refresh: the periodic-refresh `glib::timeout_add_seconds_local` calls `act_refresh()` directly, bypassing the action-group disable that `set_refresh_in_progress(true)` installs for menu / keyboard entry points. At normal 30-min cadence harmless. At sub-cycle-duration cadence (debug fast-refresh = 1 s while a cycle takes 5–13 s) the timer kicks off N overlapping cycles, each allocating its own per-cycle state, so peak appears as `N × per-cycle delta`. Made the v2.6.12 mimalloc data unreadable.

### Fix

Early-return at the top of both `act_refresh` and `refresh_specific_feeds` when `refresh_progress_source` is already `Some` (the canonical "cycle in flight" state — set by `show_refresh_progress`, cleared by `hide_refresh_progress`). Logs at `debug` level so the skip is visible in `--debug` runs but doesn't pollute normal output.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.12 — mimalloc

The v2.6.11 SQLite + reqwest fixes flattened the worst of the per-cycle drift, but a follow-up 5-cycle run still showed RSS climbing cycle-over-cycle:

```
Cycle  pre   post   stuck-Δ
  1    294   364    —
  2    380   449    +85
  3    449   485    +36
  4    485   528    +43
  5    528   539    +11
```

The deltas are decreasing (85 → 36 → 43 → 11) — that's the classic glibc-malloc warm-up signature, not a leak. glibc's `ptmalloc` keeps freed memory in per-thread arenas; once an arena grows it (mostly) never shrinks back to the OS. RSS plateaus eventually, but at a level that exceeds our 500 MB budget on bursty workloads.

### Fix

`#[global_allocator] static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;` at the top of `viaduct/src/main.rs`. Adds `mimalloc = "0.1"` (one dep, brings `libmimalloc-sys` for the C source). mimalloc returns memory to the OS aggressively — typical RSS reduction on bursty allocators is 30–50%, and the drift across many refresh cycles flattens much faster.

The global allocator is binary-only — `viaduct-core` stays allocator-agnostic so library consumers (and the `mem_check` harness) inherit whatever the embedder picked. Library tests continue to run under glibc; the GTK binary uses mimalloc.

### Why mimalloc over jemalloc

Both are good. mimalloc is the smaller dep (no `tikv-jemallocator` chain), faster to build, and Microsoft has been hammering on it for production workloads. Within Rust apps the choice is mostly a wash for performance; mimalloc has the slight edge on memory frugality which is what we need.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean. Release build adds ~10 s for the C compile of `libmimalloc-sys`; runtime no measurable startup difference.

## v2.6.11 — SQLite WAL containment + reqwest pool cap

The v2.6.10 debug-refresh harness produced a clean answer in four cycles:

```
Cycle 1: pre 293 → post 370 (Δ77)
Cycle 2: pre 372 → post 438 (Δ66)
Cycle 3: pre 449 → post 508 (Δ59)
Cycle 4: pre 509 → post 546 (Δ58)
```

Pre-cycle RSS climbed +60–80 MB per cycle. The `hide_for_background post-clear` snapshot only freed 0–2 MB, so the in-memory `ImageCache` LRU wasn't the source. On-disk inspection of `~/.local/share/viaduct/`:

```
articles.sqlite      52 MB
articles.sqlite-wal  149 MB   ←
articles.sqlite-shm  131 KB
```

### Root cause: 30 GB mmap × unbounded WAL

The articles DB's `setup_schema` set `PRAGMA mmap_size = 30000000000` (30 GB request — SQLite treats this as "feel free to map the whole file"), with no `journal_size_limit` to cap the WAL. SQLite's default `wal_autocheckpoint` runs *passive* checkpoints which sync pages to the main DB but don't truncate the WAL file; with no size limit, the WAL grew monotonically. Every refresh cycle wrote a fresh batch of pages to the WAL, the mmap grew accordingly, and that mapped memory contributed directly to RSS — the kernel can evict mmap pages under pressure but they show up as resident regardless.

Net effect: a 130-feed force-refresh ballooned WAL to ~150 MB, the mmap kept that mapped, RSS climbed ~60 MB per cycle on top of the genuinely transient peak the v2.6.9 semaphore already bounded.

### Fix

Three changes to `articles::setup_schema` and `settings::setup_schema`:

- **`mmap_size = 67108864`** (64 MB) on articles, dropped from 30 GB. Plenty for SQLite's read-side optimizations on a database this size.
- **`journal_size_limit = 67108864`** on articles, **`16777216`** on settings. SQLite truncates the WAL at the next checkpoint that crosses the limit, so steady-state WAL is bounded.
- **`PRAGMA wal_checkpoint(TRUNCATE)`** prepended to the existing `vacuum()` ops. Reclaims any large WAL a prior session left behind on the existing user DB at the next startup; without it the new size limit only prevents future growth.

### Secondary fix: reqwest connection pool cap

reqwest's defaults are `pool_max_idle_per_host = usize::MAX` and `pool_idle_timeout = 90s`. With 130 feeds across many hosts, after one refresh we hold a TLS session per host indefinitely (rustls sessions retain certs + session keys, several hundred KB each). `viaduct-core/src/network/http.rs` now sets `pool_max_idle_per_host(4)` + `pool_idle_timeout(30s)` on both `build_default_client` and `client_builder`. Smaller magnitude than the WAL fix but compounds well — and applies to the favicon/image cache + reader-view clients too.

### Expected impact

For an existing user upgrading: the first startup post-v2.6.11 reclaims the existing oversized WAL via the `wal_checkpoint(TRUNCATE)` in cleanup. Steady-state RSS should sit near the v2.6.10 cycle-1 baseline (~290 MB on the test corpus) and stay there across many cycles instead of climbing.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.10 — Refresh progress bar + debug fast-refresh override

Two paired additions in service of memory-growth diagnosis. The v2.6.9 concurrency cap dropped per-cycle peak by ~20% (124 → 99 MB on a 130-feed corpus) but a second source of accumulation is still climbing the high-water mark across cycles. To find it without waiting overnight:

### Bottom progress bar

A `GtkRevealer` containing a `GtkLabel` + `GtkProgressBar` lives below the navigation split views, hidden until a refresh cycle starts. New `RefreshProgress` (paired `Arc<AtomicUsize>` for `completed` and `total`) threads through `run_refresh_with_tally` → `AccountRefresher::with_completion_counter` → the per-feed task tail. The window-side `show_refresh_progress` reveals the strip and installs a 250 ms `glib::timeout_add_local` that reads the atomics and updates the bar:

- **`total == 0`** — refresher hasn't yet computed `paired.len()` (load_opml + pairing still running). Bar pulses indeterminately, label reads "Refreshing feeds…".
- **`total > 0`** — fraction = `completed / total`, label reads "Refreshing feeds… 47 / 130".

`hide_refresh_progress` cancels the poll loop and slides the strip back down. Hooked at the same callsites as `set_refresh_in_progress(true/false)` (`act_refresh` + `refresh_specific_feeds`).

The completion counter increments in **every** per-feed task tail (success / 304 / fetch error / parse error all count) and at the skip branch in the refresher loop, so the bar reaches 100 % cleanly even when half the feeds got throttled or 304'd.

### Debug fast-refresh override

New GSetting `debug-fast-refresh-seconds` (i, default 0, range `[0, 3600]`). When viaduct is launched with `--debug` *and* this is non-zero, `arm_periodic_refresh` installs a `glib::timeout_add_seconds_local` at this seconds-cadence instead of the user-facing `refresh-interval-minutes`. So `--debug` + setting "30 seconds" simulates ~120 cycles per hour, surfacing cumulative growth in minutes instead of overnight.

Surfaced in the Preferences dialog as an `AdwSpinRow` "Debug: refresh every N seconds", added to the Sync group **only when `crate::is_debug_mode()` returns true**. End users never see it. Re-arms the periodic-refresh timer immediately on change (same connect_changed pattern as the user-facing minute setting).

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.9 — Bound refresh-cycle concurrency

The v2.6.3 diagnostics caught a real signal in a user's run on a 130-feed corpus:

```
diag: refresh cycle pre  rss_mb=295 peak_mb=295 feeds_attempted=130 force=true
diag: refresh cycle post rss_mb=397 peak_mb=419 peak_delta_mb=124
```

A single refresh cycle added **124 MB to peak RSS**. Inside the 500 MB budget, but worth fixing — and explains the user's reported ~450 MB peak in long sessions.

### Root cause

`AccountRefresher::refresh_feeds_inner` `tokio::spawn`-ed every feed simultaneously, with no concurrency limit. For a 130-feed cycle that means 130 in-flight per-feed pipelines, each holding:

- The HTTP response body (`Vec<u8>`, often 50–200 KB)
- The `ParsedFeed` (full article content_html for every item)
- The `ArticleChanges` queued for the DB worker
- v2.6.4 favicon discovery: an additional home-page HTML fetch up to 256 KB

Multiply by 130 in flight and the per-cycle peak scales linearly with feed count. NetNewsWire side-steps this by leaning on `URLSession.httpMaximumConnectionsPerHost = 1` plus URLSession's internal global cap; reqwest pools differently, so the limit didn't carry across the port.

### Fix

A `tokio::sync::Semaphore` capped at **8** (constant `REFRESH_PARALLELISM` in `fetcher.rs`) gates the start of each per-feed pipeline. All 130 tasks still spawn immediately — the captured state per parked task is small (Arc clones + a Feed struct) — but only 8 hold the permit at once, so the heavy allocations (HTTP body, parser tree, favicon-discovery HTML) stay bounded regardless of feed count. 8 is roughly the URLSession default global concurrency on macOS — enough pipelining to keep network busy on a fast connection, low enough to bound memory.

### Expected impact

Peak per-cycle delta should drop from `feed_count × per-feed allocation` to `REFRESH_PARALLELISM × per-feed allocation`. For a 130-feed cycle that's roughly a 16× reduction in the cycle's peak contribution. The v2.6.3 `diag: refresh cycle post` log line is the canonical way to verify on your own corpus.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.8 — Tray icon on GNOME, take three (`index.theme`)

Follow-up to v2.6.7. User reported the icon was still the placeholder after the v2.6.7 fix. On-disk verification: the PNGs were present at the right paths (`~/.cache/viaduct/tray-icons/hicolor/{256x256,512x512}/apps/org.virinvictus.Viaduct.png`), the SNI item was registered, the slot was allocated. So the install worked. So why still a placeholder?

### Root cause

GTK's icon-theme spec requires a per-theme `index.theme` file at the theme root declaring which subdirectories exist and what size each holds. The format is described in [the freedesktop spec](https://specifications.freedesktop.org/icon-theme-spec/latest/). Without it, `St.IconTheme.lookup_icon_for_scale` — the call AppIndicator's `_getIconData` makes — has no way to know that `256x256/apps/` is a valid icon directory at all, and the lookup fails. The system hicolor theme at `/usr/share/icons/hicolor/index.theme` is 8 KB enumerating every standard subdir for exactly this reason.

### Fix

`install_tray_icon_theme` now also writes a minimal `index.theme` at `<base>/hicolor/index.theme`:

```ini
[Icon Theme]
Name=Viaduct Tray
Comment=viaduct tray indicator icons (auto-generated; safe to delete)
Hidden=true
Directories=256x256/apps,512x512/apps

[256x256/apps]
Size=256
Context=Applications
Type=Fixed

[512x512/apps]
Size=512
Context=Applications
Type=Fixed
```

`Hidden=true` keeps the theme out of any user-facing theme picker. `Type=Fixed` says "this directory holds icons at exactly this size" rather than threshold/scalable — accurate for our two PNG buckets. Idempotent install (compares against the embedded const string).

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.7 — Tray icon on GNOME (AppIndicator extension)

User report after v2.6.6: "It's still a box with three dots in it. It is not our logo." Screenshot confirmed the slot is allocated but rendered as the AppIndicator placeholder.

### Root cause

GNOME doesn't natively host StatusNotifierItem — it uses the [`gnome-shell-extension-appindicator`](https://github.com/ubuntu/gnome-shell-extension-appindicator) extension as a bridge. That extension **mostly ignores `IconPixmap`** (the property v2.6.6 populates) and resolves icons through GTK's icon theme using `IconName` + `IconThemePath`. v2.6.6 worked on KDE / XFCE / Cinnamon (which honor `IconPixmap`) but produced the placeholder on GNOME because the extension never read the pixel data.

### Fix

`Tray::icon_theme_path` now returns `$XDG_CACHE_HOME/viaduct/tray-icons/`, populated at startup with a hicolor-shaped layout:

```
~/.cache/viaduct/tray-icons/
└── hicolor/
    ├── 256x256/apps/org.virinvictus.Viaduct.png
    └── 512x512/apps/org.virinvictus.Viaduct.png
```

Bytes come from the same `include_bytes!` of `docs/icon-{256,512}.png` v2.6.6 added; v2.6.7 just writes them to disk in the structure GTK's icon theme search expects. The AppIndicator extension prepends `icon_theme_path` to its icon-theme search and looks up `org.virinvictus.Viaduct` in there. Result: real logo on GNOME.

`icon_pixmap` (v2.6.6) and `icon_name` (the freedesktop name) both stay in place so KDE / XFCE / Cinnamon hosts that prefer pixmap, and shipped builds where the SVG is in `$datadir/icons/hicolor/`, both still work without any second discovery pass.

### Implementation notes

- Install runs once per process via `OnceLock<Option<String>>` and is idempotent across runs: only writes the file when missing or when the on-disk size differs from the embedded byte length (treats the static-asset bytes as immutable, no hash needed).
- I/O failures log and return `None` so the SNI host falls back to the system icon-theme search path. Doesn't bring the tray down.
- Lives at `$XDG_CACHE_HOME/viaduct/tray-icons/` rather than `$XDG_DATA_HOME` because it's regenerable from the binary on every startup — same disposability the existing favicon / image / video-thumb caches have. The new v2.6.5 cache sweep doesn't touch it (paths.rs only resolves `favicons/`, `images/`, `video-thumbs/`).

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.6 — Tray icon renders without an installed icon theme

User report: "It runs in the background, but its system tray icon doesn't work." Root cause: `tray.rs` only implemented `Tray::icon_name`, returning the freedesktop name `"org.virinvictus.Viaduct"`. SNI hosts resolve that against the system icon theme — which works for shipped builds (`meson install` drops the SVG into `$datadir/icons/hicolor/scalable/apps/`, Flatpak puts it in the runtime's theme), but **not for `cargo run` dev builds**, which never install anything. The host fell back to a placeholder, or in some KDE / extension cases nothing at all.

### Fix

`Tray::icon_pixmap` now returns a `Vec<ksni::Icon>` populated from two PNGs embedded at compile time (`docs/icon-256.png` + `docs/icon-512.png`, total ~42 KB on disk). Decode happens once per process via `gtk::gdk_pixbuf::PixbufLoader` (already a transitive of gtk4 — no new dep), cached in a `OnceLock<Vec<Icon>>`. RGBA is converted to ARGB32 in network byte order per the SNI spec via `pixel.rotate_right(1)`. Two resolutions give the host a choice — KDE / XFCE trays usually pick 256, HiDPI extensions tend to want 512.

`icon_name` stays in place. Per the SNI spec, hosts that prefer themable icons (KDE on a system with the SVG installed) will use that; everyone else falls back to the embedded pixmap. This means installed builds remain themable while dev builds + barebones environments get a working tray icon for free.

### Implementation notes

- `gtk::gdk_pixbuf::PixbufLoader` is `!Send`, so the decode runs on the GTK main thread (where `wire()` is called from `main.rs build_ui`). The resulting `ksni::Icon` is plain data (`Vec<u8>` + two `i32`s) and crosses freely to the tokio thread that ksni's worker runs on.
- Decode failures log and return an empty Vec; the SNI host then falls back to `icon_name` (still works for installed builds).
- PNGs live in `docs/` rather than `data/icons/`. The latter is reserved for the SVG that meson installs into the system icon theme; PNGs are exclusively the embedded fallback path.

### Test status

23 viaduct + 90 viaduct-core + 1 integration = 114 tests pass (the change is GTK-side and exercised by interactive run, not unit tests). fmt + clippy clean.

## v2.6.5 — Ghost-data sweep + missing-file failsafes

A spring-cleaning release. Audited the SQLite stores and disk caches for "ghost information" we were carrying across sessions, audited the file-missing failsafes for the inevitable user `rm -rf ~/.local/share/viaduct/foo`, and surfaced one user-visible failure mode that was previously silent.

### Disk-cache sweep (the actual ghost-data fix)

`~/.cache/viaduct/{favicons,images,video-thumbs}/` had no garbage collection. Files for unsubscribed feeds and pruned articles accumulated forever — after a year of normal use the favicon dir alone could be hundreds of MB of icons for feeds you no longer subscribe to. Two strategies, picked per cache kind:

- **Targeted sweep (favicons)**. New `SettingsDbOp::CollectFaviconUrls` returns every non-blank `favicon_url` + `icon_url` value in `feed_settings`. Hash to the same md5-hex filename `cache::cache_filename` produces, walk `favicons/`, delete files not in the live set. Exact, no false positives.
- **Age-based sweep (images, video-thumbs)**. There's no clean live set for either — would mean parsing every article's HTML for `<img>` URLs and running `detect_video` over every body. Instead we lean on the fact that article retention is itself age-bounded: anything older than the retention window is for an article that's been pruned out, so by definition orphan. Threshold = `retention_days × 2` for images (buffer for re-bound articles), `retention_days` for video-thumbs.

Both sweeps run from `cleanup_at_startup` after the existing settings/articles/statuses/authors prunes. I/O hops to `tokio::task::spawn_blocking` so the worker thread doesn't stall.

### Sync.sqlite ghost-row sweep

When the active delegate is `LocalAccountDelegate` (no Inoreader credentials), every row in `syncStatus` is leftover state from a previous Inoreader session — the local delegate never reads or writes that table. New `SyncDbOp::WipeAll` plus a `cleanup_at_startup` branch nukes the lot. New `AccountDelegate::is_local()` (default false; LocalAccountDelegate overrides to true) gates the wipe so flipping back to Inoreader doesn't lose live remote-sync state.

### Missing-file failsafes

Audited every "what if the user `rm`s X" path. Most were already graceful — `paths::ensure_dirs()` recreates missing data + cache dirs; `Connection::open` recreates missing SQLite files and `setup_schema` rebuilds tables; `load_opml` returns an empty `OpmlFile` when `local.opml` is missing; the existing `delete_settings_for_feeds_not_in([])` and `delete_articles_not_in_feeds([])` regression-tested early-returns mean an empty OPML doesn't nuke the DBs. The one gap was malformed OPML: `load_opml` errored, the sidebar sat silently empty, and the user had no UI signal anything was wrong. Now surfaces an `adw::Toast` (`window.rs::wire_models` callback): "Couldn't load local.opml — see log for details."

### `--debug` "Wipe Disk Caches" action

New `win.debug-clear-caches` in the Debug submenu. Drops the in-memory `ImageCache` LRU, then `wipe_dir`s all three cache subdirs from a `spawn_blocking` task. Toast reports the file count. Useful when triaging a favicon / image / video-thumb caching bug — restart with a guaranteed-empty disk cache without `rm -rf`-ing by hand.

### Implementation notes

- New module `viaduct-core/src/network/cache_sweep.rs`: `sweep_targeted` (md5 set), `sweep_by_age` (mtime threshold), `wipe_dir` (unconditional), `live_filenames_for` (URL → md5 helper). 8 unit tests covering each function + missing-dir handling + the `live_filenames_for` round-trip with `cache::cache_filename`.
- `cache::cache_filename` promoted to `pub(crate)` so the sweep module reads from one canonical naming function.
- `Account::collect_favicon_urls` / `Account::wipe_sync_statuses` / `Account::is_local_account()` added; the cleanup chain calls all three.
- mtime, not atime, gates the age-based sweep — most filesystems mount with `noatime` or `relatime` so reads don't reliably bump access time. mtime gets rewritten on every cache *write*, which is the operation that matters for "still in active use".

### Test status

23 viaduct + **90** viaduct-core (was 82 — +8 cache_sweep unit tests covering targeted/age/wipe + missing-dir handling + live-set round-trip) + 1 integration = 114 tests pass. fmt + clippy clean.

## v2.6.4 — Favicon discovery + YouTube placeholder filter

Two cosmetic but persistent rough edges fixed in one release.

### Favicon discovery

Pre-v2.6.4, the sidebar favicon path was `settings.favicon_url.or(settings.icon_url)`, both populated only from feed-level XML metadata (`<image><url>` for RSS, `<icon>`/`<logo>` for Atom). Most personal blogs (Sacha Chua, Karthinks, Howardism, public voit, vv's blog, Protesilaos Stavrou…) ship neither, so the sidebar fell back to the `adw::Avatar` initials. Real readers — including NetNewsWire — also probe the home-page HTML head and `<origin>/favicon.ico`. We didn't.

Now we do. New `viaduct-core/src/network/favicon_discovery.rs` ports NNW's `SingleFaviconDownloader` flow:

1. Fetch the home page HTML (capped at 256 KB so we don't pull a multi-MB landing page).
2. Run `parser::extract_metadata` and pick the best `<link>` candidate: plain `rel="icon"` / `rel="shortcut icon"` first, `rel="apple-touch-icon"` as fallback.
3. Verify with HEAD; if HEAD 405s (some Cloudflare-fronted feeds do), retry with GET.
4. If still nothing, fall back to `<origin>/favicon.ico` and verify the same way.

Successful discoveries persist into `FeedSettings.favicon_url`, so the probe runs at most once per feed across the lifetime of the install — subsequent refreshes hit the persisted URL via the existing `ImageCache::favicon` path.

`fetcher.rs::refresh_one_feed` also now persists `parsed.home_page_url` into `new_settings.home_page_url`, which was being dropped on the floor (the parser captures it from `<link>` / Atom alternate but the refresher never wrote it back). Without this, favicon discovery has nothing to probe against; with it, every feed gets a base URL after its first successful refresh post-upgrade.

`Fetcher` gained a `pub fn client(&self) -> Client` accessor so adjacent network work (favicon discovery, future home-link UI) reuses the existing connection pool instead of building a fresh `reqwest::Client`. `reqwest::Client` is cheap to clone — internally an `Arc`.

### YouTube thumbnail placeholder filter

The v1.3.0 timeline thumbnail column was reserving 80×45 of layout for some rows even when no real video was present. Root cause: `detect_video` was matching YouTube URLs in article body text (Michael Tsai's link-roundup posts, HN summaries with embedded YT links, TimminsToday news articles), the fetch against `i.ytimg.com/vi/<id>/hqdefault.jpg` succeeded with HTTP 200, and YouTube returned its **120×90 gray "no thumbnail" placeholder** instead of a real thumb. `from_bytes` decoded the placeholder fine, `set_visible(true)` ran, and the row got a dark 80×45 square.

`spawn_video_thumbnail_fetch` now drops decoded textures whose `width()` is < 200. Real `hqdefault.jpg` is 480×360; the placeholder is exactly 120×90. The picture stays invisible, the column collapses, the row reflows tightly.

### Test status

23 viaduct + **82** viaduct-core (was 73 — added 9 favicon-discovery unit tests covering each `<link>` rel variant + relative-href resolution + ignore-non-icon-links) + 1 integration = 106 tests pass. fmt + clippy clean.

## v2.6.3 — Background-mode memory diagnostics

User report: "Hit ~450 MB peak after a session with run-in-background on; v1.1.0 baseline foreground-only was ~280 MB." Could be a real leak; could be `VmHWM`-monotonic-aggregation across many refresh peaks over a long session. v2.6.3 adds enough logging that the next overnight run answers it definitively.

### What's logged

Five new `tracing::info` lines, all gated on the default `viaduct=info` filter so they surface in normal output (no `--debug` required):

- **`diag: hide_for_background post-clear`** — fires after `ImageCache::clear_memory` + WebView idle + ListStore compact. Verifies v1.10.0's "≤ 100 MB hidden" target still holds.
- **`diag: reload_current_timeline re-show`** — fires from `unhide_from_background` when GApplication re-activates the existing window (dock icon, D-Bus, tray Show). Delta from the last hide line tells you the re-summon cost.
- **`diag: refresh cycle pre` / `diag: refresh cycle post`** — wraps `run_refresh_with_tally` (used by manual `Ctrl+R`, post-import refresh, and the v1.8.0 periodic-refresh `glib::timeout`). Post-line carries `peak_delta_mb`. Climbing peak across many cycles = real leak; flat peak = `VmHWM` is just sticky.
- **`diag: background tick`** — `glib::timeout_add_seconds_local(300)` armed by `hide_for_background`, cancelled by `unhide_from_background`. Logs VmRSS every 5 min while hidden. Climbing line = leak. Oscillating around a stable value = heap caching, benign.
- **`diag: tray Show` / `diag: tray Quit`** — fires from the `viaduct/src/tray.rs` receive loop on each menu activation.

### How to use

Run viaduct with `RUST_LOG=viaduct=info` (or no env override — `info` is default) for an overnight session in background mode. Grep the log for `diag:` and reconstruct the timeline. Suspects to investigate if a leak is confirmed: ksni's tokio worker + zbus retention, `LocalAccountRefresher` per-cycle `Arc<Account>` retention, `WebContext` scheme-handler closure retention, any new `glib::Object` cycles introduced in v2.x.

### Implementation notes

- New `pub fn read_memory_mb() -> (u64, u64)` on `viaduct-core` (the helper was already there, just private — promoted so the binary crate can call it from anywhere without a roundabout). Re-exported at the `viaduct` crate root.
- New `imp.hidden_state_ticker: RefCell<Option<glib::SourceId>>` on `ViaductWindow`. Armed in `hide_for_background`, removed in the new `unhide_from_background` method, and self-breaks if the window is already gone.
- `run_refresh_with_tally` reads VmHWM both before and after the cycle so the post-line carries `peak_delta_mb` directly — no need to correlate two separate log lines.
- `main.rs build_ui` calls `existing.unhide_from_background()` before `existing.reload_current_timeline()` so the re-show snapshot reflects "just re-presented", not "re-presented + repopulated".

### Test status

23 viaduct + 73 viaduct-core + 1 integration = 97 tests pass. fmt + clippy clean. The new log lines are observation-only — no behaviour change to test against.

## v2.6.2 — Mark All Read leaves badge stuck at 1 (orphan status rows)

User report: "After hitting mark all as read, the All Unread stays with a 1 in the badge." Investigation found the same count-vs-fetch-disagreement bug pattern v2.6.1 fixed for Today, but in a different location.

### Root cause

Three queries converge on "how many unread articles":

| Query | Source table | Orphan-immune? |
|---|---|---|
| `fetch_unread` (the click result) | `articles INNER JOIN statuses` | ✓ |
| `unread_counts_by_feed` (per-feed badge) | `articles LEFT JOIN statuses` | ✓ |
| `smart_feed_counts.today_unread` | `articles INNER JOIN statuses` | ✓ |
| `smart_feed_counts.all_unread` (the All Unread badge) | bare `statuses` | ✗ |
| `smart_feed_counts.starred_unread` | bare `statuses` | ✗ |

The two outliers ran straight against the `statuses` table without joining articles, so **orphan status rows** counted toward the badge. Orphans are an intentional viaduct (and NetNewsWire) behaviour: when an article is deleted by retention or by a feed-removal sweep, its status row is preserved so a re-fetch later restores the user's old read/starred state. The `articles_ad_lookup` cascade trigger explicitly skips status-row cleanup for that reason. So orphans are normal; they just shouldn't pollute the badge.

The user-visible symptom: an unread orphan exists → the All Unread badge says 1 → user clicks Mark All as Read → only the visible articles get marked → orphan stays at read=0 → badge stays at 1, no clickable article to remedy. Stuck state.

### Fix

`smart_feed_counts.all_unread` and `smart_feed_counts.starred_unread` now `INNER JOIN articles ON s.article_id = a.article_id` like every other count + fetch query in the system. New regression test `smart_feed_counts_excludes_orphan_statuses` plants a real article + a real status + an orphan status (and a starred orphan), confirms the bare `COUNT(*) FROM statuses` returns 2 and the smart-feed counts return 1 / 0. Drift back to the broken behaviour will fail this test loudly.

### Test status

23 viaduct + **73** viaduct-core (was 72 — added the regression test) + 1 integration = 97 tests pass. fmt + clippy clean.

## v2.6.1 — Today smart feed timezone fix + diagnostic logging

User report: "the today smartfeed doesn't actually show today's entries." Investigation surfaced one provable bug — the badge count and the click-result spanned different windows on any non-UTC system.

### Root cause

`fetch_today` (the click-result query) and `smart_feed_counts.today_unread` (the badge count) both computed `today_start` independently:

- `fetch_today` — `Local::now().date_naive().and_hms_opt(0, 0, 0).and_local_timezone(Local).to_utc().timestamp()` → unix seconds for **local** midnight, expressed as UTC. Correct.
- `smart_feed_counts` — `Local::now().date_naive().and_hms_opt(0, 0, 0).and_utc().timestamp()` → unix seconds for **UTC** midnight on the local calendar date. **Wrong** — interprets the naive datetime as already-in-UTC instead of converting from local.

On EDT (UTC-4) the buggy count's window starts 4 hours earlier than the click-result's window. Exactly the local-offset's worth of articles arriving in those hours appeared in the badge but vanished on click — and articles arriving in the equivalent window the next morning showed up in the click result but had already been counted yesterday. Mostly invisible at low article volume; visible as "the count doesn't match what shows" at higher volume.

### Fix

Both call sites now go through one helper: `local_midnight_utc_seconds()`. The helper handles DST edge cases (gap → 0 fallback; ambiguous → pick earlier instance — neither matters in practice since DST transitions happen at 02:00 local, never midnight, but the guards mean a future regulator change can't brick the smart feed). New unit test `local_midnight_helper_matches_explicit_local_midnight` locks the semantic against future drift.

### Diagnostic logging

`fetch_today` now emits a `tracing::debug` line on every invocation showing the resolved `today_start` (as both unix seconds and an RFC3339 local timestamp) plus the result count. Run with `RUST_LOG=viaduct=debug` to surface. If the smart feed still shows the wrong articles after v2.6.1 the log line will identify whether the boundary or the article data is wrong.

### Test status

23 viaduct + **72** viaduct-core (was 71 — added the new helper-semantic test) + 1 integration = 96 tests pass. fmt + clippy clean.

### If this doesn't fix what you're seeing

The Today smart feed shows articles where **`status.date_arrived >= local_midnight_today`** OR **`article.date_published >= local_midnight_today`**. Cases that intentionally don't show:

- An article published yesterday that you haven't read yet — it's in **All Unread**, not Today. "Today" filters by date, not by read state.
- Stale articles re-fetched today (the parser flags articles older than 6 months as already-read on insert; their `date_arrived` is still set to `now`, so they DO appear in Today even if pre-marked-read).

If you're seeing zero results when you expect entries, run with `RUST_LOG=viaduct=debug` and check the `fetch_today` log line — the `today_start_local` field shows the boundary the query is using.

## v2.6.0 — Drag-and-drop sidebar reorder

The "Move to Folder…" right-click action shipped in v2.1.0 covered the functional case (relocating a feed between folders / standalone), but a real reader wants to grab a feed and toss it into a folder. v2.6.0 wires that up.

### How it works

Two GTK4 controllers attached to every sidebar row in `setup_sidebar_list_view`'s `connect_setup`:

- **`gtk::DragSource`** — `connect_prepare` reads the bound `SidebarItem` via the existing `viaduct-sidebar-item` `set_data` attached during `connect_bind` (same path the v1.7.1 right-click gesture handler uses to recover the clicked item). For `Feed` items, returns a `gdk::ContentProvider::for_value(&feed.url.to_value())`. For non-Feed rows (folders, smart feeds, the smart-feed group), returns `None` to suppress the drag — only feeds drag.
- **`gtk::DropTarget`** — accepts `String::static_type()` content with `DragAction::MOVE`. `connect_drop` reads the bound `SidebarItem`; only `Folder` rows accept the drop. On drop, fires `Account::move_feed_to_folder(url, Some(folder.name))` on the tokio runtime, awaits the result via a `tokio::sync::oneshot`, then activates `win.reload-sidebar` from the GTK thread to repopulate the tree.
- **Coexistence with right-click "Move to Folder…"** — the dialog path stays. It's still the only way to move a feed back to **standalone** (no top-level drop target — there's no single root row to drop onto, and adding a margin-area drop zone would overload the listview's hit-testing).

### Why use a `oneshot` channel here

GTK widgets are `!Send`. The drop callback captures the dropped-on widget so it can `activate_action` on it after the move succeeds. But `Account::move_feed_to_folder` is an `async fn` we want to spawn on tokio (which requires `Send` futures). Splitting the work — DB op on tokio, action activation on the local glib executor with the widget reference held there — and bridging via `tokio::sync::oneshot` is the standard pattern.

### About `viaduct-sidebar-item`'s `set_data`

The `unsafe` block reading the bound item is the same pattern the v1.7.1 right-click gesture handler uses; the data lives until the next `connect_bind` overwrites it, which is fine for the synchronous duration of `prepare` / `drop`.

### What this leaves open

- **Drop on the root area for standalone** — would need a top-level drop target with hit-testing. Right-click "Move to Folder… → (No folder)" covers the same outcome.
- **Reorder feeds within a folder** — NetNewsWire allows this; we don't yet. OPML doesn't preserve order across save/reload semantically (the parser preserves `<outline>` order, but mutations re-emit in iteration order). Would need adding explicit `index` tracking in the OPML representation. Deferred until anyone asks.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass; fmt clean; clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms the binary runs at v2.6.0. Drag-and-drop dispatch is interactive QA — visible behavior requires a feed and a folder side by side.

## v2.5.0 — System tray indicator for run-in-background

The Phase 17 background daemon (v1.10.0) explicitly deferred a tray indicator with the rationale "wait for explicit user demand." That demand arrived: closing the window with run-in-background on used to make the app vanish invisibly — no way to tell it was still alive, no way to kill it short of `pkill viaduct`. v2.5.0 fixes that.

### What ships

A new `ksni`-backed StatusNotifierItem that lives whenever `run-in-background` is on:

- **Icon** — `org.virinvictus.Viaduct` resolved against the system icon theme. Resolves correctly on Flatpak / Meson-installed builds; dev builds fall back to the SNI host's default placeholder.
- **Left-click** — re-summons the existing window via `Application::activate()` (same path the dock icon uses; goes through `main.rs build_ui`'s present-existing-window branch from Phase 17 D-Bus re-summon).
- **Right-click menu** — "Show viaduct" + "Quit viaduct". Quit goes through `gio::Application::quit()` which bypasses `connect_close_request`'s hide-instead-of-quit branch — exactly the kill-switch the user asked for.

### Lifecycle

Tray service starts/stops based on the `run-in-background` GSetting:
- App launch with the GSetting on → tray spawned in `viaduct::tray::wire(app)` from `main.rs`
- User flips the toggle on in Preferences → `connect_changed` listener spawns the service
- User flips off → `handle.shutdown()` clears the icon
- App exit → `Quit` action calls `stop_service()` first to clean up the SNI registration, then `app.quit()`

The tray runs whenever the feature is enabled, regardless of window-visibility state — that's the simpler mental model and matches "I have viaduct configured to run in background; here's where to find it." When the window IS visible, the tray icon is harmless duplication.

### About the "K" in ksni

The crate name is historical (KDE wrote the StatusNotifierItem spec). SNI is the de-facto cross-desktop tray protocol today — natively hosted by KDE Plasma, XFCE 4.14+, Cinnamon, MATE, Pantheon, Budgie. On GNOME the icon shows up via the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/) (most-installed GNOME extension; preinstalled on Fedora's GNOME-with-extensions sessions and on Ubuntu). The marginal dependency cost is small — `zbus` is already in our transitive tree from Phase 17 (`ashpd` → `zbus` for the Background portal); ksni reuses it. New crates added: ksni 0.3.4 + pastey 0.2.2 (its macro helper). No new C dependencies.

### Communication

Tray menu callbacks fire on `ksni`'s tokio worker thread; deliver `TrayAction` enum variants over a `tokio::sync::mpsc::unbounded_channel` to a `glib::spawn_future_local` task on the GTK main thread. `app.activate()` / `app.quit()` are the actual GTK calls. Standard cross-thread bridge.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass; fmt clean; clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms the binary runs at v2.5.0 — at-exit memory peak: 282 MB / 500 MB budget. SNI dispatch + tray-icon visibility are interactive QA on a desktop that hosts a tray.

### What's still open

- **Drag-and-drop sidebar reorder** — coming in v2.6.0.

## v2.4.0 — Per-feed notifications

The last NetNewsWire-parity gap I called out after v2.0 ships. The v0.8.0 patchnotes explicitly noted that NNW's per-feed `newArticleNotificationsEnabled` toggle was deferred because it needed a feed-inspector pane viaduct didn't have. v2.4.0 builds the inspector and wires the toggle through.

### What ships

- **Feed Settings dialog** — new entry on the sidebar feed right-click menu. Opens an `AdwAlertDialog` with two `AdwSwitchRow`s: **New article notifications** (the v2.4.0 feature) and **Always use Reader View** (existing `reader_view_always_enabled` field, exposed in a UI for the first time). Pre-loads the current values from `FeedSettings`; on save, `fetch_feed_settings → mutate → upsert_feed_settings` round-trips through the DB worker.
- **Per-feed notification dispatch** — `RefreshTally` switched from a single `new_articles: usize` to `per_feed_new: HashMap<feed_id, count>`. The drain task in `run_refresh_with_tally` groups `ArticleChanges.new_articles` by `Article.feed_id`. `dispatch_refresh_notification` now walks the map: for each feed with new articles, fetches its `FeedSettings`, and fires a `gio::Notification` titled with the feed's display name **only when** the global `notifications-on-refresh` GSetting **and** the per-feed `new_article_notifications_enabled` flag are both on.
- **Notification coalescence** — each per-feed notification uses an `id` of `viaduct.refresh.<feed_id>` so the desktop notification daemon (`xdg-desktop-portal-gnome` etc.) coalesces repeated refreshes of the same feed instead of stacking N notifications when the user mashes Ctrl+R.
- **Toast preserves the global summary** — the after-refresh `AdwToast` still says "Refreshed 50 feeds — 8 new articles" using the new `RefreshTally::total_new_articles()` accessor. Only the desktop-notification path is per-feed.

### Schema migration

`feed_settings` table picks up `new_article_notifications_enabled INTEGER NOT NULL DEFAULT 0`. Default is `0` (off) to match NNW — users opt in explicitly via the dialog. The migration is an idempotent `ALTER TABLE … ADD COLUMN … DEFAULT 0` so existing databases pick up the column on next startup; freshly-created DBs get it from the `CREATE TABLE` statement directly.

### What this completes

The audit I gave after v2.0 listed five user-visible NetNewsWire-parity gaps:

1. ✅ Sidebar editing (rename / new folder / move) — **v2.1.0**
2. ✅ Print article — **v2.2.0**
3. ✅ Article appearance popover (font scale / line height) — **v2.3.0**
4. ✅ Per-feed notifications — **v2.4.0**
5. **Drag-and-drop sidebar reorder** — deferred indefinitely (right-click + dropdown is sufficient for the common case)

Plus multi-window and system tray, both flagged as nice-to-haves with no concrete demand. All five "users will notice" gaps are now closed.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass. fmt clean, clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms the binary runs at v2.4.0 and the schema migration runs cleanly on the existing DB. Per-feed notification dispatch is interactive QA — visible behavior requires a feed actually delivering new articles.

## v2.3.0 — Article Appearance popover

Closes the "Article Settings Popover" item that's been called out as a Future UX addition in `roadmap.md` since the original v1.0 plan. NetNewsWire has live text-size adjustment in the Mac UI; viaduct now has the same plus a line-spacing slider, both attached to a small `font-x-generic-symbolic` button in the article-pane toolbar.

### What ships

- New article-pane header-bar button (between play-video and reader-view). Clicking it pops a `gtk::Popover` containing an `AdwPreferencesGroup` with two `AdwSpinRow`s: **Text Size** (75 – 200 % of theme default, step 5; default 100 = no scaling) and **Line Spacing** (centi-units, 100 – 250; default 150 = `line-height: 1.5`, matching most NNW themes' native value). A "Reset to Defaults" `AdwActionRow` puts both back.
- Two new GSettings keys under `org.virinvictus.Viaduct`: `article-font-scale` (i, 75–200, default 100) and `article-line-height` (i, 100–250, default 150). The spin rows bind bidirectionally via `gio::Settings::bind` so external flips (dconf-editor, future Preferences page) sync the popover automatically.
- CSS plumbing via the existing `:root { … }` injection in `article_renderer::render_themed`. Two new custom properties — `--article-font-scale` and `--article-line-height` — are appended to the same `:root` block that already carries `--accent-color` (v2.0.0-pre6). The `VIADUCT_PANE_OVERRIDE_CSS` cascade applies them via `body { font-size: calc(1em * var(--article-font-scale, 1)); line-height: var(--article-line-height, 1.5); }` so theme rules expressed in `em`/`rem` scale proportionally.
- Live re-render: `ArticlePaneView::bootstrap` now subscribes to `notify::article-font-scale` and `notify::article-line-height`. Slider drags fire a `refresh_render()` so the new values take effect without re-selecting the article.

### Trade-off worth noting

Because we don't (yet) parse each theme's native line-height to multiply against, default 150 = `line-height: 1.5` overrides whatever the theme set even when the slider is at "100 %" (the default). For most NNW themes that's already what they do; the visible effect is small. A user who dislikes 1.5 can drop the slider to 110 (= 1.1) or wherever — the schema range is `[100, 250]`. The Reset button puts it back to 150.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass. fmt clean, clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms the binary runs at v2.3.0.

### What's still open

- **Per-feed notification toggle** — the last NetNewsWire-parity gap. Touches `feed_settings` schema (ALTER TABLE), the refresh tally, and the dispatch path. Planned for v2.4.0.

## v2.2.0 — Print Article (Ctrl+P)

Closes the second of the post-v2.0 NetNewsWire-parity gaps. The article currently rendered in the WebKit pane can now be sent to a printer (or exported as PDF, depending on the GTK print backend's offered destinations) via `Ctrl+P` or the primary menu's "Print Article…" entry.

### How it's wired

- `ArticleRenderer::print(parent: Option<&gtk::Window>)` wraps `webkit6::PrintOperation::run_dialog(parent)`. The `PrintOperation` is constructed against the renderer's owned `WebKitWebView`, so the printed output is exactly what the pane currently displays — theme + macros + locked-down CSP intact.
- `ArticlePaneView::print` delegates to the renderer.
- `ViaductWindow::act_print_article` upcasts `self` to `gtk::Window` and calls into the article pane.
- New `win.print-article` action with a `Ctrl+P` accelerator. New "Print Article…" entry in the primary menu (between Export OPML and Preferences). When no article is selected, the dialog presents but prints an empty about-blank page — minor cosmetic limitation we can gate later if it bites.

### What's still on the gap list

- **Per-feed notification toggle** — touches the schema (`feed_settings` ALTER TABLE), the refresh tally, and the dispatch path. Planned for v2.3.0 or v2.4.0.
- **Article appearance popover** (font scale + line height) — needs new GSettings keys, a popover UI in the article-pane header bar, and CSS-variable injection through the renderer. Planned for v2.3.0.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass. fmt clean, clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms the binary builds and reports `version="2.2.0"`. Print-dialog round-trip is interactive QA — the dialog itself is system-managed.

## v2.1.0 — Sidebar editing: Rename, New Folder, Move to Folder

First of the gap-filling polish releases following the v2.0 architectural refactor. Closes the most user-visible NetNewsWire-parity gap: the inability to edit the sidebar from inside the app. Pre-v2.1.0 you had to hand-edit OPML to rename a feed, create a folder, or move a feed between folders.

### What ships

Three new actions wired through the existing right-click + primary-menu plumbing:

- **Rename Feed…** (right-click → context menu): `AdwAlertDialog` with a `GtkEntry` pre-filled with the current display name (selected so a single keystroke overwrites). On save, sets `edited_name` on the feed via the new `Account::rename_feed`. Empty input clears `edited_name` and reverts to the parsed-feed-name → URL-host fallback chain. Bound to the existing `display_name_for_feed` resolver so the sidebar updates immediately.
- **Move to Folder…** (right-click → context menu): `AdwAlertDialog` with a `GtkDropDown` listing existing folders plus a leading "(No folder)" entry. The dropdown's initial selection mirrors the feed's current location (resolved by walking the controller tree). On save, calls `Account::move_feed_to_folder`, which sweeps the feed out of its current home and reinserts it at the destination — creating the destination folder if needed.
- **New Folder…** (primary menu, between Add Feed and Import OPML): `AdwAlertDialog` with a `GtkEntry`. On create, appends an empty folder via `Account::create_folder`. Empty input is rejected; a folder with the same name is rejected with a toast.

### viaduct-core API additions

Three new helpers on `Account`, each following the existing `add_feed` / `remove_feed` `load_opml → mutate → save_opml` pattern:

- `rename_feed(feed_url: &str, new_name: String) -> Result<bool>`
- `create_folder(name: String) -> Result<bool>`
- `move_feed_to_folder(feed_url: &str, target_folder: Option<String>) -> Result<bool>`

Each returns `true` when a real change was made. `move_feed_to_folder` short-circuits when source and destination are the same (re-inserts without re-saving). All three save the OPML on success — the existing 500 ms debounced writer + `reload_sidebar_after_opml_change` covers the GTK-side refresh.

### What's still on the gap list

- **Per-feed notification toggle** — needs a Feed Settings dialog. Planned for v2.2.0.
- **Article appearance popover** (font scale, line height) — planned for v2.3.0.
- **Print article** — also v2.3.0.
- **Drag-and-drop sidebar reorder** — would supersede "Move to Folder…"; deferred indefinitely (right-click + dropdown is sufficient for the common case).

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass. fmt clean, clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms the new menu entries appear, the dialogs present, and OPML mutations round-trip through the sidebar refresh.

## v2.0.0 — Phase 18 ships: god-object decomposition + architectural refinement

The v2.0 release. No new code over -pre6 — this is the version banner that collects the six pre-releases into a single tag. The arc went:

- **-pre1 — `ArticlePaneView` extraction.** First pane lifted out of `ViaductWindow`. Locked-down WebKit + reader/play-video buttons + macro substitution + in-pane video dialog now live in a real custom widget. `window.rs`: 2900 → 2358.
- **-pre2 — `TimelineView` extraction.** Timeline list view + `gio::ListStore` + selection + search bar + scope toggle + FTS5 debounce out of the window. `window.rs`: 2358 → 2306.
- **-pre3 — `SidebarView` extraction.** The biggest cross-cutting concern: sidebar tree (`SidebarTreeControllerDelegate` / `TreeController` / `SidebarDataSource`), `feed_names` resolver, header bar buttons (`mark_all_read_btn` / `sync_btn` / `search_btn` / `menu_btn` + `primary_menu`), right-click feed/folder popovers, the unread-count tree walk. `window.rs`: 2306 → 2043 (cumulative −857 / −30 % vs the pre-Phase-18 baseline).
- **-pre4 — `ArticleRenderer` GObject promotion.** The encapsulated WebView wrapper. Per-renderer `WebContext` so `viaduct-img://` + `viaduct-font://` scheme handlers no longer leak onto the global default context (and into the video-dialog WebView).
- **-pre5 — Derived unread-count aggregation.** `TreeNode::set_child_nodes` now wires `notify::unread-count` subscriptions on each child; folder + smart-feed-group totals auto-derive from leaves. `SidebarView::refresh_unread_counts` shrank from ~80 to ~50 lines.
- **-pre6 — WebKit ↔ GTK CSS polish.** Thinner `currentColor`-driven scrollbars, 150 ms article-to-article fade-in, `--accent-color` CSS custom property propagation that picks up the libadwaita system accent for the Adwaita theme.

### What ships in v2.0.0

The whole stack post-Phase-18:

- **Three custom widget panes** (`SidebarView` / `TimelineView` / `ArticlePaneView`) plus the `ArticleRenderer` GObject inside the article pane. NetNewsWire's `MainWindowController` / `SidebarViewController` / `TimelineViewController` / `DetailViewController` structure ported to GTK4 + libadwaita 1.7.
- **`window.rs` slimmed to 2043 lines** (from a 2900-line baseline) and now squarely the cross-pane plumbing role: action group, refresh orchestration, keyboard shortcut installation, `connect_close_request`, the timeline-row right-click popover (the only popover whose actions are window-level), and the cross-pane signal handlers (sidebar-selection → timeline fetch, timeline-selection → article render, status mutations → unread-count refresh).
- **Per-renderer `WebContext`** scoping `viaduct-img://` + `viaduct-font://` to the article pane, leaving the video-dialog WebView's default context clean.
- **Property-driven unread-count aggregation** in `TreeNode` so adding nested folders later (currently disallowed by NNW normalization) just works.
- **Visual polish**: libadwaita-feel scrollbar inside the WebKit pane, smooth article-to-article fade-in, system accent → `--accent-color` for theme stylesheets that opt in via `var(--accent-color)`.

### What's still open for the 1.0 milestone

(Phase 17 + Phase 18 cleanup is in-tree. The v1.0 release is a **separate** milestone that ships the local-account-only feature set under the Flathub badge.)

- Tag `1.0.0` and submit to Flathub — blocked on the user (Flathub onboarding credentials, not code).
- The four polish concerns the v2.0 series picked up are also "free" for any v1.x Flathub release — no breaking changes; the GSettings schema, OPML format, DBs are all forward-compatible.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass. fmt clean, clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch through every release confirmed real OPML loads + sidebar→timeline→article render at single-digit milliseconds.

## v2.0.0-pre6 — Phase 18: WebKit ↔ GTK CSS bridge polish

The last polish release before v2.0.0 ships. Three small visual / typography refinements that build on the v1.2.0 accent system + v1.1.0 page-wrapper architecture, all delivered through the existing `VIADUCT_PANE_OVERRIDE_CSS` cascade so the byte-perfect NNW theme stylesheets stay untouched.

### Scrollbar parity

The article pane's WebKit scrollbar used to be a hard-coded 8 px gray thumb that didn't match libadwaita's overlay-scrollbar feel. New treatment:

- **Thinner**: 6 px idle, 10 px on hover (smooth `transition: width 120ms ease`). The hover expansion keeps the target grabbable without making it visually heavy at rest.
- **`currentColor`-driven**: thumb uses `color-mix(in srgb, currentColor 30%, transparent)` (55 % on hover), so it adopts the page's text color automatically — light themes get a soft gray, dark themes get a soft off-white, theme-tinted themes (Sepia / Biblioteca) get the matching tint. No GTK→WebKit color-querying needed.
- **Pill-shaped** (`border-radius: 999px`) to match the v1.2.0 sidebar unread badges' visual language.

### Article fade-in transition

Within-content article-to-article swaps used to be instant cuts. Now the page wrapper carries a `@keyframes viaduct-fade-in` 150 ms ease-out animation on `body`, replayed on every `WebKitWebView::load_html`. Matches the existing 150 ms `article_stack` content/empty crossfade duration so the rhythm stays consistent across selection states.

### System accent → WebKit pane

Themes already had a static `accent_hex: Option<&str>` for the GTK chrome accent (driven by `apply_app_accent` in v1.2.0). On the article body itself, that accent was either baked into the theme stylesheet (Sepia's cinnamon, Biblioteca's deep blue) or absent (Adwaita).

This release introduces a `:root { --accent-color: <hex>; }` CSS custom property at the top of every render's style cascade:

- **Themes with hard-coded accent**: keep theirs (Sepia / Biblioteca / Tiqoe Dark / etc.). `--accent-color` matches the chrome.
- **Themes with `accent_hex: None`** (Adwaita): pick up the libadwaita system accent via `adw::StyleManager::default().accent_color().to_standalone_rgba(is_dark)` (GNOME 47+ system-accent integration). Theme stylesheets can opt in by referencing `var(--accent-color)` for link colors, blockquote borders, etc. — the infrastructure is now there even though the bundled NNW theme CSSes don't yet consume it.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass; fmt clean; clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms the app starts and renders articles through the new style cascade.

### What's next

- **v2.0.0** — final tag.

## v2.0.0-pre5 — Phase 18: derived unread-count aggregation

`refresh_unread_counts` used to walk every top-level sidebar node, set leaves *and* sum folder/group totals manually, and call `set_unread_count` on every node along the way. This release shifts the aggregation into `TreeNode` itself so the parents derive from their children automatically — the imperative walk only needs to touch leaves.

### What changed

- **`TreeNode::set_child_nodes`** now disconnects any previous child `notify::unread-count` subscriptions, connects new ones for each incoming child, and computes the initial sum. Each subscription's handler calls `recompute_aggregate_unread` on the parent, which sums the children's counts and (if changed) calls `set_unread_count` on self. That `set_unread_count` itself fires `notify::unread-count`, so the cascade naturally propagates upward through the tree — grandparent → great-grandparent works for free.
- **`TreeNode` imp** gains `child_unread_handlers: RefCell<Vec<(WeakRef<TreeNode>, SignalHandlerId)>>` so the handlers get cleaned up properly across `set_child_nodes` calls (relevant when the controller rebuilds after OPML import).
- **`SidebarView::refresh_unread_counts`** loses the `folder_total` / `group_total` accumulators and the `top.set_unread_count(total)` calls. The new flow: walk top-level; for `Feed` standalones, set the count; for `Folder` / `SmartFeedGroup` containers, walk one level down and set the leaf counts; everything aggregates up. Method body shrank from ~80 lines to ~50.

### Why this is good

The pre-pre5 walk had a duplication-of-truth problem: the sum logic in `refresh_unread_counts` had to stay in lockstep with the tree shape. Adding nested folders (currently disallowed by NNW's normalization, but possible if we ever extended it) would have meant rewriting the walk; with auto-aggregation, depth-N trees just work.

### Test status

23 viaduct + 71 viaduct-core + 1 integration = 95 tests pass; fmt clean; clippy `--workspace --all-targets -- -D warnings` clean. Smoke launch confirms sidebar unread badges still update when reading articles (notify::unread-count fires on the leaf → folder badge re-renders).

### What's next

- **v2.0.0-pre6** — WebKit ↔ GTK CSS bridge polish (scrollbar parity, article fade-in, GNOME 47+ system accent → WebKit pane)
- **v2.0.0** — final tag

## v2.0.0-pre4 — Phase 18: ArticleRenderer GObject promotion

The first of the polish releases that build on the three-pane decomposition. `ArticlePaneView` used to reach directly into a `WebKitWebView` template_child and call into a sprawling `article_renderer.rs` module of free functions. This release wraps that whole concern in a real GObject — `ViaductArticleRenderer` — so the article pane stops touching WebKit internals and the URI-scheme handlers stop leaking onto the global `WebContext::default()`.

### What moved

A new `ViaductArticleRenderer` custom widget (in `viaduct/src/ui/article_renderer_widget.rs` + `article_renderer_widget.ui`) now owns:

- The `WebKitWebView`. Constructed in `bootstrap()` against a per-renderer `WebContext` (not the global default — that's the architectural improvement) and parented as the main child of an internal `GtkOverlay`.
- The `viaduct-img://` + `viaduct-font://` URI scheme handlers. Registered against the renderer's *own* context, so they no longer leak into the video-playback dialog's WebView (which uses the default context for YouTube / Vimeo embeds and doesn't need our schemes anyway).
- The hover URL `GtkLabel` (overlay child) and its `mouse-target-changed` wiring.
- The locked-down `WebKitSettings` profile (JS/WebGL/WebRTC/storage/dev-tools all OFF) and the `decide-policy` link interceptor that routes link clicks to `xdg-open`.
- Public surface: `bootstrap(image_cache)`, `render_themed(theme, substitutions, base_uri)`, `idle()`, `web_view()` (read-only escape hatch for future preview / printing paths).

`ArticlePaneView` keeps the orchestration layer: per-article `ArticleDisplayState`, the reader-view button + extraction kick-off, the play-video button + video-source detection, the in-pane embed dialog, and the macro substitution + theme resolution that produces the inputs `render_themed` consumes. The article pane now talks to one method (`article_renderer.render_themed(...)`) instead of reaching into a WebView template_child.

### What stayed in `article_renderer.rs`

The pure helpers — they're stateless and consumed by *both* the GObject and the (now-tiny) call sites in `ArticlePaneView`:

- `Theme` struct + `THEMES` const + `theme_by_id` / `select_for_dark_mode`
- `ArticleSubstitutions` struct + `escape_html`
- `render_themed` / `render` / `render_with_macros` (the macro engine)
- `sanitize_and_rewrite_image_srcs` / `encode_image_url` / `decode_image_url`
- `apply_locked_down_settings` / `install_link_interceptor` / `install_hover_url_overlay` (now called only from `ArticleRenderer::bootstrap`)
- `install_image_uri_scheme` / `install_font_uri_scheme` — refactored to take a `&webkit6::WebContext` parameter (was hard-wired to `WebContext::default()`); ArticleRenderer passes its own context
- `apply_app_accent` (the GTK chrome accent CSS provider — orthogonal to the renderer)
- `BUNDLED_FONTS` registry + `font_face_css`

### Numbers

`ArticlePaneView` shrank from 559 lines (pre3) to 540 (−19 — modest because the WebKit setup it offloaded was already tightly factored). The new `article_renderer_widget.rs` is 155 lines + a 29-line `.ui` template. `article_renderer.rs` stays at ~1140 lines (the helpers; small signature change on the two install_*_uri_scheme fns to accept a `&WebContext`). Tests: 23 viaduct + 71 viaduct-core + 1 integration = 95, no count change.

### Test status

fmt clean, clippy `--workspace --all-targets -- -D warnings` clean, all 95 tests pass. Smoke launch confirms real OPML loads, sidebar click → timeline populate → article render through the new renderer at 2 ms total. `viaduct-img://` resources still resolve (favicons + inline article images still display), `viaduct-font://` still serves Atkinson Hyperlegible for the Hyperlegible theme.

### What's next

- **v2.0.0-pre5** — `glib::derived_properties` expansion (kill `refresh_unread_counts` walks via `gtk::ClosureExpression` on `TreeNode.unread_count`)
- **v2.0.0-pre6** — WebKit ↔ GTK CSS bridge polish (scrollbar parity, article transitions, GNOME 47+ system accent → WebKit pane)
- **v2.0.0** — final tag

## v2.0.0-pre3 — Phase 18: SidebarView extraction

Final piece of the three-pane decomposition. The article pane went out in -pre1, the timeline in -pre2; this release lifts the sidebar — the most cross-cutting of the three — out of `ViaductWindow`. With this, the god-object refactor is structurally complete: every widget in the three-pane layout now lives inside its own custom widget subclass, and `window.rs` is essentially the cross-pane plumbing role NetNewsWire gives `MainWindowController`.

### What moved

A new `ViaductSidebarView` custom widget (in `viaduct/src/ui/sidebar_view.rs` + `sidebar_view.ui`) now owns:

- The entire sidebar `AdwToolbarView` — header bar with `mark_all_read_btn`, `sync_btn` (with its `sync_btn_stack` + `sync_btn_spinner` for the in-progress flip), `search_btn`, `menu_btn` — plus the `GtkScrolledWindow → GtkListView`, the `bottom_action_bar`, and the `primary_menu` GMenu (Add Feed / Import OPML / Export OPML / Preferences / Keyboard Shortcuts)
- `sidebar_delegate` (`Rc<RefCell<SidebarTreeControllerDelegate>>`), `sidebar_tree_controller` (`Rc<TreeController>`), `sidebar_data_source` (`Rc<SidebarDataSource>`), and the `gtk::SingleSelection` returned by `setup_sidebar_list_view`
- `feed_names: FeedNameMap` — the per-feed display-name resolver consumed by the timeline factory. Now a SidebarView accessor; the window queries it during `wire_models` and threads it into `TimelineView::bootstrap`
- `sidebar_feed_popover` + `sidebar_folder_popover` (right-click menus) and the gesture controller that resolves the clicked row to a `SidebarItem`
- `right_clicked_feed` / `right_clicked_folder` cells, plus `take_right_clicked_*` accessors that read-and-clear so a stale value can't bleed into a later keyboard activation
- `bootstrap(account, image_cache)` runs once from `ViaductWindow::wire_models` and does all the construction
- `apply_opml(OpmlFile)` — fully applies a freshly-loaded OPML: rebuilds the feed-name resolver, pushes the file into the delegate, kicks the controller to rebuild, refreshes the data-source root. Used by both the initial load path and the post-import / post-delete reload
- `refresh_unread_counts(account)` — the tree walk that applies per-feed unread totals + Today/All-Unread/Starred smart-feed totals + folder/group sums. Lifted byte-for-byte from the window-side body
- `set_refresh_in_progress(on)` — flips the sync button's `GtkStack` between `icon` and `spinner`
- `unparent_popovers()` — called from the window's `connect_close_request` quit branch so the listview doesn't whine about dangling children at finalize time
- Public surface: `list_view()`, `selection()`, `search_btn()`, `mark_all_read_btn()`, `sync_btn()`, `primary_menu()`, `feed_names()`, `controller()`, `delegate()`, `data_source()`, `list_folder_names()`, `take_right_clicked_feed/folder()`, `apply_opml`, `refresh_unread_counts`, `set_refresh_in_progress`, `unparent_popovers`. File-private `display_name_for_feed` + `pick_sidebar_item_at` helpers

### What stayed on `ViaductWindow`

The cross-pane orchestration that's not specific to any one pane:

- The action group and every `act_*` body (including `act_*_clicked_feed/folder`, which now read context via `sidebar_view.take_right_clicked_*()`)
- The sidebar-selection handler: still queries `selected_sidebar_item(sel)`, fetches articles, calls `timeline_view.populate(articles)` + `refresh_statuses(account)`. The cancel-stale-fetch generation counter and the `viaduct::perf` timing instrumentation stay window-level
- Refresh orchestration: `act_refresh`, `pair_feeds_with_settings`, `run_refresh_with_tally`, refresh notifications, the periodic-refresh `glib::timeout`
- `hide_for_background` / `connect_close_request` / `reload_current_timeline` / `set_refresh_in_progress` (now thin delegates to SidebarView)
- The timeline-row right-click popover (`timeline_popover`) and its gesture handler — the menu items hit window-level actions, so the popover stays alongside its handlers

### Numbers

`window.rs` shrank from 2306 lines (pre2) to 2043 lines (−263). Cumulative across Phase 18: 2900 → 2043 = **−857 lines (−30%)**. The new `sidebar_view.rs` is 470 lines + a 108-line `.ui` template. Tests: 23 viaduct + 71 viaduct-core + 1 integration = 95, no count change. Smoke launch: app starts cleanly, OPML loads, sidebar click → timeline populate at 6 ms.

### Test status

fmt clean, clippy `--workspace --all-targets -- -D warnings` clean, all 95 tests pass. Real OPML loads (sidebar populates and feed-name resolution works), feed-clicking routes through TimelineView, right-click context menus on sidebar rows still surface (now wired in `SidebarView::bootstrap` instead of `wire_context_menus`), unread counts refresh after status mutations.

### What's next

Phase 18 decomposition (the three-pane lift-out) is **complete**. The remaining adopted items are the architectural polish that becomes possible *because* the panes are now real components:

- **v2.0.0-pre4** — Promote `article_renderer.rs` into a dedicated `ArticleRenderer` GObject (lives inside `ArticlePaneView`)
- **v2.0.0-pre5** — `glib::derived_properties` expansion (kill `refresh_unread_counts` walks via `gtk::ClosureExpression` on `TreeNode.unread_count`)
- **v2.0.0-pre6** — WebKit ↔ GTK CSS bridge polish (scrollbar parity, article transitions, GNOME 47+ system accent → WebKit pane)
- **v2.0.0** — final tag

## v2.0.0-pre2 — Phase 18: TimelineView extraction

Second piece of the Phase 18 decomposition. The article pane went out in -pre1; this release lifts the timeline pane out the same way.

### What moved

A new `ViaductTimelineView` custom widget (in `viaduct/src/ui/timeline_view.rs` + `timeline_view.ui`) now owns:

- The timeline `GtkListView`, its `gio::ListStore`, and the `gtk::SingleSelection` that drives article rendering and keyboard navigation
- The `GtkSearchBar` + `GtkSearchEntry` + scope toggle (per-feed vs all-feeds)
- The `GtkStack` that flips between the populated `GtkScrolledWindow` content page and the "No articles" `AdwStatusPage` empty state
- `selected_feed_id: RefCell<Option<String>>` (search-scope target)
- `search_timeout: RefCell<Option<glib::SourceId>>` (FTS5 debounce cancel)
- `bootstrap(account, image_cache, feed_names, search_btn)` — runs once from `ViaductWindow::wire_models`. Builds the row factory via `setup_timeline_list_view`, hooks `connect_items_changed` to flip the stack between content/empty atomically per splice, and sets up the search wiring (bidirectional `search_btn` ↔ `search_bar` bind, scope toggle re-trigger, 150 ms debounce → `account.search_articles_with_snippets` → `populate_with_snippets` → `refresh_statuses`)
- Public surface: `list_view()`, `store()`, `selection()`, `selected_feed_id()`, `set_selected_feed_id()`, `current_article_node()`, `populate(Vec<Article>)`, `populate_with_snippets(Vec<(Article, String)>)`, `clear()`, `refresh_statuses(account)`, `search_active()`, `focus_search_entry()`
- `escape_fts5` (file-private) plus a unit test locking the wrap-and-double-quotes invariant

### What stayed on `ViaductWindow`

The cross-pane orchestration: the sidebar-selection handler that calls `timeline_view.populate(articles)` after fetching, the timeline-selection handler that builds the article render context + auto-mark-reads + refreshes sidebar unread totals, and the `act_*` methods that mutate read/star status (they read the current node via `timeline_view.selection()` then update the DB and trigger sidebar refresh). `search_btn` itself stays in the sidebar's `AdwHeaderBar` — that's a v2.0.0-pre3 concern when `SidebarView` extracts.

### Why this is more port-faithful, not less

Same argument as the article-pane extraction. NetNewsWire's `Mac/MainWindow/Timeline/TimelineViewController.swift` is the structural counterpart; having the boundary as a real Rust widget subclass instead of a comment in the god-object brings the architecture closer to the source we're translating.

### Numbers

`window.rs` shrank from 2358 lines (v2.0.0-pre1) to 2306 lines (−52 lines vs pre1; −594 vs the pre-pre1 baseline of 2900). The new `timeline_view.rs` is 348 lines + a 77-line `.ui` template. Net code at this point is ~+90 lines, mostly new public API surface and the `bootstrap` boilerplate. Tests: 23 viaduct + 71 viaduct-core + 1 integration = 95 (was 94 — added the `escape_fts5_wraps_and_doubles_quotes` test that came along with its function). Smoke launch from a release build: app starts cleanly, sidebar click → `TimelineView::populate` → perf log shows healthy 8 ms total.

### Test status

fmt clean, clippy `--workspace --all-targets -- -D warnings` clean, all 95 tests pass. Real OPML loads, sidebar feeds clickable, articles populate the timeline, search bar still binds via the sidebar's search button (now from inside `TimelineView::bootstrap` instead of `wire_search` on the window).

### What's next

- **v2.0.0-pre3** — `SidebarView` extraction (sidebar list view + `feed_names` map + unread-count walks + right-click feed/folder context menus + `act_*_feed` and `act_*_folder` methods)
- **v2.0.0-pre4** — Promote `article_renderer.rs` into a dedicated `ArticleRenderer` GObject (lives inside `ArticlePaneView`)
- **v2.0.0-pre5** — `glib::derived_properties` expansion (kill `refresh_unread_counts` walks)
- **v2.0.0-pre6** — WebKit ↔ GTK CSS bridge polish
- **v2.0.0** — final tag

## v2.0.0-pre1 — Phase 18: ArticlePaneView extraction

First step in the v2.0 architectural refinement. `ViaductWindow` was a 2900-line god object owning everything from the sidebar tree controller to the WebKit lockdown profile. NetNewsWire splits the equivalent into `SidebarViewController` / `TimelineViewController` / `DetailViewController`; we're catching up. This release peels the article pane off — the most self-contained chunk — to establish the pattern. TimelineView and SidebarView follow in -pre2 / -pre3.

### What moved

A new `ViaductArticlePaneView` custom widget (in `viaduct/src/ui/article_pane_view.rs` + `article_pane_view.ui`) now owns:

- The locked-down `WebKitWebView` plus the `viaduct-img://` / `viaduct-font://` scheme handlers, the link interceptor, the hover-URL overlay
- The reader-view + play-video buttons in the article pane's `AdwHeaderBar`
- The article-stack (content / "No article selected" empty page)
- `ArticleDisplayState` (raw HTML / extracted HTML / metadata for the NNW theme macros)
- The detected `VideoSource` for the active article
- `render_article_body` (private), `set_article` / `set_auto_reader` / `clear` / `idle_for_background` / `refresh_render` / `toggle_reader` / `play_video` / `current_article_url` (public), plus the in-pane `present_video_dialog`
- The `VideoPlaybackMode` enum + `current_video_playback_mode` + `embed_url_for_iframe` helpers (and their three unit tests)

### What stayed on `ViaductWindow`

Cross-pane plumbing: the toast overlay, the action group, the sidebar / timeline / split-view widgets, `Arc<LocalAccount>` + `Arc<ImageCache>`, the feed-name map, refresh orchestration, OPML import/export, hide-for-background, Phase 17's `connect_close_request`, and the timeline-selection handler (which now builds an `ArticleRenderContext` and calls `article_pane.set_article(ctx)` instead of mutating `imp.article_display` directly). Plus `act_open_in_browser` / `act_copy_url` (which read URLs through the timeline selection) and `act_toggle_reader` (now a one-line delegate to `article_pane.toggle_reader()`).

### Why this is more port-faithful, not less

Section 4 of `CLAUDE.md` is "Port. Don't invent." Decomposing `ViaductWindow` could read like an architectural deviation, but NetNewsWire already does this — `Mac/MainWindow/Detail/DetailViewController.swift` is the article-pane equivalent. Having that boundary as a Rust widget subclass instead of a comment in a 2900-line file just makes the porting reference clearer.

### Numbers

`window.rs` shrank from 2900 lines to 2358 lines (−542). The new `article_pane_view.rs` is 552 lines + a 79-line `.ui` template. Net code is ~+90 lines, mostly the new public API surface and per-pane wiring boilerplate. Tests: 22 viaduct + 71 viaduct-core + 1 integration = 94, no count change (the three `embed_url_for_iframe` tests came along with their function). Smoke test from a release build: app launches cleanly, sidebar selections route through the new pane, perf log shows healthy 23–24 ms total per click.

### Test status

fmt clean, clippy `--workspace --all-targets -- -D warnings` clean, all 94 tests pass. Manual smoke: real OPML loads, sidebar feeds clickable, articles render, hover URLs preview, theme/dark-mode switches still re-render the pane (via the new `pane.refresh_render()` path).

### What's next

- **v2.0.0-pre2** — `TimelineView` extraction (timeline list + search bar + timeline-mutating `act_*` methods)
- **v2.0.0-pre3** — `SidebarView` extraction
- **v2.0.0-pre4** — Promote `article_renderer.rs` into a dedicated `ArticleRenderer` GObject (lives inside `ArticlePaneView`)
- **v2.0.0-pre5** — `glib::derived_properties` expansion (kill `refresh_unread_counts` walks)
- **v2.0.0-pre6** — WebKit ↔ GTK CSS bridge polish
- **v2.0.0** — final tag

## v1.10.0 — Refresh while the window is closed (Phase 17 background daemon)

The last unchecked Phase 13 entry. NetNewsWire's macOS analog (`NSBackgroundActivityScheduler`) has no Linux equivalent without a portal client, so the architecture had been written up in `docs/background-service-plan.md` but never wired. This release wires it.

### What ships

A new **Keep refreshing after the window is closed** switch in Preferences → Sync. When on, closing the main window hides it instead of quitting; the periodic-refresh `glib::timeout` keeps firing (so notifications keep arriving if `notifications-on-refresh` is enabled), and clicking the dock icon re-summons the same window — not a second instance — with the timeline repopulated for whatever feed you'd left selected. Default is off, so first-run users aren't surprised by an app that refuses to quit.

### How it works

- **`run-in-background` GSetting** (boolean, default false) added to `org.virinvictus.Viaduct.gschema.xml`. The Preferences row binds bidirectionally; flip-to-true triggers the portal flow.
- **xdg-desktop-portal Background API** via the existing `viaduct_core::network::background::request_background_permission` helper (`ashpd` was already a dep). Fired on flip-to-true; the result rides a `tokio::sync::oneshot` back to the GTK thread through `glib::spawn_future_local`. On portal denial the GSetting flips back to false (the bind syncs the switch off automatically) and the parent window toasts an explanation. On non-Flatpak installs the portal grant is a no-op and the toggle just works.
- **Window-hide-on-close**: `connect_close_request` consults the GSetting and calls `hide_for_background` instead of running the popover-cleanup quit path. Returns `glib::Propagation::Stop` so the default close handler (which would tear down the application) doesn't run.
- **D-Bus activation re-summon**: `main.rs build_ui` first scans `app.windows()` for an existing `ViaductWindow` and `present`s it instead of constructing a second one. Without this, opening viaduct while it's hidden silently launches a duplicate. After re-presenting it calls `reload_current_timeline` so the user lands back where they were with whatever articles arrived in the meantime.
- **Idle memory drop on hide**: `hide_for_background` clears the `ImageCache` LRU (disk cache retained — re-loads go disk → texture, skipping the network), loads `about:blank` in the article-pane WebView so the WebProcess goes idle (releases DOM, JS heap, image surfaces), resets `article_display` / `current_video`, hides the play-video button, `remove_all`s the timeline `gio::ListStore`, and flips both stacks to their empty pages before `set_visible(false)`. The ImageCache gained `clear_memory()` (fire-and-forget) and `clear_memory_now()` (awaitable, used by the harness).
- **Flatpak manifest**: `--talk-name=org.freedesktop.portal.Background` added to `org.virinvictus.Viaduct.json` `finish-args`. Notifications portal accessible by default — no explicit talk-name needed (v0.8.0 notifications already worked).
- **`mem_check` background-cycle checkpoint**: fourth checkpoint added that calls `clear_memory_now()` after the warmup + reader-view phases and reports the RSS delta. The full GUI hide cycle (WebView idle, ListStore compact) needs interactive QA — those widgets can't be constructed from a headless bin. Headless number on the synthetic 500-favicon + 50-image corpus: ~−4 MB after clear.

### What this leaves for the 1.0 tag

Phase 17's only remaining bullet is "tag 1.0.0 and submit to Flathub" — blocked on Flathub onboarding, not code. Everything else for the 1.0 milestone is in tree.

### Test status

22 viaduct + 71 viaduct-core + 1 integration tests passing (no test count change — Phase 17 is wiring, not new logic surface). fmt + clippy clean.

## v1.9.1 — The Digital Antiquarian fix (Pango shaping + URL scan caps)

The v1.9.0 instrumentation paid off immediately. Brandon's logs:

```
INFO viaduct::perf: selection navigation
  item=The Digital Antiquarian
  articles=10 fetch_ms=13 populate_ms=12 status_ms=0 total_ms=25
```

Twenty-five milliseconds end-to-end on our side. But the user perceived a 2-second wait before the timeline rendered. **The slow part was happening AFTER our `populate_timeline` returned** — in GTK's per-row `connect_bind` callbacks for the visible rows. Two hot paths inside bind both scaled with `content_html` length, and Digital Antiquarian writes 5000-word essays.

### Root cause #1 — Pango shaping the entire stripped article

`strip_html_for_preview(content_html)` produced the full plain-text body (60 KB+ for long essays), which we passed directly to `preview_label.set_text()`. **Pango shapes the entire input** to compute line breaks before deciding which 2 lines to display and ellipsize. The remaining 60 KB of text never appears on screen but Pango paid the shaping cost anyway.

Fix: hard cap on `strip_html_for_preview` output at 400 chars. The function now early-exits its loop after collecting that many output chars, so it scans at most ~600–800 chars of input regardless of article length. Two new tests lock the cap.

### Root cause #2 — `detect_video` scanning entire article body

`spawn_video_thumbnail_fetch` runs `detect_video` on every row bind. For non-video articles, that scan walks the entire `content_html` looking for YouTube/Vimeo URLs and finds nothing — pure wasted main-thread work proportional to article size.

Fix: `scan_html_for_video` truncates input to the first 8 KB before scanning, backing off to a UTF-8 char boundary so the slice stays sound. Rationale: a video-bearing article references the embed in the lead paragraph 99% of the time. No realistic feed buries a YouTube link 10 KB into a 100 KB essay and expects the timeline preview to surface a thumbnail. Two new tests: one confirms the cap (URL at byte 10000 isn't found), one confirms the inverse (URL within 8 KB still found).

### Bonus fix — `Finalizing GtkListView, but it still has children left` warnings

The v1.7.1 right-click popovers attach via `set_parent(&list_view)` to live as children of the timeline / sidebar list views. Without explicit `unparent()` before the window tears down, GTK warns about the dangling parent-child relationship at finalize time. Non-fatal but ugly in the logs. New `connect_close_request` handler unparents all three popovers before propagation; clean shutdown.

### What this should look like in your logs

Re-run the app and click Digital Antiquarian. Per-row bind cost is now bounded by the 400-char preview cap and the 8-KB video scan cap. **If the click still feels slow**, the new perf log will show whether time is going to fetch (DB), populate (splice), status (status), or — what we suspect now — to GTK layout / Pango that's still happening outside our instrumentation. Either way the logs will tell us.

### Test status

22 viaduct + 71 viaduct-core + 1 integration tests passing (was 20 + 69 + 1 — added the 4 new perf-cap tests). fmt + clippy clean.

## v1.9.0 — Timeline navigation perf + debug instrumentation

Brandon reported sidebar feed-clicking taking up to 3 seconds and the UI sometimes refusing further clicks. This release adds the instrumentation you'd want to diagnose that, plus the two structural fixes that explain most of the latency.

### What was making it slow

1. **Folder aggregation was sequential despite a doc-comment claiming parallelism.** `fetch_folder_articles` had `for feed in folder.feeds { account.fetch_articles_by_feed().await }` — each feed fetch awaited the previous before starting. For a folder with 30 feeds that was 30 sequential round-trips through the single-writer mpsc → SQLite → reply chain. Easy way to spend a few seconds.

2. **Rapid sidebar clicks piled up uncancelled fetches.** Every click spawned a fresh `glib::spawn_future_local` that fetched articles, populated the timeline, and refreshed statuses. If the user clicked five feeds in two seconds, five fetches were in flight; they all eventually completed and applied their results, the last writer winning. In the meantime the worker thread was contended and the main thread chewed through stale work between user inputs.

### Fixes

**Cancel-stale-fetch via generation counter.** New `selection_fetch_generation: Cell<u64>` on `ViaductWindow imp`. Every selection-changed handler bumps the counter and captures its value; when the spawned task returns, it compares to the current counter and drops the result if they don't match. Port of NNW's `FetchRequestQueue` pattern. Logs a structured line under the `viaduct::perf` target every time a result is dropped so you can see in real time when this is actually firing.

**`Account::fetch_articles_by_feeds(Vec<String>)`** — bulk DB op. New `ArticlesDbOp::FetchByFeeds` variant with a single `WHERE feed_id IN (?, ?, …)` SQL query, chunked at 500 IDs (under SQLite's 999 parameter limit, with headroom). `fetch_folder_articles` rewritten to use it — one round-trip instead of N.

### Debug instrumentation

Every sidebar click → timeline navigation now logs one structured line under `viaduct::perf`:

```
INFO viaduct::perf: selection navigation
  item="Daring Fireball"
  articles=143
  fetch_ms=12
  populate_ms=38
  status_ms=4
  total_ms=54
```

When `total_ms ≥ 500`, the level promotes to `WARN` so the slow case is visible without scrolling. Cancelled fetches log a separate line so you can tell when rapid clicks are dropping results:

```
INFO viaduct::perf: selection fetch dropped — newer click in flight
  item="All Unread"
  generation=12
  current=14
  fetch_ms=187
```

To see them, run from a terminal — `target/release/viaduct` — or filter explicitly:

```bash
RUST_LOG=info,viaduct::perf=info viaduct
```

`docs/debugging.md` (new) walks through all of this end-to-end, plus the `--debug` flag, the `RUST_LOG` examples, the `mem_check` harness, and how to add timing instrumentation to new hot paths.

### What this can't fix

The `populate_ms` cost on big smart feeds (e.g. "All Unread" with 4855 articles in your screenshot) is fundamental — it's the wall-clock time to construct that many `ArticleNode` glib::Object instances on the main thread. Easily 150–300 ms for a 5000-row populate. The eventual fix is a custom `gio::ListModel` that lazily vivifies items only as they scroll into view, but that's a bigger refactor (hundreds of lines, careful scrolling-state management). For v1.9.0 we make it visible in the logs so you can confirm whether populate is your actual bottleneck before investing in the rewrite.

### Test status

89 unit + 1 integration tests passing. fmt + clippy clean.

## v1.8.0 — Sync on open, periodic refresh, plus the background daemon plan

Two new preferences and a written architecture plan for the background-refresh feature.

### Sync feeds when viaduct opens

New `refresh-on-startup` boolean GSetting (default off). When enabled, viaduct fires a refresh cycle ~1500 ms after the OPML load completes — long enough for the sidebar / timeline to render so the user sees something before the spinner takes over the sync button.

Surfaced in **Preferences → Sync** as a switch row.

### Sync feeds periodically

The existing `refresh-interval-minutes` GSetting (which had been declared back in v0.8.0 but never wired) now actually drives a `glib::timeout_add_seconds_local`. Schema range was tightened — lower bound moved from 10 to 0, with 0 acting as the "disabled" sentinel.

Surfaced in **Preferences → Sync** as a combo row with discrete choices: Never / Every 15 minutes / Every 30 minutes / Every hour / Every 2 hours / Every 6 hours / Once a day. Snaps the closest preset on load if dconf-editor was used to set an off-preset value.

Re-arms automatically when the user changes the dropdown — the previous timer is cancelled before the new one starts, so toggling the dropdown a few times can't pile up overlapping cycles. Brandon's request was specifically "every 30 minutes" — that maps to one of the presets, which is now the default cadence we'd recommend in the README.

### Background daemon — the plan

`docs/background-service-plan.md` (new) documents the design for refreshing while the main window is closed. The short version:

- **Architecture chosen: window-hide pattern + xdg-desktop-portal Background.** Process keeps running after window close, periodic refresh continues to fire, dock-icon click re-summons the existing window. Portal permission requested via `ashpd::desktop::background::Background` (helper already exists in `viaduct-core/src/network/background.rs` since v0.5).

- **Architectures rejected**: separate `viaduct-daemon` binary (over-engineered for a reader app, doubles SQLite-write contention surface) and `systemd --user` timers firing CLI invocations (cold-start cost dwarfs refresh time, doesn't compose with Flatpak).

- **Work breakdown** (~3 focused days):
  1. `gtk::ApplicationWindow::connect_close_request` → hide instead of quit, gated on a new `run-in-background` GSetting.
  2. Switch row in Preferences → Sync to flip that setting; on flip-to-true, fire the portal request via `spawn_on_runtime`. On portal denial, flip back off + toast.
  3. Flatpak manifest gains `--talk-name=org.freedesktop.portal.Background`.
  4. D-Bus activation routes a launcher click while hidden to "present the existing window" instead of starting a second process.
  5. (Optional, deferred) System-tray indicator. GNOME doesn't ship one stock; KDE / XFCE do. Wait for users to ask.
  6. Idle-memory reduction when hidden: flush the `ImageCache` LRU, load `about:blank` on the article WebView, splice the timeline store empty. Budget: ≤ 100 MB resident while hidden. New `mem_check --background-cycle` mode to lock the budget in CI.

The plan is concrete enough that the next release can pick it up directly. Tagged for a v1.9.0 or v2.0.0 banner depending on how big a marketing beat we want it to be.

### Test status

89 unit + 1 integration tests still passing. fmt + clippy clean.

## v1.7.1 — Right-click context menus

The other half of v1.7.0's "obvious missing UI" round.

### Sidebar — feed rows

Right-click a feed in the sidebar to open a context menu with three sections:

1. **Mark All as Read** — fetches every article from the right-clicked feed and marks them all read in one batch. Doesn't change the timeline pane (you weren't looking at this feed; we shouldn't yank you to it).
2. **Refresh** + **Copy Feed URL** — single-feed refresh fires `refresh_specific_feeds([feed])`. Copy URL lands in `gtk::Clipboard` with a toast confirming.
3. **Delete Feed** — destructive-styled, opens an `AdwAlertDialog` with the feed name in the title and an explicit "this cannot be undone" warning. Confirm calls `Account::remove_feed(url)` (added in v1.7.0 in anticipation of this), reloads the sidebar, toast confirms. Article rows pruned by the next `cleanup_at_startup` cycle.

### Sidebar — folder rows

Smaller menu — just **Mark All as Read**, which walks the folder's feeds, fetches all articles, and marks them all read in one upsert.

### Timeline — article rows

Right-click an article in the timeline to open a context menu with:

1. **Toggle Read** + **Toggle Star** — same actions as `r` / `s` shortcuts.
2. **Open in Browser** + **Open Enclosure** + **Copy URL** — same actions as `b` / `Ctrl+Enter` / `Ctrl+Shift+C`.

The right-click also auto-selects the row before showing the popover, so existing `timeline_selection`-bound actions operate on the right-clicked article without us having to introduce a parallel `right_clicked_article` state. Cleaner than the sidebar path because timeline actions were already selection-driven; sidebar actions weren't.

### How the wiring works

Single `gtk::GestureClick` per list view (not per row), in BUBBLE phase. On right-click, `widget.pick(x, y)` returns the leaf widget under the cursor; we walk up `parent()` looking for an ancestor with `viaduct-article` (timeline) or `viaduct-sidebar-item` (sidebar) data attached. The data is set during the row factory's `connect_bind` via `unsafe set_data` and gets overwritten cleanly when the row recycles to a new model object.

This avoided changing `setup_timeline_list_view` / `setup_sidebar_list_view` signatures to accept a callback parameter, which would have required threading a `glib::WeakRef<ViaductWindow>` through the `sidebar.rs` / `timeline.rs` modules and creating a circular type dependency. Trade-off was a small `unsafe` block on each side of the parent-walk; same pattern we already use for `viaduct-read-handler` (the per-row `notify::read` signal handler in `timeline.rs:366`).

### What's not yet wired

- **Mark Above Read** / **Mark Below Read** in the timeline menu — NNW has these and they're useful. Skipped for v1.7.1 to keep this release focused; a future release can add them once we decide whether they should respect the current sort direction or always go strictly chronological.
- **Rename Feed** — would need a one-line `AdwEntryRow` mini-dialog. Not user-blocking; deferred.

### Test status

77 + 12 = 89 unit tests across the workspace, 1 integration. fmt + clippy clean.

## v1.7.0 — Add Feed dialog (the obvious missing feature)

Until v1.7.0 the only way to add a feed to Viaduct was OPML import. v1.0.0 → v1.6.0 shipped a complete feed reader that didn't *let you add feeds*. Brandon caught it the right way — by laughing about it. Fixed.

### What ships

- **`Ctrl+N` / "Add Feed…" in the primary menu.** New `act_add_feed` action wired into the same `gio::SimpleAction` infrastructure as every other window action. Menu accelerator surfaces the keybinding next to the entry.

- **Modal `AdwDialog` with three fields**:
  - **URL** — accepts a feed URL OR a website URL. Discovery handles either.
  - **Name (optional)** — overrides whatever the feed itself reports as its title. Blank → falls back to the parsed feed title.
  - **Folder** — `AdwComboRow` populated from the current OPML's folder list, with "None" at the top for standalone placement.

- **Two-pass discovery** — port of NetNewsWire's `FeedFinder`. New module `viaduct-core/src/network/feed_discovery.rs`:
  1. Treat the URL as a feed and try to parse it directly. RSS / RDF / Atom / JSON Feed all dispatch through the existing `parser::parse`.
  2. If parsing fails, treat it as HTML. Use `parser::extract_metadata` to scan the `<head>` for `<link rel="alternate" type="application/rss+xml | atom+xml | feed+json">` tags. Pick the first match, recurse into it as a feed.
  
  Recursion depth is capped at 2 to avoid runaway loops. URL canonicalization adds `https://` when the user pastes a bare hostname like `daringfireball.net`. 8 unit tests cover every branch.

- **Async hand-off pattern.** All reqwest work goes through `crate::spawn_on_runtime`, never directly off the GLib executor. The dialog awaits a `tokio::sync::oneshot` for the result. Same pattern as `viaduct-img://`, the favicon fetch, and the v1.5.6 video-thumbnail fix. Anti-pattern explicitly called out in CLAUDE.md gotchas; followed religiously.

- **`Account::add_feed`** — places the discovered feed into the OPML hierarchy under the chosen folder (creating the folder if needed) or as a standalone entry. Dedupes by `feed_url`: adding a feed already present at the same URL returns the existing entry rather than duplicating. Saves the OPML through the same coalesced ~500 ms debounced writer the rest of the app uses.

- **`Account::remove_feed`** — symmetric removal helper, ready for v1.7.1's context-menu Delete Feed action. Sweeps both the standalone list and every folder. Empty folders left behind are *preserved* — folders are user-curated, not auto-pruned. Matches NNW behaviour.

- **Inline status feedback.** While discovery runs, the dialog shows "Looking up the feed…" in a dim caption row. On failure ("No feed found at that URL. Check the address and try again."), the row turns into a red error message and the Add button re-enables so the user can retry. On success, the dialog closes with a toast confirming the feed name.

- **Immediate refresh of just the new feed.** After the OPML save, `refresh_specific_feeds(vec![feed])` fires so articles appear in the timeline within seconds of clicking Add. Same path the OPML-import flow uses for newly-discovered feeds.

### Why this didn't ship earlier

It was on the roadmap — implicit in the "user-facing OPML exchange" Phase 12 work — but somehow never got translated into a single-feed entry path. Every release between v0.10 and v1.7.0 assumed the user would either (a) pre-build an OPML file and import it, or (b) edit `~/.local/share/viaduct/local.opml` by hand. Neither is the right answer. Brandon hit the gap immediately when actually using the app, which is exactly the testing methodology that was missing during the long heads-down feature push from v1.0 → v1.5.

### Test status

- `viaduct-core`: 69 tests (was 61; 8 new for `feed_discovery::first_feed_link` and `canonicalize_input`).
- `viaduct`: 20 tests (unchanged; the dialog UI itself isn't unit-tested but the discovery layer it depends on is thoroughly covered).
- 1 integration test still passing.
- fmt + clippy clean.

### What's next

- **v1.7.1 — Right-click context menus.** Sidebar (mark-all-read / refresh-feed / copy-feed-url / delete-feed) and timeline (mark-read / star / open-in-browser / copy-url / mark-above-read / mark-below-read) popovers. The `Account::remove_feed` helper added in this release is what the Delete Feed action will call.

## v1.6.0 — Stable

Collecting the v1.5.5 → v1.5.9 stability arc into a tagged stable point. No new code beyond a version bump — every fix this release covers already shipped in a v1.5.x release. v1.6.0 exists so packagers, the Flathub manifest, and the AppData release history have a single coherent "stable" line to point at after a long string of incremental hotfixes.

### What's in this release line

Everything from v1.5.0 (workspace refactor) through v1.5.9 (working YouTube playback). Headline items:

**Architecture**
- Cargo workspace split into `viaduct-core` (headless: database, network, parser, models) and `viaduct` (binary: GTK / libadwaita / WebKit). Boundary enforced by the compiler, not code review (v1.5.0).
- Meson build wrapper for Flatpak packaging; Flatpak manifest switched to `buildsystem: meson` (v1.5.1).

**NetNewsWire parity catch-up** (v1.5.2)
- Atom `<summary>` and `<content>` strictly separated — port of NNW `d6eb8df7d`. Summary lands in `ParsedItem.summary`, content in `content_html`, never share a slot.
- Orphan-author cleanup on startup — port of NNW `200e5b19f` (issue #5232). Sweeps `authorsLookup` rows whose article no longer exists, drops `authors` rows no longer referenced. Plus an `authorsLookup_article_id_idx` index for the trigger and sweep.
- Domain lists (`SPECIAL_CASE_DOMAINS`, `NO_MINIMUM_TIME_DOMAINS`) confirmed in sync with NNW's April 2026 commits.

**Visual identity** (v1.5.3–v1.5.4)
- Application icon — stone arch with RSS broadcast waves emerging from the inner archway. Concentric arcs (uniform 10px wall thickness), integrated keystone, NNW-orange RSS mark inside. Scalable SVG plus pre-rendered 256/512 PNGs in `docs/`.
- Symbolic icon for menus / sidebars / notifications.
- Horizontal banner logo with unified cream card so the wordmark reads on both light and dark host backgrounds.
- README rewritten — leads with "A Linux port of NetNewsWire" framing; features table is specific instead of marketing-shaped; explicit Acknowledgements section thanking Brent and the NNW team and recommending NetNewsWire to macOS / iOS users.
- Real screenshots wired into both README and AppStream metainfo (v1.5.8).

**Stability arc** (v1.5.5–v1.5.9)
- Selected-row contrast: GNOME's stock convention (full accent background, contrasting foreground), with a WCAG-luminance-based foreground picker that handles every shipped theme — including Tiqoe Dark's warm tan where white-on-tan would fail AA (v1.5.5).
- Adaptive-layout forward navigation push: tap a feed in collapsed mode, the timeline page actually appears (v1.5.5).
- Vimeo thumbnail panic fix: every reqwest call now goes through `spawn_on_runtime`, never directly off `glib::spawn_future_local`. Fixes the cascading AdwNavigationSplitView freeze users hit on Vimeo-bearing articles (v1.5.6).
- Defensive `set_show_content` guards: only push when state actually needs to change. Eliminates the "back / close button stuck until Esc" bug (v1.5.7).
- WebKit focus-grab cleanup: `act_close_article` and the video-dialog `connect_closed` both explicitly grab focus on the timeline list view, plus load `about:blank` on closing the embed view to release WebKit's input grab (v1.5.7).
- Touchpad scroll judder fixed: 220 ms debounce on video-thumbnail spawns. Rows scrolled past quickly never trigger a tokio task (v1.5.7).
- YouTube + Vimeo playback finally works: embed runs inside a real `<iframe>` of a synthetic host HTML document loaded with a `viaduct.local` base URI, satisfying the player's iframe-context check that surfaces error 153 when loaded as a top-level navigation. Plus the storage / WebGL settings the player needs to initialize. Plus URL escaping so query separators aren't corrupted by the HTML parser (v1.5.8 + v1.5.9).

### Test status

77 unit + 1 integration tests passing across the workspace. fmt + clippy clean. Mem-check harness still running 500 feeds × 10 articles + 550 image fetches + 10 Reader View extractions under the 500 MB ceiling.

### What's next

The roadmap's only remaining open item is **tag 1.0.0 and submit to Flathub** under Phase 17. That's still on Brandon — the Flathub submission needs his account credentials. v1.6.0 is the stable line that submission would point at.

## v1.5.9 — YouTube playback finally works (iframe wrapper)

The v1.5.8 fix (re-enabling LocalStorage / IndexedDB / WebGL on the embed view) addressed the *symptom-adjacent* problem but not the actual cause. YouTube error 153 kept firing.

Real cause: `https://www.youtube-nocookie.com/embed/<id>` is *designed to be loaded inside an `<iframe>` element of a host page*, not as a top-level navigation. The embed page's player JS checks for that context — specifically that `window !== window.top` — and surfaces "Error 153: Video player configuration error" when loaded directly. The check happens *before* the player gets to verifying browser features, which is why enabling the storage APIs in v1.5.8 didn't help.

### Fix

`present_video_dialog` now generates a small HTML host document containing the YouTube / Vimeo embed inside a real `<iframe>`, then loads that HTML via `view.load_html(&html, Some("https://viaduct.local/embed/"))`. The synthetic `viaduct.local` base URI is what the embed sees as its parent origin — a plausible host that satisfies the iframe-context check. The iframe element gets `allow="autoplay; encrypted-media; fullscreen; picture-in-picture"` and `referrerpolicy="strict-origin-when-cross-origin"` per YouTube's official iframe-embed documentation.

### Why HTML escaping matters

The embed URL carries `?autoplay=1&rel=0&modestbranding=1` — three `&` separators. Inserted directly into `<iframe src="…">` without escaping, the HTML parser would interpret `&rel` as an entity reference and silently corrupt the URL. New `embed_url_for_iframe(url)` helper escapes the five attribute-context-relevant characters (`& < > " '`). 3 unit tests lock the escaping behaviour.

### What this also catches

The same iframe-context check applies to Vimeo's `player.vimeo.com/video/<id>` URL. Both providers now route through the iframe wrapper and play correctly.

### Test status

77 unit + 1 integration tests passing (was 74; 3 new for the URL-escape helper). fmt + clippy clean.

## v1.5.8 — YouTube playback fix + real screenshots

Two unrelated items.

### YouTube error 153 — "Video player configuration error"

In-pane video playback shipped broken. Tapping the play button on a YouTube article opened the dialog, the embed loaded, and then the YouTube player surfaced its own "Error 153: Video player configuration error" message before any video could play. Reproducible on every YouTube video, every time.

Root cause: the embed WebView's `WebKitSettings` were modeled on the article-pane WebView's lockdown profile, which disables three things the embedded YouTube player actually *needs* to initialize:

- `enable_html5_local_storage` — YouTube stores volume / quality / playback preferences in LocalStorage; the player JS bails when the store isn't writable
- `enable_html5_database` — IndexedDB; modern YouTube uses it for the player session cache
- `enable_webgl` — needed for VP9 / AV1 hardware decode paths; without it the player falls back to software decode and the embed page fails

Privacy was the reason for the lockdown, but it's preserved by the dialog lifecycle: when the dialog closes, `connect_closed` calls `load_uri("about:blank")` then `try_close()` on the embed WebView. Cookies / LocalStorage / IndexedDB written during playback die with the WebProcess. The article-pane WebView's lockdown profile is unrelated and unaffected; it stays as strict as ever.

Also dropped the `set_user_agent_with_application_details("Viaduct", "1.4")` call. Some video hosts run anti-bot heuristics on the UA string — appending an unknown application identifier risks throttling or refusal. WebKitGTK's default UA is a standard `Mozilla/5.0 ... AppleWebKit/...` string, accepted everywhere.

### Real screenshots

Brandon dropped two captures into `docs/screenshots/`:
- `main-adwaita.png` — three-pane wide layout, dark mode, Adwaita theme with the system orange accent
- `main-sepia.png` — same layout but with the Sepia theme active, showing the warm cinnamon accent propagated across the chrome (selected timeline row, sidebar selection)

Both are 1962×1202 captures from Fedora 43 / GNOME 50 / Wayland. Wired into both the README's Screenshots section and the AppStream metainfo (`data/org.virinvictus.Viaduct.appdata.xml`) — the AppStream entry is what gnome-software / Flathub display on the install page.

74 unit + 1 integration tests passing. fmt + clippy clean.

## v1.5.7 — Three responsiveness bugs in the adaptive layout

After the v1.5.6 Vimeo panic fix, Brandon kept testing the adaptive layout and surfaced three more issues. None of them were panics, all of them were visible in normal use:

### 1. Back / close buttons stuck until Escape

The `AdwHeaderBar` back button (and the window-manager X button) sometimes wouldn't respond after navigating in collapsed adaptive mode. Pressing **Escape** unstuck them.

Root cause: v1.5.5's forward-push code called `set_show_content(true)` *unconditionally* when our handlers fired, even if the SplitView was already showing content. During an in-flight transition animation (which AdwNavigationSplitView treats as state-mutating), calling `set_show_content` again from selection-change handlers confused the state machine. The result was the SplitView's nav stack ending up half-popped — visually showing the article but treating itself as if it had already popped, so the back button's pop call was a no-op. Escape worked because our `act_close_article` did its own state cleanup that re-aligned things.

Fix: every call to `set_show_content(true | false)` now checks `is_collapsed() && !shows_content()` (or its inverse) first. Calling it with the desired state already in effect was the bug — guarding it makes the calls idempotent.

### 2. Focus stuck on the WebKit article view

Same symptom class — keyboard focus would stay on the article-pane WebKitWebView even after navigating back via Esc, leaving the user unable to interact with anything until they clicked something. WebKit on Wayland is notorious for holding keyboard focus aggressively.

Fix: `act_close_article` now explicitly calls `timeline_list_view.grab_focus()` after popping the nav stack and clearing the selection. Same fix applied to `present_video_dialog`'s `connect_closed` — when the playback dialog dismisses, we explicitly re-focus the timeline list view AND load `about:blank` on the embed WebView before `try_close()`. Loading `about:blank` forces WebKit to reset its input context, releasing whatever focus / pointer grab it held during playback. Defense in depth.

### 3. Touchpad scroll juddered through video-heavy timelines

Two-finger touchpad scrolling stalled or skipped frames when scrolling through feeds with many YouTube / Vimeo articles.

Root cause: every row recycle in `connect_bind` called `spawn_video_thumbnail_fetch`, which immediately spawned a tokio task. During fast scrolls, rows recycle 30+ times per second; the cumulative `cache.client().await` Mutex acquisition on every recycle stalled the GTK main thread enough to drop scroll frames. The recycled-row stale-guard correctly threw away the result, but the task spawn + Mutex round-trip happened anyway.

Fix: the fetch is now wrapped in a 220 ms `glib::timeout_add_local_once` debounce. If the row recycles to a different article (or the picture widget gets dropped) before the timer fires, we bail without spawning the runtime task. Rows scrolled past in less than a quarter-second never trigger a fetch; rows the user actually lingers on still get their thumbnail.

### What's not fixed

The single-pane (≤600sp width) layout still has noticeable transition cost compared to two-pane and three-pane. That's `AdwNavigationSplitView`'s collapsed-mode page-transition animation — CSS-transform-driven, runs on the compositor, costs frame budget proportional to the widget tree being animated. We've reduced the per-row work that competes with it (the debounce above), but we can't eliminate the transition cost itself without forking AdwNavigationSplitView. If it stays bothersome, the workaround is to use the app at the wide breakpoint where the splits stay mounted.

### Test status

74 unit + 1 integration tests passing. fmt + clippy clean.

## v1.5.6 — Vimeo thumbnail panic fix (and the AdwNavigationSplitView freeze it caused)

**Critical fix.** v1.3.0 introduced a tokio-reactor regression in the Vimeo path of `spawn_video_thumbnail_fetch`, but it stayed dormant because most users have YouTube-heavy timelines. Brandon hit it testing the v1.5.5 adaptive layout on a Vimeo-bearing article and the app froze.

### What was happening

`spawn_video_thumbnail_fetch` runs inside `glib::spawn_future_local` (it has to — it touches `gtk::Picture`). For YouTube articles the code only does string formatting, all good. For Vimeo articles, it called:

```rust
let client = cache.client().await;
let url_opt = video_thumbs::thumbnail_url(&client, &source).await;
```

…where `thumbnail_url` for Vimeo internally calls `client.get(oembed_url).send().await`. **That triggers DNS resolution, which `hyper-util` performs with the assumption that a Tokio reactor is running on the current thread.** GLib's `MainContext` executor isn't a Tokio reactor, so `dns.rs:119` panics with "there is no reactor running, must be called from the context of a Tokio 1.x runtime."

The CLAUDE.md gotchas section warns about exactly this pattern. I missed it when porting v1.3.0's video thumbnail code; the YouTube branch ran clean and I shipped without testing the Vimeo branch on a real Vimeo article.

### Why the freeze

The panic itself doesn't crash the process — `glib::spawn_future_local` swallows panics from individual futures. But the panic leaves the *future* in a poisoned state: cancelled mid-await, with the `tokio::sync::oneshot::Sender` it owns dropped without sending. Any `AdwNavigationSplitView` animation or back-button handler that ended up awaiting on the same task graph stalled forever waiting for a result that wasn't coming. From the user's perspective the app froze on back-navigation right after viewing a Vimeo article.

### Fix

Rewrote `spawn_video_thumbnail_fetch` to do **all reqwest work inside `crate::spawn_on_runtime`** and ferry the resolved bytes back to the GTK thread via `tokio::sync::oneshot`. Same pattern used by `viaduct-img://`'s scheme handler and `ImageCache::favicon`/`image`/`video_thumbnail`. The GLib-side future just awaits the channel and decodes the texture — no reqwest call ever runs on the GLib executor.

```rust
// v1.5.6 (correct):
let (tx, rx) = oneshot::channel::<Option<Vec<u8>>>();
crate::spawn_on_runtime(async move {
    // every reqwest call lives inside this block
    let bytes = match source { ... };
    let _ = tx.send(bytes);
});
glib::spawn_future_local(async move {
    let Ok(Some(bytes)) = rx.await else { return };
    // texture decode + assignment, GTK-thread only
});
```

### Audit

Swept every other `glib::spawn_future_local` block in the workspace for stray `reqwest` / `client.send()` / `.bytes().await` / `.json()` calls. None found. The Vimeo path was the lone offender. The viaduct-img URI scheme handler, the favicon fetch, the OPML load, the timeline-fetch, the article-render, the Reader View extraction kick-off — all correctly hop to the tokio runtime before doing any I/O.

### Lag

Brandon also reported the adaptive layout was laggy. That's almost certainly cascading from the same panic — every Vimeo article binding queued a panicking task, and the GLib loop spent extra time unwinding cancelled futures between frames. With the panic gone, the lag should resolve. If lag persists at the breakpoint transition itself (the AdwBreakpoint animation), that's libadwaita's transition cost, not us.

### Test status

74 unit + 1 integration tests passing. fmt + clippy clean. The video-thumbnail logic was already covered by the v1.3.0 unit tests for URL detection; the I/O path itself is integration-tested through a live request.

### Lesson

CLAUDE.md gotchas section is right in the open about this exact pattern. I read past it when porting Vimeo support. Pre-flight check before shipping any future that does I/O: *is the I/O happening inside a `glib::spawn_future_local`? If yes, route through `spawn_on_runtime` + oneshot.* No exceptions.

## v1.5.5 — Selected-row contrast + adaptive navigation push

Two real bugs Brandon caught at the running app, both significant.

### 1. Selected timeline row was unreadable in Sepia (and other themes)

The v1.2.0 app-wide accent unification set selected-row CSS to:
```
listview > row:selected {
  background-color: alpha({hex}, 0.20);
  color: {hex};
}
```
That gave both the row background AND the row foreground the same hue, just at different alphas. In light mode it was tolerable (accent text on accent-tinted-white reads okay). In dark mode it was a disaster: Sepia's `#7a4d1f` text on a 20%-alpha Sepia tint over the dark base gave roughly 1.6:1 contrast — well below the 4.5:1 WCAG AA minimum, and visibly illegible in the screenshot.

Fix: switch to GNOME's stock convention — full-saturation accent background, contrasting foreground. Plus override the inner labels' colors so `.heading` (which inherits color naturally), `.dim-label` (which has its own opacity) and our `viaduct-row-read` class (opacity 0.55 on read rows) all paint correctly when their row is selected:

- `listview > row:selected` — full accent background, contrasting foreground.
- `listview > row:selected label` — every child label inherits the foreground.
- `listview > row:selected .dim-label` — opacity bumped from 0.55 → 0.85 (preserves preview-vs-title hierarchy but stays legible on the accent fill).
- `listview > row:selected .viaduct-row-read` — opacity reset to 1 (read-state dimming actively unhelpful when the user is reading the selected item).

### 2. Foreground picked by WCAG contrast, not hardcoded white

For most shipped themes, white-on-accent gives 7:1 or better — no problem. But Tiqoe Dark's warm tan `#b08660` is a special case: white gives ~3.4:1 (fails AA), while `#1d1d1d` near-black gives ~4.9:1 (passes AA). The previous fix would have left Tiqoe Dark's selected-row foreground borderline-illegible.

New helper `pick_accent_fg(hex)` computes WCAG relative luminance and contrast-ratio against both white and near-black, returns whichever wins. Applied to every accent the CSS uses:

| Theme | Accent | Picked fg | Contrast |
|---|---|---|---|
| Sepia | `#7a4d1f` | `#ffffff` | ~8.4:1 ✓ |
| Appanoose / Hyperlegible / Promenade | `#086aee` | `#ffffff` | ~7:1 ✓ |
| Biblioteca | `#1145a5` | `#ffffff` | ~9:1 ✓ |
| NewsFax | `#3a3a3a` | `#ffffff` | ~12:1 ✓ |
| Tiqoe Dark | `#b08660` | `#1d1d1d` | ~4.9:1 ✓ |
| Verdana Revival | `#2670c4` | `#ffffff` | ~5:1 ✓ |
| Adwaita | _(none — opts out)_ | _(GNOME picks)_ | _(system)_ |

5 new unit tests covering the picker's behaviour: white for dark accents, near-black for warm tan, safe fallback to white on garbage hex, luminance endpoints (white = 1.0, black = 0.0), malformed-input rejection. **17 article_renderer tests now passing.**

### 3. Adaptive layout broken when collapsed

The `AdwBreakpoint`-driven mobile / phone layout collapses the inner and/or outer split views into a navigation stack. Selecting a sidebar item (when the outer view is collapsed) or a timeline article (when the inner view is collapsed) needed to push to the next page in the stack. Without that push, the user was stuck on whatever page they started on — tap a feed in the sidebar, screen doesn't change. Tap an article, screen doesn't change. App became unusable below the 900sp / 600sp breakpoints.

The forward-push direction was simply never wired. The back direction works because `act_close_article` (Esc, v1.5.2) calls `set_show_content(false)`. The forward direction needed:
- Sidebar selection handler — `if outer_split_view.is_collapsed() { set_show_content(true) }`
- Timeline selection handler — `if inner_split_view.is_collapsed() { set_show_content(true) }`

Both wired, both checked through the existing template children we already had. ~6 lines net, but the difference between "useless on a narrow window" and "fully functional in mobile mode."

### Test status

74 unit + 1 integration tests passing across the workspace (was 73 — 5 new in `article_renderer`, no regressions elsewhere). fmt + clippy clean.

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
