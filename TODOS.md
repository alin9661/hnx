# TODOS

## Performance

### Fetch only missing pagination slots

**What:** Replace cumulative 30/60/90-item feed and search downloads with
range-aware fetching or cached-row reuse.

**Why:** Moving through later pages currently re-downloads the complete ranked
prefix, and a previously loaded 500-slot prefix makes later refreshes request
all 500 slots again.

**Context:** `page_limit` and `refresh_limit` in `src/cli.rs` intentionally
request a cumulative prefix so canonical ranks remain stable. Preserve that
ordering contract while teaching `HybridClient` or the cache layer to fetch
only missing and newly refreshed rows.

**Effort:** M
**Priority:** P2
**Depends on:** None

### Move interactive SQLite writes off the async TUI loop

**What:** Send bookmark, read-state, and layout persistence through
`spawn_blocking` or a dedicated persistence worker.

**Why:** SQLite uses a five-second busy timeout, so a second hnx process holding
the database lock can temporarily pause keyboard input and refresh animation.

**Context:** `run_tui` in `src/cli.rs` currently calls the synchronous cache
methods directly for small writes. Preserve immediate in-memory UI feedback,
surface persistence failures asynchronously, and serialize writes without
losing the most recent layout or read state.

**Effort:** M
**Priority:** P2
**Depends on:** None

## Completed
