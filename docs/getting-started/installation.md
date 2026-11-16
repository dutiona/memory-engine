# Installation

## Requirements

- **Rust 1.89+** (edition 2024)
- No external services — SQLite is bundled via `rusqlite`

## Add to Cargo.toml

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine" }
```

### Async support

`MemoryEngine` is async-native: its DB-touching methods are `async fn` and are
awaited inside a tokio runtime (`#[tokio::main]` or `Runtime::block_on`).
`tokio` is a non-optional dependency, so there is no feature to enable.

If you disable default features, re-enable a storage backend (at least one is
required):

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine", default-features = false, features = ["backend-sqlite"] }
```

### ANN (Approximate Nearest Neighbor)

Enable the `ann` feature for HNSW-accelerated vector search at scale:

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine", features = ["ann"] }
```

This adds the `hnsw`, `space`, and `rand` crates as dependencies. Without this feature, vector search uses brute-force only (zero additional dependencies). See [Hybrid Search — ANN](../advanced/hybrid-search.md#ann-approximate-nearest-neighbor) for configuration details.

## Build

```bash
cargo build
```

The first build compiles SQLite from source (bundled via `rusqlite`). Subsequent builds are cached.

## Verify

```bash
cargo test
```

All tests run against in-memory SQLite databases — no setup required.
