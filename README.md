# hnx

`hnx` is a fast, cache-first Hacker News client for the terminal. Installing
the crate creates the `hnx` command.

It combines an adaptive Ratatui interface with stable text and JSON commands.
Cached results render immediately, refresh asynchronously, and remain usable
offline with visible freshness and source metadata.

## Why a new Rust client?

Rust improves cold start, resident memory, packaging, and terminal safety. The
largest performance gain, however, comes from changing request topology:
Firebase supplies canonical ranked IDs, Algolia fills most records in one
batch, and bounded Firebase fan-out fills only the gaps. Comment traversal is
cycle-safe and capped by time, depth, node count, response size, and at most 12
concurrent requests.

On the development Mac, the legacy request shape took 3.35–3.67 seconds for 30
serial Firebase story requests. The bounded concurrent equivalent took
0.39–0.45 seconds (about 8.5x faster), and one Algolia front-page request took
0.22–0.41 seconds. These are directional measurements, not universal network
guarantees; the reproducible budgets live in [docs/PERFORMANCE.md](docs/PERFORMANCE.md).

## Install

During development:

```bash
cargo install --path .
```

After publication:

```bash
cargo install hnx
```

## Use

Launch the interactive client:

```bash
hnx
```

Use the headless interface:

```bash
hnx feed top --limit 30
hnx feed jobs --format json
hnx item 8863 --comments
hnx search "rust terminal" --type story --format json
hnx hiring rust
hnx freelance design
hnx --offline feed top
hnx cache stats
```

Global options include `--offline`, `--theme <name-or-path>`, and
`--log-file <path>`. Run `hnx --help` for the complete command surface.

JSON output has a versioned envelope:

```json
{
  "schema_version": 1,
  "source": "algolia",
  "stale": false,
  "fetched_at": 1786334400,
  "data": []
}
```

Exit status `0` means usable output, including explicitly marked stale cache;
`2` means invalid input; `3` means the requested data was unavailable from
both cache and network. Thread JSON includes `data.metadata.partial`, loaded,
declared, omitted, and unresolved counts. Stdout contains only requested data.

## TUI

The interface adapts without discarding selection or scroll state:

- 80 columns and wider: story list plus the active thread or detail pane.
- Below 80 columns: one focused pane.

Use arrow keys or `j`/`k` to move, `Ctrl+U`/`Ctrl+D` to move half a viewport,
and Page Up/Page Down to move a full viewport. `Tab` or `h`/`l` moves between
the story list and active right pane; `t` selects the thread and `d` selects
detail. `Enter` loads a thread or folds a comment, `/` searches, `f` applies a
case-insensitive regex filter, `b` bookmarks, `a` reads an article in-terminal,
`o` opens a link, `O` toggles offline mode, `r` refreshes, `?` opens help, and
`q` quits. The status line always identifies offline/stale/partial data and its
source.

The default `classic` theme uses Y Combinator orange (`#FF6600`) and Hacker
News cream (`#F7F6F0`). `midnight`, an ANSI 16-color theme, `NO_COLOR`, and
custom semantic TOML themes are supported.

## Development

The project uses Rust 2024 with MSRV 1.88 and pins the development toolchain.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo bench
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[docs/PERFORMANCE.md](docs/PERFORMANCE.md), and
[docs/DEPENDENCIES.md](docs/DEPENDENCIES.md).

## Provenance and naming

This is an original clean-room implementation informed by the public behavior
of the projects listed in [ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md). No source
from those projects is included.

Both the project and its command are named `hnx`. That is deliberate:
[haxor-news](https://github.com/donnemartin/haxor-news) already installs a `hn`
command, and shipping a second binary under the same name would collide on
`PATH` and invite confusion about which project a user is running. `hnx` is
distinct at every layer — crate name, command name, and repository.

Copyright is not the concern here. The
[U.S. Copyright Office](https://www.copyright.gov/help/faq/faq-protect.html)
states that names and short phrases are not protected by copyright; it applies
to original expression such as source code, and none of that is shared. The
upstream project is
[Apache-2.0 licensed](https://github.com/donnemartin/haxor-news/blob/master/LICENSE.txt),
and [Apache-2.0 section 6](https://www.apache.org/licenses/LICENSE-2.0) does not
grant general trademark rights, so naming can still raise trademark or
passing-off questions independent of copyright. Using a distinct command name
and stating clearly that this project is independent and unaffiliated addresses
both. This is practical U.S.-focused project guidance, not legal advice.

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option. `hnx` is independent and is not affiliated with or endorsed by
Y Combinator.
