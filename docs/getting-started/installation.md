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

## Server image

The repository's `Containerfile` builds a self-contained image of the MCP server (`memory-engine-mcp`) and the
operator CLI (`memory-engine-cli`). Build it with Podman from a checkout of a release tag:

```bash
podman build --pull=newer -t memory-engine:<tag> .
```

- `--pull=newer` refreshes the floating base images (`rust:1-slim-trixie`, `debian:trixie-slim`). Podman's default
  (`missing`) keeps the first copy it pulled, so later tags would build on a stale toolchain and ship stale OpenSSL,
  `curl` and CA certificates.
- The release build runs with `--locked` from the committed `Cargo.lock`; a lock that no longer matches the
  manifests fails the build.
- The build context is an allowlist (`.containerignore`): only `Cargo.toml`, `Cargo.lock` and `crates/`. Docker
  ignores `.containerignore` and would send the whole tree, so build with Podman (or Buildah).
- The runtime is Debian slim with CA certificates, the `sqlite3` CLI and `curl`.
- The image runs as the non-root user `memory-engine`, UID and GID `10001`, in `/var/lib/memory-engine`, with
  `memory-engine-mcp` as its entry point. Run the CLI with `--entrypoint memory-engine-cli`.

How the image is deployed (volumes, ports, secrets) is outside this repository.
