# viaduct — Manual QA checklist

Hand-test list for the v2.6.22 → v2.7.0 release arc. fmt + clippy + 132
tests pass; this list catches the things automation can't (UI flow,
WebKit pane, GNOME shell behavior, real-feed network paths).

Run a release build first:

```sh
cargo build --release
./target/release/viaduct
```

Tail the log in a second terminal so you can verify behavior:

```sh
RUST_LOG=info,viaduct=info ./target/release/viaduct 2>&1 | tee /tmp/viaduct-qa.log
```

A fresh XDG sandbox is the safest test bed (your real feeds stay
untouched):

```sh
mkdir -p /tmp/viaduct-qa/{data,cache}
XDG_DATA_HOME=/tmp/viaduct-qa/data \
XDG_CACHE_HOME=/tmp/viaduct-qa/cache \
./target/release/viaduct
```

---

## v2.6.22 — Article sort options

- [ ] Open the timeline header bar — there's a sort menu button (icon
      next to the existing sort/search controls).
- [ ] Click it; the menu lists "Newest first" and "Oldest first" with
      a radio-style check on the active option.
- [ ] Pick "Oldest first". Timeline reorders without reload jank.
      Article selection clears (expected).
- [ ] Pick "Newest first". Timeline reorders back.
- [ ] Select a folder in the sidebar; sort still applies across the
      cross-feed merge.
- [ ] Select a Smart Feed (Today / All Unread / Starred); sort applies.
- [ ] Quit and reopen — your last sort choice persists. (Stored in
      `timeline-sort-order` GSetting.)
- [ ] `dconf-editor /org/virinvictus/Viaduct/timeline-sort-order` shows
      the current selection as a string nick.

---

## v2.6.23 — Welcome dialog (first-launch)

- [ ] In the fresh XDG sandbox above (no `local.opml`), launch viaduct.
- [ ] After ~1 sec, a modal "Welcome to viaduct" dialog appears with
      one paragraph of intro, two pill buttons ("Add a feed…" /
      "Import OPML…"), and a 5-row list of suggested feeds.
- [ ] Click any suggested feed's `+`. Toast confirms it was added,
      sidebar refreshes, the feed begins fetching.
- [ ] Quit and relaunch in the same sandbox — welcome dialog **does
      not** reappear.
- [ ] In a second fresh sandbox, launch and dismiss with Escape /
      modal-blocker click — that should also count as "shown".
      Relaunch confirms.
- [ ] To test "show me again": `dconf write
      /org/virinvictus/Viaduct/welcome-shown false` and relaunch.
- [ ] Click "Add a feed…" — welcome dismisses, the standard Add Feed
      dialog opens.
- [ ] Click "Import OPML…" — welcome dismisses, the import file
      chooser opens.

---

## v2.6.24 — Activity Log dialog

- [ ] Open the primary menu (hamburger). There's a new entry
      "Activity Log…".
- [ ] Click it on a fresh sandbox: `AdwStatusPage` "No activity yet"
      with helper copy.
- [ ] Subscribe to a feed and refresh. Reopen the dialog.
- [ ] Verify entries show: feed name (or URL), the outcome
      ("Updated · 3 new" / "Not modified (304)" / "HTTP 503" / etc.),
      and a relative-time stamp ("Just now" / "5 min ago").
- [ ] Force a refresh (Ctrl+R) — many feeds 304. Verify "Not
      modified (304)" rows.
- [ ] Wait until the periodic-refresh timer fires (or set
      `refresh-interval-minutes=15` and wait). Verify Skipped rows
      with "refreshed within the last 29 minutes" appear.
- [ ] Pick a feed that's been deleted by the server (404 / 403) or
      add a junk URL. Verify "HTTP 404" / "HTTP 403" rows.
- [ ] Add a non-feed URL (e.g., `https://example.com/`). Verify a
      "Parse error · …" row.
- [ ] Click Clear in the dialog header. The list flips back to the
      empty state.
- [ ] Refresh once more; verify the empty state flips back to populated.
- [ ] Long-running session: leave viaduct running with periodic
      refresh on overnight, then check the dialog. Should hold the
      most recent 500 events; older ones evicted (FIFO).

---

## v2.6.25 — Share / Send to

- [ ] Select an article. Look at the article-pane header bar — there's
      a `send-to-symbolic` icon next to Reader View.
- [ ] Click it; menu shows: Copy URL / Copy URL with Title / Email
      Link… / Save to Pocket / Save to Instapaper.
- [ ] **Copy URL** → toast "Article URL copied." Paste somewhere;
      verify URL.
- [ ] **Copy URL with Title** → paste; verify two lines (title,
      then URL).
- [ ] **Email Link…** → your default mail client opens a new compose
      window with the title in subject and URL in body. (On GNOME this
      is whatever's set as the `x-scheme-handler/mailto` MIME default.)
- [ ] **Save to Pocket** → browser opens
      `https://getpocket.com/edit?url=<encoded>`. Login → article
      added to Pocket.
- [ ] **Save to Instapaper** → analogous flow at
      `instapaper.com/edit?url=…`.
- [ ] Right-click an article in the timeline. The "Send to" submenu
      shows all five entries.
- [ ] Pick an article with no URL (rare; a feed item with `<guid>`
      only and no `<link>`). All five share entries should toast a
      polite "no URL" message instead of crashing.
- [ ] Confirm percent-encoding is correct: pick an article with `&`,
      `?`, `=` or non-ASCII characters in the URL or title. Pocket
      should still redirect cleanly; the `mailto:` should preserve
      special characters.

---

## v2.7.0 — Custom Smart Feeds

### Create

- [ ] Open the primary menu. Click "New Smart Feed…".
- [ ] Dialog opens with one empty rule row (field combo defaults to
      "Title contains").
- [ ] Type a name ("Linux unread"). Pick "Read", "unread". Click Save.
      Toast confirms; sidebar refreshes; "My Smart Feeds" header now
      appears below the built-in Smart Feeds.
- [ ] Click the new Smart Feed in the sidebar. Timeline populates with
      every unread article across all feeds.

### Field types

- [ ] New Smart Feed: name "Test field — text". Pick "Title contains",
      type "linux". Save. Click — only articles whose title matches.
- [ ] New: "Body contains — code review". Save. Verify the body match
      hits across `content_html`, `content_text`, and `summary`.
- [ ] New: "Author is — Brent". Pick "Author contains" with a name
      you know exists in your feeds.
- [ ] New: "From feed — Hacker News". Pick "Feed is", select a feed.
      Verify the timeline narrows to that feed only.
- [ ] New: "Starred". Pick "Starred", "starred". Verify it lists
      all starred articles regardless of read state. Compare with the
      built-in Starred row (same content; the built-in row narrows to
      starred *and unread* per NNW semantics).
- [ ] New: "Recent — newer than 3 days". Verify only recent articles
      appear.
- [ ] New: "Old — older than 30 days". Verify older items only.

### Multiple conditions (AND)

- [ ] New: "Linux + unread". Two conditions: Title contains "linux"
      AND Read = unread. Click Save. Verify the timeline only shows
      unread items with "linux" in the title.
- [ ] New: "Recent starred". Conditions: Starred = starred AND newer
      than 7 days. Verify.
- [ ] Add a third condition for "From feed" too. Verify all three AND
      together.

### Empty / invalid forms

- [ ] Open New Smart Feed, type no name, click Save. Toast "Give your
      Smart Feed a name first." dialog stays open.
- [ ] Type a name, leave the rule's text field empty, click Save. Toast
      "Add at least one non-empty condition." dialog stays open.
- [ ] Open New Smart Feed, click Add Condition twice (now 3 rule
      rows). Click the trash icon on the middle row — that row
      disappears, the others remain.

### Delete

- [ ] Right-click a Smart Feed in "My Smart Feeds". Popover appears
      with "Delete Smart Feed".
- [ ] Click. Toast confirms. Row disappears from the sidebar.
- [ ] Delete the last custom Smart Feed; the "My Smart Feeds" header
      itself disappears (only shown when ≥1 exists).
- [ ] Right-click the built-in Smart Feeds (Today / All Unread /
      Starred) — no popover (those aren't deletable).

### Persistence

- [ ] Create two Smart Feeds, quit, relaunch. Both are in the sidebar
      after OPML loads.
- [ ] Inspect `~/.local/share/viaduct/smart-feeds.json` (or the QA
      sandbox's). It's pretty-printed JSON with `version: 1` and a
      `feeds` array.
- [ ] Manually corrupt the file (truncate to invalid JSON). Relaunch
      — startup logs a warning ("smart-feeds.json: …"); sidebar comes
      up without Smart Feeds rather than crashing.

### Edge cases

- [ ] Rename a feed via right-click → Rename Feed…, then click a
      Smart Feed that includes a "Feed is <that feed>" condition. The
      timeline still resolves correctly (the rule stores `feed_id`,
      not the display name).
- [ ] Delete a feed that's referenced by a "Feed is" rule. The Smart
      Feed still loads (returns no articles for that condition);
      future edit dialogs would show the dropdown without the deleted
      feed selected — by design.
- [ ] Refresh feeds while a Smart Feed is selected. Timeline
      auto-updates only on next click (no live re-evaluation; this is
      the deliberate v1 cut).

---

## Cross-cutting / regression

- [ ] **Memory:** open Activity Log → leave viaduct running with
      periodic refresh on (15 min) for an hour. Watch
      `tail -f /tmp/viaduct-qa.log | rg 'diag: refresh cycle post'`.
      Steady-state RSS should plateau — no monotonic climb.
- [ ] **Background daemon:** with run-in-background on, close the
      window. Tray icon stays. After 30 min, click tray → Show; window
      reopens with the latest articles already populated.
- [ ] **Adaptive layout:** drag the window to ~600 px wide. Sidebar /
      timeline / article pane collapse into the navigation stack.
      Smart Feeds (built-in + custom) still navigable.
- [ ] **Keyboard shortcuts:** Ctrl+? brings up the shortcuts window.
      Ctrl+R refreshes. Ctrl+N opens Add Feed. Ctrl+F focuses search.
      r / m / s / b / Enter all work on the selected article.
- [ ] **WebKit lockdown:** open an article whose body contains a
      `<script>` tag — should not execute. Open one with an external
      `<iframe>` — blocked by CSP. (Dev console: there isn't one;
      lockdown profile disables DevTools.)
- [ ] **Dark mode:** flip system to dark via GNOME settings; article
      pane swaps theme without refresh; sidebar accent updates.

---

## Known cuts you should NOT test

These are deliberate v2.7.0 scope cuts; if they happen to work, fine,
if they don't, that's expected:

- **Edit-existing Smart Feed dialog.** Delete + recreate is the v1
  workflow.
- **Per-Smart-Feed unread count badges.** Built-in Smart Feeds get
  badges; custom ones don't.
- **Reordering Smart Feeds in the sidebar.** They appear in creation
  order.
- **OR / NOT / nested groups** in the rule editor. AND-only.
- **Mastodon share intent.** Pocket and Instapaper only; Mastodon
  needs an instance picker GSetting that doesn't exist yet.
