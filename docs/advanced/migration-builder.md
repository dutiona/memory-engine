# Migrating to `MemoryEngine::builder`

The telescoping `MemoryEngine::open*` constructors were removed in favour of a
single typestate builder (issue #541). `EngineConfig`'s fields were sealed; build
one with `EngineConfig::new` + the `with_*` chain. This is a one-time, mechanical
upgrade.

## Constructors → builder

| Removed constructor                                     | New call                                                          |
| ------------------------------------------------------- | ----------------------------------------------------------------- |
| `MemoryEngine::open_memory(d)`                          | `MemoryEngine::builder(d).build()`                                |
| `MemoryEngine::open_memory_with_config(d, Some(sc))`    | `MemoryEngine::builder(d).search_config(sc).build()`              |
| `MemoryEngine::open_memory_with(d, Some(sc), Some(rr))` | `MemoryEngine::builder(d).search_config(sc).reranker(rr).build()` |
| `MemoryEngine::open(&cfg)` (file)                       | `MemoryEngine::builder(d).path(p).build()`                        |
| `MemoryEngine::open_with_reranker(&cfg, Some(rr))`      | `MemoryEngine::builder(d).path(p).reranker(rr).build()`           |

`.path(p)` switches the builder to file-backed and unlocks the file-only setters
`.read_only(bool)`, `.backup_dir(dir)`, `.read_pool_size(n)`. Those setters do
**not** exist before `.path()` — calling `.read_only(true)` on an in-memory
builder is a _compile error_ (it was previously a runtime contradiction).

```rust
use memory_engine::MemoryEngine;

// In-memory (the common path):
let engine = MemoryEngine::builder(384).build()?;

// File-backed, read-only, custom pool:
let engine = MemoryEngine::builder(384)
    .path("agent.db")
    .read_only(true)
    .read_pool_size(2)
    .build()?;
# Ok::<(), memory_engine::MemoryError>(())
```

## `EngineConfig` field mutation → `with_*`

`EngineConfig`'s fields are now `pub(crate)`; the struct is `#[non_exhaustive]`.
Replace struct literals and field assignment with `new` + chained setters:

```rust
use memory_engine::EngineConfig;

// Before:  let mut c = EngineConfig::new(path, dim); c.read_only = true;
// After:
let config = EngineConfig::new(path, dim).with_read_only(true);
```

Setters: `with_read_pool_size`, `with_search_config`, `with_backup_dir`,
`with_upcaster_registry`, `with_read_only`. `EngineConfig` is still what the
`restore_*` family consumes (and, internally, the builder); for the common
file-open path prefer the builder, which assembles a config internally.

## Async

Construction stays **synchronous**: `MemoryEngine::builder(dim).build()` and the
`restore_*` family build the pool/backend without awaiting. Only the engine's
**runtime, DB-touching methods** (`add_fact`, `query`, `statistics`, `close`, …)
are `async fn` — they `.await` the `Arc<dyn StorageBackend>` port.

### Async-native cutover (#631)

`MemoryEngine` is now async-native: its DB-touching methods are `async fn` that
`.await` an `Arc<dyn StorageBackend>` port. There is no separate
`AsyncMemoryEngine` wrapper anymore — you use `MemoryEngine` directly and await
it from inside a tokio runtime (`#[tokio::main]` or `Runtime::block_on`). The
`async` Cargo feature is now **default-on**: it no longer gates a wrapper type,
it provides the tokio runtime the async-native engine needs.

```rust
use memory_engine::MemoryEngine;

#[tokio::main]
async fn main() -> Result<(), memory_engine::MemoryError> {
    let mut engine = MemoryEngine::builder(384).build()?; // construction is synchronous
    // ... await the engine's runtime methods (add_fact, query, …) ...
    engine.close().await?; // close is async — it flushes the sidecar snapshot
    Ok(())
}
```

Call `MemoryEngine::close(&mut self).await` for a clean shutdown — it flushes the
sidecar HNSW/snapshot. `Drop` is now warn-only: an engine dropped without
`close()` is still durable (the source of truth is the DB), but it rebuilds its
sidecar from the DB on the next open instead of loading the flushed snapshot.
