# Background service — design plan

> Goal: keep feeds refreshing while the main window is closed, on a
> user-configurable cadence, on Linux/Wayland, in a way that survives
> Flathub sandboxing.

This is a **plan**, not shipped code. The current state (v1.8.0):

- `refresh-on-startup` and `refresh-interval-minutes` GSettings drive
  refresh while the application process is alive.
- `viaduct-core/src/network/background.rs` already wraps
  `ashpd::desktop::background::Background::request()` for the portal
  permission, but nothing currently calls it.
- The Flatpak manifest declares `--share=network` but doesn't yet
  expose the Background portal.

What's missing is the persistence layer: when the user closes the
window, the process exits and refresh stops. The user has to re-open
the app for fresh articles. Below is how to fix that.

---

## Decision: window-hide pattern + xdg-desktop-portal Background

Three architectural choices considered. Picking the first.

### Option A — Window-hide-on-close, single process, portal-permitted

The `viaduct` binary stays running after the window closes. On window
close, hide the window instead of quitting; the periodic-refresh
`glib::timeout` we already wired in v1.8.0 keeps firing. Optionally
register a D-Bus activation handler so clicking the dock icon
re-summons the existing window.

For Flathub sandboxing, the `xdg-desktop-portal` Background API
(`org.freedesktop.portal.Background`) is the official mechanism for
"please keep me running after the window closes." On a non-Flatpak
install the API is a no-op and the process just keeps running by
default.

**Pros**:
- Simplest. Reuses every refresh code path we already have.
- One binary, one install, one set of preferences.
- Single source of truth for state — no IPC needed.

**Cons**:
- Process keeps full GUI memory footprint resident. We measured ~280
  MB peak with the window visible; idle-with-window-hidden should be
  noticeably less but still not as small as a pure daemon.
- User has to grant Background permission once via the portal dialog
  the first time they enable the feature.

### Option B — Separate `viaduct-daemon` binary + D-Bus IPC

Ship a second binary that owns the SQLite worker and the refresher,
exposed over D-Bus. The GUI launches the daemon if it isn't running,
sends commands, and reads from the same `articles.sqlite`. Daemon
runs as a systemd user service; auto-starts on login.

**Pros**:
- Smallest possible idle footprint — no GTK or WebKit resident when
  the window is closed.
- Conceptually cleaner separation of concerns.

**Cons**:
- Requires designing a D-Bus interface and keeping the daemon and
  GUI in sync across releases.
- Cargo workspace gets a third crate.
- Two SQLite WAL writers becomes a real concern — currently the
  single-writer mpsc pattern guarantees no contention. Splitting
  across processes means the GUI either reads-only (fine, WAL
  supports it) or contends with the daemon for the write lock
  (much worse). NetNewsWire's analog is the "coordinated SQLite"
  approach which is non-trivial.
- More moving parts means more places for state to desync.
- Disproportionate cost for the value — most users keep their RSS
  reader open most of the time anyway.

### Option C — `systemd --user` timer firing a one-shot CLI

Cron-style: a `viaduct refresh` subcommand that runs the refresher
once and exits. A `systemd --user` timer fires it every N minutes.

**Pros**:
- Smallest possible footprint per refresh.
- Survives across reboots without GUI involvement.

**Cons**:
- Each invocation cold-starts: SQLite open, OPML parse, reqwest pool
  rebuild, conditional-GET headers re-loaded. Throughput much lower
  than a long-running daemon.
- No way to deliver desktop notifications without a running session
  bus client.
- Doesn't compose well with the Flatpak sandbox; systemd timers
  inside a Flatpak sandbox aren't a thing.
- User has to install the timer unit manually; we lose the
  prefs-dialog UX.

---

## Picking Option A

It's the right trade for a reader app. Most users keep the window
open most of the time anyway; the optimization point is "don't make
me reopen it just because I closed it earlier." A daemon-grade
architecture would be over-built for that use case and creates
ongoing IPC maintenance cost.

Adopting Option A means the work breaks down cleanly:

---

## Work items

### 1. Window-close behaviour

Wire `gtk::ApplicationWindow::connect_close_request` to:

- Hide the window (`window.set_visible(false)`).
- Return `glib::Propagation::Stop` so the default close handler
  (which quits the process) doesn't run.
- Clear the timeline + sidebar selection so re-show doesn't open
  on a stale article.

Conditional: only do this when the new
`run-in-background` GSetting is enabled. Otherwise, normal
quit-on-close behaviour. Default off so first-run users aren't
surprised by the app refusing to quit.

### 2. New GSetting

```xml
<key name="run-in-background" type="b">
  <default>false</default>
  <summary>Keep refreshing feeds after the window is closed</summary>
  <description>
    When enabled, viaduct hides the main window on close instead of
    quitting, and continues to run scheduled refresh cycles in the
    background. The first time you enable this, viaduct will ask the
    desktop for permission to run in the background.
  </description>
</key>
```

Add a switch row in Preferences → Sync, below the existing two rows.
On flip-to-true, call `viaduct_core::network::background::request_background_permission`
via `crate::spawn_on_runtime`. On portal denial, flip the switch back
off and toast an explanation.

### 3. xdg-desktop-portal integration

`viaduct-core::network::background` already has the request helper.
Hook it as above. The `auto_start(true)` flag means the desktop will
also auto-launch viaduct at login (subject to the user's portal
prompt response).

For the Flatpak manifest, add the portal permission:

```json
"finish-args": [
  …existing…,
  "--talk-name=org.freedesktop.portal.Background"
]
```

(Plus possibly `org.freedesktop.portal.Notifications` if we don't
already have it for the notification-on-refresh setting — verify
under Flatpak.)

### 4. D-Bus activation for re-summoning the window

`adw::Application` can register itself for D-Bus activation via
`gio::ApplicationFlags::HANDLES_OPEN | DEFAULT_FLAGS`. Then a click
on the dock icon while the process is hidden routes through
`activate` and we re-show the existing window instead of starting a
new instance.

This is mostly free — the existing `app.connect_activate(build_ui)`
handler can be modified to:

- If a window already exists, present it.
- Otherwise build a new one.

### 5. System tray indicator (optional)

Some users will want a visible "viaduct is running in the background"
cue. GNOME 50 doesn't ship a system tray by default but extensions
exist; KDE / XFCE do. The right primitive is `gtk::StatusIcon` (legacy)
or libayatana-appindicator. Defer until users actually request it —
the dock icon + system search returning a running viaduct should be
enough for most.

### 6. Idle memory reduction

When the window is hidden:

- Clear the `ImageCache` LRU (drop in-memory bytes; disk cache
  retained).
- Drop the article-pane WebView's body (load `about:blank`) so the
  WebProcess goes idle.
- Compact the timeline `gio::ListStore` (remove all rows; rebuild on
  re-show from the active sidebar selection).

Target: ≤ 100 MB resident while hidden, ramping back up on re-show.
Add a `mem_check` mode that runs the hide → idle → show cycle and
reads VmRSS at each step so we can lock the budget in CI.

### 7. User-visible behaviour summary

Once shipped:

- **Sync feeds when viaduct opens** (existing v1.8.0 setting) — fires
  one refresh on startup.
- **Sync feeds periodically** (existing v1.8.0 setting) — fires
  refresh every N minutes while the process is running.
- **Keep refreshing after the window is closed** (NEW) — when the
  user closes the window, the process keeps running, the periodic
  timer keeps firing, notifications keep arriving (if enabled).
  Reopening the dock icon re-summons the existing window.

---

## Test plan

- `mem_check --background-cycle` harness: run hide → 30 sec idle →
  show, read VmRSS at each phase, fail if hidden idle exceeds
  100 MB.
- Manual: enable run-in-background, close window, wait for the
  next periodic refresh, verify a notification fires (with
  notifications-on-refresh enabled).
- Manual under Flatpak: verify the portal permission prompt appears
  on first enable, and that revoking it via gnome-control-center
  cleanly transitions us back to quit-on-close.

---

## Estimated effort

- Items 1–3 (window-hide + setting + portal call): one focused
  release, ~1 day of work. This is the minimum viable background.
- Item 4 (D-Bus re-summon): half a day. Without this, "open viaduct
  while it's hidden" silently launches a second instance — broken.
- Item 6 (idle memory): another day, the work is mostly profiling
  and wiring up release paths to drop the right things.

Total: ~3 focused days. Fits a single release tagged as v1.9.0 or
v2.0.0 (depending on how big a banner we want for the feature).

## Tracking

Roadmap entry: Phase 17, "xdg-desktop-portal Background daemon."
This document supersedes the brief mention there with a concrete
plan. Convert to a checklist when the work actually starts.
