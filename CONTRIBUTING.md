# Contributing

Contributions are welcome. Keep changes focused, include tests for behavior, and
do not copy source from other Hacker News clients. Product behavior may be used
as inspiration, but implementation must remain original or be accompanied by a
compatible license and explicit provenance.

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Performance-sensitive changes should include a Criterion comparison or a
reproducible timing command. Do not report a speedup without a baseline.

