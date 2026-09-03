# Honk SQL parser spike

This crate tests whether `sqlparser-rs` can enforce Honk's V1 read-only SQL
policy for Athena engine version 3. It has no AWS dependencies and makes no
network calls at runtime.

Run it with:

```bash
cargo test --manifest-path spikes/sqlparser/Cargo.toml
cargo clippy --manifest-path spikes/sqlparser/Cargo.toml --all-targets -- -D warnings
```

The production-shaped validator is in `src/lib.rs`. The main Athena and attack
corpus is in `tests/corpus.rs`. `tests/alternatives.rs` records the reasons for
rejecting `sqlglot-rust` as the V1 parser.

The spike intentionally rejects Athena Iceberg time-travel syntax. See
[`docs/decisions/0001-sql-parser.md`](../../docs/decisions/0001-sql-parser.md)
for the measured results and recommendation.
