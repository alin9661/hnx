# Cache architecture decision

Status: accepted for the local terminal client.

`hnx` keeps SQLite as its durable cache and local-state store. This is not a
claim that SQLite has the lowest possible lookup latency. It is the best fit
for the whole workload: a single-user, cache-first application that must start
without a service, work offline, preserve user state, and coordinate occasional
commands from more than one process.

## Workload and performance evidence

The active feed, thread, and article live in `App` memory. Rendering therefore
does not query SQLite; cache reads occur when a command starts or the user
changes context. The relevant optimization target is end-to-end startup and
data availability, not millions of key lookups per second.

The release benchmark records a warm offline Top command at **4.9 ms mean per
process**. See [the performance contract](PERFORMANCE.md) for the environment,
other measurements, and reproduction command. That result already includes
process startup, argument parsing, opening SQLite, decoding the snapshot, and
writing JSON; it does not isolate storage time. An ideal backend still cannot
eliminate startup, parsing, decoding, or output, so profile those components
before attributing total command time to storage.

Network topology remains the dominant cold-path optimization: canonical
Firebase IDs, one Algolia batch, and bounded Firebase gap filling remove serial
round trips. Cache backend work should be justified by a measured regression
or a new deployment model.

## What SQLite currently provides

The schema stores four TTL-managed network snapshots and three durable kinds of
user state:

| Data | Key | Lifetime |
| --- | --- | --- |
| Feed | feed kind | TTL, readable when stale |
| Item | Hacker News item ID | TTL, readable when stale |
| Thread | root item ID | TTL, readable when stale |
| Search | search type + trimmed query | TTL, readable when stale |
| Bookmark | item ID | durable until explicitly removed |
| Read state | item ID | durable until explicitly toggled or cleared |
| Setting | setting name | durable until explicitly removed |

The implementation also provides:

- schema migrations inside an immediate transaction;
- transactional feed/search fan-out into the item cache;
- expiry indexes and explicit fresh-versus-stale reads;
- persisted covered-slot and available-item counts so a cached `--limit 1`
  result cannot incorrectly satisfy a later `--limit 30` request and missing
  upstream records do not compress canonical ranks;
- freshness-aware replacement: a newer smaller prefix may replace a stale
  larger page, while older writes cannot win and equal-or-greater coverage
  cannot reduce the number of available items;
- a 16 MiB limit for each serialized value;
- WAL mode with `synchronous = NORMAL` for file-backed databases;
- a five-second busy timeout and SQLite locking across local processes; and
- separate `clear` and `clear_all` operations so ordinary cache maintenance
  cannot erase bookmarks, read state, or settings.

SQLite is [serverless and zero-configuration](https://www.sqlite.org/serverless.html),
which matters for a distributable terminal binary. Its own guidance recommends
the client/server model when many machines write the same database or when
write concurrency is high; neither condition describes `hnx` today. See
[Appropriate Uses for SQLite](https://www.sqlite.org/whentouse.html).

### WAL size alone is not retained-row growth

SQLite's write-ahead log is a reusable sidecar. A WAL near its automatic
checkpoint threshold can be larger than the main database without indicating
that rows are leaking. A lower checkpoint threshold or `journal_size_limit`
can make the directory look smaller, but it also checkpoints more often. That
is an operational tuning choice, not a backend-performance win. The
[SQLite WAL documentation](https://sqlite.org/wal.html) describes checkpoint
and reuse behavior.

## Highest-value cache optimization

Keep SQLite and add a **bounded stale-retention policy with logical-size
budgets and physical-size observability** when cache growth becomes measurable.

Today the 16 MiB ceiling applies per row, and expired rows remain available for
offline fallback until `hnx cache prune` runs. There is no total logical cache
budget. The recommended next implementation should:

1. report logical payload bytes plus main database, WAL, shared-memory, and
   total physical bytes in `hnx cache stats`;
2. preserve recent expired snapshots for useful offline fallback;
3. prune the oldest eligible feed, item, thread, and search rows only after a
   configurable logical-payload or stale-age threshold is crossed;
4. never evict bookmarks, read state, or settings;
5. run maintenance transactionally and idempotently;
6. consider checkpointing or `VACUUM`/incremental vacuum separately, and only
   when measurements show that reclaiming physical space is worth its I/O; and
7. move maintenance to `spawn_blocking` or a dedicated cache actor only if
   profiling shows it delaying the async event loop.

Physical bytes should not directly trigger repeated row eviction. SQLite can
reuse pages freed by `DELETE` without immediately shrinking the database file,
and WAL checkpointing is a separate lifecycle. Logical size and stale age tell
the application whether data should be retained; physical size tells the user
how storage is represented on disk.

Avoid updating a `last_accessed` value on every read. That turns read-heavy
startup into extra writes, grows the WAL, and increases contention. Existing
`fetched_at` and `expires_at` values provide a stable, low-write eviction order.

Tests for this work should prove that recent stale fallback survives, the
oldest eligible network rows leave first, bookmarks/read state/settings survive,
and repeated maintenance converges to the same result.

## Why not Redis or Valkey?

Redis and Valkey are excellent shared caches. They are not free in a local CLI:
every lookup adds a socket round trip and serialization, and installation now
requires a running daemon, configuration, lifecycle management, and failure
handling. [Pipelining](https://redis.io/docs/latest/develop/using-commands/pipelining/)
amortizes round-trip cost for batches, but it does not remove socket and
protocol costs from the cache-first reads on a local `hnx` startup path.

They do offer capabilities SQLite does not:

- native TTL and configurable
  [max-memory eviction](https://redis.io/docs/latest/develop/reference/eviction/);
- centralized invalidation shared by processes or hosts;
- high concurrent request throughput; and
- operationally selectable persistence using
  [RDB snapshots, AOF, or both](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/).

Those persistence modes introduce an explicit durability/latency tradeoff.
Eviction also means user-owned data cannot safely share an evictable namespace.
If `hnx` became a service, a reasonable split would be:

```text
Redis/Valkey (evictable)                 SQLite or another durable store
hnx:feed:<feed>                          bookmarks
hnx:item:<id>                            settings
                                         read state
hnx:thread:<id>
hnx:search:<query-hash>
```

Feed and search writes would use a pipeline plus `MULTI`/`EXEC` or a server-side
script so the page, completeness metadata, and item fan-out become visible
together. The application would need an explicit policy for stale fallback,
because key expiry normally removes the old value instead of retaining it as a
marked stale snapshot. One design would store `fresh_until` in metadata and set
the actual key expiry to a later `retain_until` horizon.

Redis/Valkey becomes the better design only if `hnx` changes into a shared
service with multiple hosts or many concurrent workers, centralized cache
invalidation, and a request rate high enough to justify operating a server.
For a remote deployment, authentication, TLS, network partitions, and cache
stampede control also become part of the design.

Licensing is a deployment consideration: current Redis releases are offered
under the licenses listed on the
[Redis licensing page](https://redis.io/legal/licenses/). Valkey is the
BSD-licensed fork and documents similar
[eviction](https://valkey.io/topics/lru-cache/) and
[persistence](https://valkey.io/topics/persistence/) mechanisms.

## Other alternatives

| Backend | Where it wins | Cost for `hnx` | Decision |
| --- | --- | --- | --- |
| `HashMap`, LRU, or [Moka](https://github.com/moka-rs/moka) | Lowest in-process lookup latency; useful for very hot repeated reads | Lost on exit, cannot provide offline startup, duplicates active `App` state | Add an L1 only after profiling proves repeated SQLite reads are hot |
| JSON or MessagePack files | Minimal format and tooling for one immutable blob | Must rebuild transactions, indexes, migrations, atomic multi-record writes, locking, and pruning | Worse reliability for little speed benefit |
| [redb](https://github.com/cberner/redb) | Pure-Rust embedded ACID/MVCC key-value storage and fast local reads | Rebuild expiry indexes, migrations, bookmark ordering, completeness queries, and inspection tools | Strongest embedded candidate if SQLite becomes a measured bottleneck |
| [heed/LMDB](https://github.com/meilisearch/heed) | Excellent memory-mapped reads | Map sizing, single-writer behavior, native/unsafe environment setup, and custom indexing | Unnecessary complexity for the present write rate |
| [sled](https://github.com/spacejam/sled) | Embedded ordered key-value API | Project documents beta status and an unstable on-disk format | Do not use for durable user state |
| RocksDB | High write throughput, LSM tuning, compression, and large datasets | Native build, larger binary, background compaction, and operational tuning | Designed for a scale far beyond this cache |
| HTTP cache middleware | Reuses cacheable raw HTTP responses | Cannot represent normalized hybrid snapshots, canonical ordering, completeness, source, or marked-stale semantics | Possible supplementary layer, not a replacement |

## Revisit triggers

Re-evaluate the backend when evidence shows one of these conditions:

- cache operations materially exceed the 100 ms warm-command budget;
- profiles attribute meaningful event-loop stalls to SQLite rather than JSON,
  process startup, rendering, or network work;
- the retained dataset grows by orders of magnitude;
- concurrent writers routinely exhaust the busy timeout; or
- one cache must be shared across hosts.

Until then, SQLite minimizes total system cost while meeting the performance,
offline, durability, and packaging requirements.
