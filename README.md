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

Or install the latest release binary on macOS or Linux:

```bash
bash -o pipefail -c 'curl --proto "=https" --tlsv1.2 -LsSf https://github.com/alin9661/hnx/releases/latest/download/hnx-installer.sh | sh'
```

`pipefail` makes a failed download fail the full install command instead of
letting an empty `sh` invocation report success.

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

Global options include `--offline`, `--theme <name-or-path>`, `--config <path>`,
`--layout two[:STORIES]`, `--layout three[:STORIES,THREAD]`, and
`--layout reset`. Run `hnx --help` for the complete command surface.

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

The default two-pane interface adapts without discarding loaded data,
selection, folded comments, offsets, or article scroll state:

- 80 columns and wider: story list plus the active thread or detail pane.
- Below 80 columns: one focused pane.

Press `L` for the optional three-pane Stories → Thread → Detail view. It uses
three panes at 120 columns and wider, falls back to the configured two-pane
view from 80–119 columns, then to the focused pane below 80 columns. Any layout
falls back further rather than rendering a pane narrower than 18 columns.

Use arrow keys or `j`/`k` to move, `Ctrl+U`/`Ctrl+D` to move half a viewport,
and Page Up/Page Down to move a full viewport. `Tab`/`BackTab` cycles visible
panes; `h`/`l` moves spatially and stops at the edges. `Alt+h`/`Alt+l` shrinks
or grows the focused pane by two percentage points, and `Alt+0` clears saved
live overrides. `t` selects the thread and `d` selects detail. `Enter` loads a
thread or folds a comment, `/` searches, `f` applies a
case-insensitive regex filter, `b` bookmarks, `a` reads an article in-terminal,
`o` opens a link, `O` toggles offline mode, `r` refreshes, and `n`/`p` load the
next/previous 30-story page. Opening a story marks it read; `m` toggles
read/unread, and read state persists across launches. `?` opens help, and `q`
quits or closes help. The status line always identifies offline/stale/partial
data and its source. Comment bodies reflow to the live thread-pane width.

The default `classic` theme uses Y Combinator orange (`#FF6600`) and Hacker
News cream (`#F7F6F0`). `midnight`, an ANSI 16-color theme, `NO_COLOR`, and
custom semantic TOML themes are supported.

Layout preferences are loaded in this order: explicit CLI, saved SQLite
`layout.v1` state, platform `hnx/config.toml`, then built-in defaults. The
platform file accepts:

```toml
[layout]
mode = "two"
two = [44, 56]
three = [38, 34, 28]
two_min_width = 80
three_min_width = 120
```

An explicitly requested invalid config exits with status 2. Invalid automatic
or saved preferences fall back as a complete unit and produce one status-line
warning. `--layout reset` removes the saved override so TOML changes take
effect again.

## Development

The project uses Rust 2024 with MSRV 1.88 and pins the development toolchain.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo bench
```

Project guides: [architecture](docs/ARCHITECTURE.md),
[dependencies](docs/DEPENDENCIES.md), [performance](docs/PERFORMANCE.md),
[themes](docs/THEMES.md), [releasing](docs/RELEASING.md),
[contributing](CONTRIBUTING.md), the [security policy](SECURITY.md), and the
[changelog](CHANGELOG.md).

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
