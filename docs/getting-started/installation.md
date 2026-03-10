# Installation

## Requirements

- **Rust 1.85+** (edition 2024)
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
