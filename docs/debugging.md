# Debugging viaduct

## Where the logs go

`viaduct` uses [`tracing`](https://docs.rs/tracing) for structured logging.
Output goes to **stderr** by default. Run from a terminal to see it:

```bash
target/release/viaduct
```

…or for a system-installed build:

```bash
viaduct
```

## Default log level

The default filter is `info,html5ever=error`. You see `INFO` and above for
viaduct's own modules; `html5ever` (the readability HTML parser) is silenced
because it spams `WARN` for every malformed Atom entry, which is most of
them.

## Verbose mode

For a noisier session, pass `--debug`:

```bash
viaduct --debug
```

This sets `debug,viaduct=trace,html5ever=error` and enables a periodic
memory-status ticker (`/proc/self/status` snapshot every 8–25 s, logged
at `INFO`).

The flag is consumed by viaduct's own argv pre-pass and stripped before
GTK sees argv, so it doesn't conflict with libadwaita's command-line
handling.

## Filtering by module

`RUST_LOG` overrides the default filter. Common patterns:

```bash
# Just the perf-instrumentation lines from v1.9.0
RUST_LOG=info,viaduct::perf=info viaduct

# Network refresher chatter
RUST_LOG=info,viaduct_core::network::fetcher=trace viaduct

# DB worker op labels (which DB operation took how long)
RUST_LOG=info,viaduct_core::database::worker=trace viaduct

# Quiet everything except errors
RUST_LOG=error viaduct
```

## Performance-instrumentation logs (v1.9.0+)

Every sidebar click → timeline navigation logs one line under the
`viaduct::perf` target with structured fields:

```
INFO viaduct::perf: selection navigation
  item="Daring Fireball"
  articles=143
  fetch_ms=12
  populate_ms=38
  status_ms=4
  total_ms=54
```

Field meanings:

- **`item`** — what the user clicked. Feed display name, `[Folder Name]`,
  `Smart: All Unread`, or similar.
- **`articles`** — how many rows landed in the timeline store.
- **`fetch_ms`** — time from click → DB fetch complete. Worker-thread
  contention shows up here (e.g. if a refresh cycle is hogging the
  writer).
- **`populate_ms`** — time spent on the GTK main thread building the
  ListStore and triggering items_changed. Scales with article count;
  this is where 5000-row smart-feed clicks spend their time.
- **`status_ms`** — bulk status fetch + apply. Should always be small.
- **`total_ms`** — wall-clock end-to-end.

When `total_ms ≥ 500`, the level promotes from `INFO` to `WARN` so the
line stands out. The dropped-result log line surfaces when the user
clicks again before the previous fetch finished:

```
INFO viaduct::perf: selection fetch dropped — newer click in flight
  item="All Unread"
  generation=12
  current=14
  fetch_ms=187
```

That tells you the user had to wait 187 ms while a fetch they no longer
cared about completed. The result was discarded (no UI thrash), but
that 187 ms of worker time was wasted.

## What to do when the UI feels slow

1. Run viaduct from a terminal.
2. Reproduce the slow click.
3. Look for the `viaduct::perf` line that corresponds to it.
4. Check which field is large:
   - **`fetch_ms` is large** → DB worker is contended. Check if a
     refresh cycle is in flight (look for `viaduct_core::network::fetcher`
     output around the same timestamp). If many refreshes are competing,
     the fix is in the refresher's parallelism / throttling.
   - **`populate_ms` is large** → main-thread cost of building
     `ArticleNode` GObjects. For now this scales linearly with article
     count; a smart feed with 5000+ articles will pay ~150–300 ms here.
     The eventual fix is lazy `gio::ListModel` vivification.
   - **`status_ms` is large** → unusual. Bulk status fetch is one DB
     op. If this is large, the SQLite `IN` clause hit an unindexed
     path or the worker thread is wedged.
   - **All small but UI still feels slow** → could be GTK layout /
     adaptive-layout transition cost (libadwaita-side). Use
     `RUST_LOG=trace` and look for unexpected work between the
     `selection navigation` line and the next user input.

## Memory profiling

A separate harness exists under `cargo run --release --bin mem_check`
that synthesizes 500 feeds × 10 articles, warms the image cache, runs
the Reader View extractor, and prints `VmHWM` checkpoints. Use it after
DB / network changes to confirm the 500 MB peak budget still holds.

For live runtime tracking, `--debug` enables a periodic ticker that
logs RSS + peak every 8–25 seconds. Filter for it:

```bash
RUST_LOG=info viaduct --debug 2>&1 | grep memory
```

## Adding new instrumentation

When porting code from NetNewsWire that does heavy work (parsing,
diffing, DB access), wrap the hot path in a timing block and log under
`viaduct::perf`:

```rust
let t = std::time::Instant::now();
let result = expensive_thing();
let elapsed_ms = t.elapsed().as_millis() as u64;
if elapsed_ms >= 100 {
    tracing::warn!(target: "viaduct::perf", elapsed_ms, "expensive_thing slow");
} else {
    tracing::debug!(target: "viaduct::perf", elapsed_ms, "expensive_thing");
}
```

The `target:` argument routes the line to a custom filter target so
callers can scope `RUST_LOG` to just the perf channel without seeing
unrelated debug output.
