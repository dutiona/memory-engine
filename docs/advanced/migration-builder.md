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
`restore_*` family and `AsyncMemoryEngine::open` consume; for the common
file-open path prefer the builder, which assembles a config internally.

## Async

`AsyncMemoryEngine::open(config)` and `AsyncMemoryEngine::open_memory(dim)` are
unchanged — only their internals now route through the builder.
