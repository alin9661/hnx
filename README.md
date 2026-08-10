# hnx

`hnx` is a fast, cache-first Hacker News client for the terminal. Installing
the crate creates the short `hn` command.

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
hn
```

Use the headless interface:

```bash
hn feed top --limit 30
hn feed jobs --format json
hn item 8863 --comments
hn search "rust terminal" --type story --format json
hn hiring rust
hn freelance design
hn --offline feed top
hn cache stats
```

Global options include `--offline`, `--theme <name-or-path>`, and
`--log-file <path>`. Run `hn --help` for the complete command surface.

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

- 120 columns and wider: navigation, story list, and reading/thread panes.
- 80–119 columns: story list plus active detail pane.
- Below 80 columns: one focused pane.

Use arrow keys or `j`/`k` to move, `Tab` to change panes, `Enter` to load a
thread or fold a comment, `/` to search, `f` to apply a case-insensitive regex
filter, `b` to bookmark, `a` to read an article in-terminal, `o` to open a
link, `O` to toggle offline mode, `r` to refresh, `?` for help, and `q` to
quit. The status line always identifies offline/stale/partial data and its
source.

The default `classic` theme uses Hacker News orange and cream. `midnight`, an
ANSI 16-color theme, `NO_COLOR`, and custom semantic TOML themes are supported.

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

The [U.S. Copyright Office](https://www.copyright.gov/help/faq/faq-protect.html)
states that names and short phrases are not protected by copyright, so a short
command such as `hn` is not itself a copyright issue. Copyright instead applies
to original expression such as source code. The upstream project is
[Apache-2.0 licensed](https://github.com/donnemartin/haxor-news/blob/master/LICENSE.txt),
but [Apache-2.0 section 6](https://www.apache.org/licenses/LICENSE-2.0) does not
grant general trademark rights. Naming can therefore still raise trademark or
passing-off questions. The public project name is `hnx`, and it states clearly
that it is independent and unaffiliated. This is practical U.S.-focused project
guidance, not legal advice.

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option. `hnx` is independent and is not affiliated with or endorsed by
Y Combinator.
