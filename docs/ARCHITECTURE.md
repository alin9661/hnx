# Architecture

`hnx` has six boundaries:

1. `api` obtains normalized Hacker News data. Firebase supplies canonical feed
   identity/order; Algolia supplies batched records, and bounded Firebase
   requests reconcile gaps. Threads are traversed with cycle, depth, node,
   deadline, response-size, and concurrency limits.
2. `cache` persists normalized payloads and freshness metadata in SQLite.
3. `config` and `layout` validate preferences, resolve precedence, and compute
   responsive rectangles without terminal or database side effects.
4. `app` is a deterministic state machine. It does not perform terminal or
   SQLite I/O; layout mutations return persistence actions to the caller.
5. `ui` renders `app` state into adaptive Ratatui layouts and records rendered
   viewport/content metrics back into `App` so Detail scrolling stays bounded
   after content wrapping or terminal resize. It also records the visible pane
   set so navigation cannot focus a responsive-hidden pane.
6. `cli` coordinates cache-first reads, background refreshes, machine-readable
   output, and terminal lifecycle.

This split keeps network latency out of rendering and lets the CLI reuse the
same data model without starting the TUI. A render is proportional to visible
rows, not the entire comment tree.

## Data flow

```text
command/event -> complete cache snapshot -> app state -> resolved layout -> terminal
                         |                ^          |
                         +-> async refresh+          +-> viewport/pane metrics
                             Firebase IDs + Algolia batch + bounded gap fill
```

## Clean-room provenance

The implementation is original. The projects named in `ACKNOWLEDGMENTS.md`
were examined for public behavior and product ideas only; their source is not
incorporated here.
