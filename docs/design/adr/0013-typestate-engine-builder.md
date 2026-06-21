# 13. Typestate builder replaces telescoping `MemoryEngine` constructors

Date: 2026-06-16

## Status

Accepted. Supersedes the facade builder introduced in #543 (PR for #113/#149).

## Context

`MemoryEngine` exposed five telescoping constructors that grew by parameter
combination: `open`, `open_memory`, `open_memory_with_config`,
`open_with_reranker`, `open_memory_with`. Each new optional capability risked
another `*_with_*` variant — the classic telescoping-constructor anti-pattern
(#541, duplicate #113). `EngineConfig` was constructed via `new` + public-field
mutation, with no forward-compatibility guard (#149).

PR #543 added a _facade_ `MemoryEngineBuilder` (a single flat struct delegating
to the existing constructors) and marked `EngineConfig` `#[non_exhaustive]`, but
**kept** all the constructors and left `EngineConfig`'s fields public. That
closed #113/#149 ergonomically but did not satisfy #541's actual requirement —
_replace_ (remove) the telescoping constructors — and it re-encoded an invalid
state at runtime: `builder(d).read_only(true)` on an in-memory engine returned
`Err` instead of being unrepresentable.

## Decision

Replace the facade with a **typestate** builder and remove the constructors.

1. `MemoryEngine::builder(embed_dim) -> MemoryEngineBuilder<InMemory>`.
   `.path(p)` transitions to `MemoryEngineBuilder<File>`. The file-only knobs
   (`read_only`, `backup_dir`, `read_pool_size`) exist **only** on the `File`
   state, so `builder(d).read_only(true)` is a **compile error** — the
   "in-memory engine with a file knob" state is structurally unrepresentable
   (split-state payload: `InMemory` is a ZST, `File` carries `path` + knobs).
2. Consuming `self -> Self` setters; `build(self) -> Result<MemoryEngine>`. The
   builder owns a non-`Clone` `Box<dyn Reranker>` and is therefore not `Clone` —
   no `Clone` bound leaks onto consumer `Reranker` impls.
3. Remove all five constructors. A private `open_from_config(&EngineConfig,
Option<Box<dyn Reranker>>)` becomes the single `EngineConfig -> pool ->
init_from_pool` funnel, shared by the builder's `File` state, the restore
   family, and the async wrapper.
4. `EngineConfig` keeps `#[non_exhaustive]` and **seals its fields**
   (`pub(crate)`); construct via `EngineConfig::new` + a `with_*` chain (#149).
   No separate `EngineConfigBuilder` — the `with_*` chain _is_ the builder, and
   `MemoryEngineBuilder<File>::into_config` produces a config for restore/async.
5. `AsyncMemoryEngine::open`/`open_memory` keep their public surface but reroute
   internals through the builder / `open_from_config`. A `build_async` terminal
   (and folding the restore family into the builder) are deferred follow-ups.

> **Amendment (#631):** `AsyncMemoryEngine` was deleted by the #631 cutover.
> `MemoryEngine` is now async-native: its **runtime, DB-touching** methods are
> `async fn` that `.await` an `Arc<dyn StorageBackend>` port, so there is no async
> wrapper to reroute. Construction stays **synchronous** — the typestate builder's
> `build()` and the `restore_*` family assemble the backend without awaiting. The
> typestate-builder decision (points 1–4) stands unchanged; only this point's
> `AsyncMemoryEngine` surface is superseded.

Behavior preservation is proven, not asserted: a golden `insta` equivalence
harness froze the observable construction state of all five constructors, then
re-pointed at the builder — the snapshots match byte-for-byte.

## Consequences

- **Capability growth is O(1):** a new optional capability is one setter + one
  field (on `impl<B: Backing>` for both backings, or `impl ...<File>` for
  file-only), not a new constructor per combination — the explosion #541 kills.
- **Breaking change:** `MemoryEngine::open*` are gone; downstream uses
  `MemoryEngine::builder(d)`. `EngineConfig` fields are private; use `new` +
  `with_*`. See `docs/advanced/migration-builder.md`.
- The illegal in-memory + `read_only`/`backup_dir` combination moves from a
  runtime `Err` (the #543 facade) to a compile error.
- The reranker never touches `EngineConfig` (which stays `Clone`); it rides the
  builder straight into `init_from_pool`.
