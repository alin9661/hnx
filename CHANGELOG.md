# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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
[0.1.0]: https://github.com/alin9661/hnx/releases/tag/v0.1.0
