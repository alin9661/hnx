# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/). Four-part gstack ship versions
`A.B.C.D` map to Cargo package version `A.B.C`.

## [Unreleased]

## [0.1.0.0] - 2026-08-10

### Added

- Half-page `Ctrl+U`/`Ctrl+D` and full-page Page Up/Page Down navigation for
  stories, comments, and wrapped Detail content.
- A filled, customizable `hnx` brand chip using `accent` and `accent_fg` theme
  roles, with accessible foreground and border roles across built-in themes.

### Changed

- Replaced the wide three-pane view with a 44/56 two-panel layout that keeps
  Stories beside the active Thread or Detail pane at 80 columns and wider.
- Updated Classic to Y Combinator orange (`#FF6600`) on Hacker News cream
  (`#F7F6F0`), using black text on orange and darker orange for foregrounds.
- Detail paging now stops at the last wrapped row and reclamps after terminal
  width or height changes, preventing blank overscroll.

## [0.1.0] - 2026-08-10

### Added

- Cache-first Hacker News feeds, search, items, and threaded comments.
- Responsive Ratatui interface with classic, midnight, ANSI, and custom themes.
- Stable text and JSON commands through the `hnx` executable, named to avoid
  colliding with haxor-news' existing `hn` command.
- Canonical Hybrid Top feeds with batched Algolia records and bounded Firebase
  reconciliation.
- Cycle/depth/node/deadline-limited thread traversal with explicit partial
  metadata.
- Completeness-aware SQLite caching, offline item fan-out, and stale-while-
  revalidate TUI loading.
- DNS-rebinding-safe, bounded in-terminal article reading.
- Checksummed multi-platform cargo-dist releases with SBOMs and attestations.

[Unreleased]: https://github.com/alin9661/hnx/compare/v0.1.0...HEAD
[0.1.0.0]: https://github.com/alin9661/hnx/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/alin9661/hnx/releases/tag/v0.1.0
