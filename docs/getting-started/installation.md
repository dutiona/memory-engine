# Installation

## Requirements

- **Rust 1.88+** (edition 2024)
- No external services — SQLite is bundled via `rusqlite`

## Add to Cargo.toml

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine" }
```

### Async support

Enable the `async` feature for `AsyncMemoryEngine` (requires tokio):

```toml
[dependencies]
memory-engine = { git = "https://github.com/dutiona/memory-engine", features = ["async"] }
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
