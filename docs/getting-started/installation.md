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

This adds the `space` and `rand` crates and the vendored `hnsw` 0.11.0 (`vendor/hnsw`, carried with the #1074 fix) as dependencies. Without this feature, vector search uses brute-force only (zero additional dependencies). See [Hybrid Search — ANN](../advanced/hybrid-search.md#ann-approximate-nearest-neighbor) for configuration details.

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

or from an archive of the tag, which builds exactly its committed tree:

```bash
git archive --format=tar <tag> | podman build --pull=newer -t memory-engine:<tag> -
```

- `--pull=newer` refreshes the floating base images (`rust:1-slim-trixie`, `debian:trixie-slim`). Podman's default
  (`missing`) keeps the first copy it pulled, so later tags would build on a stale toolchain and ship stale OpenSSL,
  `curl` and CA certificates.
- The release build runs with `--locked` from the committed `Cargo.lock`; a lock that no longer matches the
  manifests fails the build.
- Only the `apt-get` steps (Debian-signed packages from the Debian archive) and `cargo fetch --locked` (downloads
  the locked crates, runs none of their code) have network. The release build, where every dependency's build script
  and proc-macro runs, and the `--version` smoke run under `RUN --network=none`: their network namespace holds only
  loopback, so code that only needs to compile cannot open a connection from the build host. `--offline` separately
  makes cargo fail on a crate the fetch missed instead of trying to download it.
- The build context is an allowlist (`.containerignore`): only `Cargo.toml`, `Cargo.lock`, `crates/` and `vendor/`.
  Docker ignores `.containerignore` and would send the whole tree, so build with Podman (or Buildah).
- The runtime is Debian slim with CA certificates, the `sqlite3` CLI and `curl`.
- The image runs as the non-root user `memory-engine`, UID and GID `10001`, in `/var/lib/memory-engine`, with
  `memory-engine-mcp` as its entry point. Run the CLI with `--entrypoint memory-engine-cli`.
- The entry point serves MCP over HTTP and needs `MEMORY_MCP_LISTEN` (`0.0.0.0:<port>` inside a container, so a
  published port reaches it), `MEMORY_MCP_TOKEN_FILE` and `MEMORY_MCP_USER` (the owner, a lowercase DNS label)
  ([MCP server reference](../reference/mcp-server.md#transport-and-authentication)).
- Every route requires a machine token, so a health probe expects 401:
  `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:<port>/mcp` prints `401`.
- `podman stop` sends SIGTERM: the server drains its tool calls, flushes its snapshot and exits 0.
- The server holds `<db>.store-lock` while it runs. A CLI container on the same volume refuses every writable command
  (`prune`, `unforget`, `erase`, `migrate`, `add-fact`, …) until the server stops; read-only commands and `export` run beside it.

How the image is deployed (volumes, ports, secrets) is outside this repository.
