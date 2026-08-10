# Architecture

`hnx` has five boundaries:

1. `api` obtains normalized Hacker News data. Firebase supplies canonical feed
   identity/order; Algolia supplies batched records, and bounded Firebase
   requests reconcile gaps. Threads are traversed with cycle, depth, node,
   deadline, response-size, and concurrency limits.
2. `cache` persists normalized payloads and freshness metadata in SQLite.
3. `app` is a deterministic state machine. It does not perform terminal I/O.
4. `ui` renders an `app` snapshot into adaptive Ratatui layouts.
5. `cli` coordinates cache-first reads, background refreshes, machine-readable
   output, and terminal lifecycle.

This split keeps network latency out of rendering and lets the CLI reuse the
same data model without starting the TUI. A render is proportional to visible
rows, not the entire comment tree.

## Data flow

```text
command/event -> complete cache snapshot -> app state -> indexed viewport -> terminal
                         |                ^
                         +-> async refresh+
                             Firebase IDs + Algolia batch + bounded gap fill
```

## Clean-room provenance

The implementation is original. The projects named in `ACKNOWLEDGMENTS.md`
were examined for public behavior and product ideas only; their source is not
incorporated here.
