# Direct dependencies

Every runtime dependency has a narrow role:

| Crate | Purpose |
| --- | --- |
| `clap` | Typed command-line parsing and generated help. |
| `crossterm` | Cross-platform terminal input and lifecycle. |
| `directories` | Platform-correct cache and configuration paths. |
| `futures` | Event streams and bounded asynchronous composition. |
| `html2text` | Non-JavaScript article conversion. |
| `open` | Browser handoff through the operating system. |
| `ratatui` | Stateful, diff-based terminal rendering and rendered-line measurement for content-aware scroll bounds. |
| `regex` | Linear-time story filtering. |
| `reqwest` | Pooled HTTPS with Rustls, guarded DNS, decompression, and streaming limits. |
| `rusqlite` | Bundled SQLite cache and local state. |
| `serde`, `serde_json`, `toml` | API, cache, JSON envelope, and theme formats. |
| `thiserror` | Typed library and command errors. |
| `tokio` | Async orchestration, channels, and signal handling. |
| `tracing`, `tracing-subscriber` | Local opt-in diagnostics; no telemetry. |
| `unicode-segmentation` | Grapheme-safe wrapping for terminal comment text. |
| `url` | Strict URL parsing and scheme checks. |

Test-only crates provide statistical benchmarks, temporary storage, and
deterministic HTTP mocks. Dependency versions are recorded in `Cargo.lock`;
Dependabot and dependency-policy CI keep them current.
