# Clean-slate subagent review — Engine Builder Plan

**Reviewer:** general-purpose Agent, no prior context, full read access to the worktree.
**Verdict: LGTM with reservations.** Load-bearing claims verified against code.

## Verified-correct (plan foundations are real)

- `init_from_pool` is private (`mod.rs:235`) and sibling-callable from `src/engine/builder.rs` — D7 correct.
- `Box<dyn Reranker>` non-Clone, consumed into `init_from_pool` (`mod.rs:240`) — D1 genuine.
- Field-write census exhaustive: `config.{read_only,backup_dir,search_config} =` only in
  `benches/search_bench.rs:377`, `tests/read_only_test.rs` (5×), `src/engine/tests.rs:817,3015,3225`,
  `memory-engine-cli/src/db.rs:43,57`. **Zero `EngineConfig { .. }` struct literals** — R8/D4 confirmed.
- No cross-crate reads of sealable fields: mcp `.embed_dim` hits are on the MCP crate's OWN
  `EngineSection` type, not `memory_engine::EngineConfig`. Only cross-crate field-poke is cli writes.
  **Sealing is cross-crate safe** — D4 accurate.
- D6 File-`build()` routing preserves all behavior (read_only branch, `backup_dir.as_deref()`,
  `read_pool_size`, embed_dim validation) vs `open`/`open_with_reranker`. No behavior lost.
- `ConnectionPool::{is_read_only(:229), is_file_backed(:242)}` exist (`pub const`); `read_pool_size`
  is a private field needing a `pub(crate)` test getter (Task 1 plan correct).
- Crate-root re-exports exist (`lib.rs:52`). Task-5 mcp import switch valid.

## Findings

- **[MEDIUM] R6 task-ordering gap:** `async_engine.rs:64,82` have intra-doc links
  `[`MemoryEngine::open`]` / `[`MemoryEngine::open_memory`]`. Task 6 deletes those items but the link
  fix is in Task 7 → Task 6's own `cargo doc` gate fails on dangling links. **Fix:** move the
  async_engine doc fixes into Task 5/6.
- **[MEDIUM] Async rustdoc references survive deletion:** `async_engine.rs:64,82` "Returns errors from
  `[MemoryEngine::open]`". Task 5 reroutes async bodies but doesn't explicitly rewrite these doc lines.
  Fold async rustdoc rewrite into Task 5 (same commit as the bodies).
- **[LOW] Site-count ~1.6× high:** actual receiver-anchored count is **226 total / 96 in tests.rs**
  (not ~360 / ~108). Use "all migrated sites" language, not a number.
- **[LOW] `into_config()` drops reranker — pin the harness assertion:** Task-1 tuple includes
  `reranker_name`; ensure the File+reranker case asserts `reranker_name() == Some(..)` so the
  "reranker → `open_from_config` 2nd arg, not into_config" wiring is proven.
- **[LOW] `restore_sqlite` `inspect_err` cleanup:** `restore.rs:157`
  `Self::open(config).inspect_err(|_| remove_file)`. The orphan-file cleanup wrapper must be preserved
  on the `open_from_config(config, None)` rewrite. Flag in Task-5 checklist.

## Design-question answers

- Typestate `build()` in both states compiles (two separate `impl` blocks, not a trait method). Sound.
- Typestate not over-engineered — makes in-memory+read_only/backup_dir structurally unrepresentable,
  matching the pool's hardcoded `read_only:false,backup_dir:None` (`connection_pool.rs:131–138`).
  Split-state payload avoids `expect`/`unreachable` (matters under `unsafe_code = forbid`).
- Sealing EngineConfig fields worth it — churn contained (11 in-repo write sites, zero external
  literals); `#[non_exhaustive]` + `with_*` (no second builder) avoids competing-builders anti-pattern.
- `cfg(ann)`/`cfg(async)` compile — all ann-gating lives inside untouched `init_from_pool`; async keeps
  public signatures. R3 feature-matrix gate is the correct net.

## Structural completeness

Documentation §4 ✓, Testing §5 ✓, Verification §6 ✓ — all present and thorough.

## Resolution

- [MEDIUM] R6 doc-link task-ordering → **Addressed.** Task 5 now explicitly rewrites the
  `async_engine.rs` intra-doc links + "Returns errors from `[MemoryEngine::open]`" rustdoc lines in the
  same commit that reroutes the async bodies; Task 6's doc gate is therefore clean. Both MEDIUMs fixed
  by one edit (they are the same async_engine.rs lines).
- [MEDIUM] Async rustdoc references → **Addressed** (same Task-5 edit as above).
- [LOW] Site-count → **Addressed.** Plan language changed to "all migrated sites" with the verified
  226/96 figures noted as approximate.
- [LOW] File+reranker harness assertion → **Addressed.** Task 1 + Task 5 testing items now explicitly
  require asserting `reranker_name() == Some(..)` on the File+reranker equivalence case.
- [LOW] `restore_sqlite` `inspect_err` → **Addressed.** Task 5 checklist now calls out preserving the
  `inspect_err` orphan-cleanup wrapper on the `open_from_config` rewrite.
