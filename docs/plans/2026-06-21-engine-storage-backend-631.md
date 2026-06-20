# Implementation Plan — #631 wire engine to `Arc<dyn StorageBackend>` (async-native)

> `refactor(core): wire engine to Arc<dyn StorageBackend>` · epic **#628** · depends on **#630** ✅ · feeds **#632** (conformance), **#633/#634** (PgBackend).
> Design locked 2026-06-21: `docs/plans/2026-06-21-engine-storage-backend-design-synthesis.md` (Fork A=A1 async-native, B=coarse atomic methods, C=HNSW→backend). Synthesized from 3 lens drafts; revised after a 3-lens internal review (`*-631-review-{staging,correctness,async}.md`).

## 1. BLUF — what, and the load-bearing constraint

Replace the engine's direct `rusqlite`/`ConnectionPool` access with `Arc<dyn StorageBackend>`; make `MemoryEngine` **async-native**; delete `AsyncMemoryEngine`; move HNSW + transaction atomicity into the backend. RRF stays engine-side.

**The cutover is one irreducible big-bang commit** (verified by all 3 lenses — dual-field coexistence, per-method `block_on` bridge, and a sync-facade were each shown non-viable). The engine struct's single `pool` field is reached at **~67 direct `with_read`/`write_conn`/`self.pool` sites** (expanding to ~150 `.await` points) across ~20 `src/engine/*` modules; swapping it to `storage: Arc<dyn StorageBackend>` deletes those helpers and forces the **DB-touching subset of the engine's 85 `pub fn` to `async fn` at once** (no half-async engine compiles). Because that breaks cli/mcp callers immediately, `cargo build --workspace` drags **engine-flip + cli-runtime + mcp-simplify + AsyncMemoryEngine-deletion into the same PR.**

**Strategy: front-load every _design_ into additive, independently-green prep PRs (Stages A–D), so the cutover (Stage E) is pure compiler-driven translation** (`fn`→`async fn`, `.await`, no new logic) — "green ⇒ correct, every regression bisectable."

## 2. End-state

```rust
pub struct MemoryEngine {
    storage: Arc<dyn StorageBackend>,           // replaces `pool: ConnectionPool`
    #[cfg(feature = "archive")]
    cold: Option<Arc<dyn ColdStorage>>,
    graph: RwLock<KnowledgeGraph>,              // in-memory projection (unchanged)
    scope_tree: RwLock<ScopeTree>,              // in-memory projection (unchanged)
    reranker: Option<Arc<dyn Reranker>>,
    upcaster_registry: Arc<UpcasterRegistry>,
    // DROPPED: pool, vector_strategy, hnsw_strategy, search_config  (→ into SqliteBackend)
}
pub async fn close(&mut self) -> Result<()>     // port-touching flush (HNSW snapshot + fingerprint)
```

- Backend selection: a defaulted `BackendKind` enum on `EngineConfig`, resolved in one `open_storage()` seam (#634 adds a `Postgres` arm).
- `async` becomes **default-on** (`default = ["async"]`); tokio is non-optional in practice (a non-async build has no backend to hold). Full removal of the now-vacuous `#[cfg(feature="async")]` gates is **Stage F** (deferred cleanup, out of #631's correctness scope).
- `AsyncMemoryEngine` deleted — audit (grep-confirmed): only `lib.rs:82` (`pub mod async_engine`) + 2 doc files reference it; **zero** cli/mcp/embed/test consumers.

## 3. Fork B — coarse atomic port methods (the only object-safe choice)

`begin`/`commit` leaks the backend tx type; a closure unit-of-work needs nightly HRTB / breaks object-safety. So: **dedicated coarse atomic methods**, each implemented in `SqliteBackend` as `block_write(|conn| { let tx = conn.unchecked_transaction()?; <DB body>; tx.commit() })`. HNSW `notify_insert`/`notify_expire` (Fork C) fire post-commit inside these, backend-private.

**⚠️ NOT a pure verbatim move (review BLOCKER):** the ingest transactions touch **in-memory `scope_tree`** mid-body (`ensure_scope_with_conn`, `mod.rs:621`; `add_facts_batch`, `ingest.rs:458-465`), which has no `self` below the seam. The seam therefore **splits**: the DB work (savepoint + scope-row creation + fact/vector/FTS insert) goes below and **returns the scope-ids-to-cache**; the engine applies `scope_tree.write()` **above** the seam from the returned ids. Each method's return type must carry that upward.

| New port method                 | Trait                | Replaces                                                           | Returns (for engine-side post-apply)                    |
| ------------------------------- | -------------------- | ------------------------------------------------------------------ | ------------------------------------------------------- |
| `insert_fact_atomic`            | `FactGraph`          | `ingest.rs:228` (identity-stamp + fact + vector + FTS; #614 guard) | `(fact_id, scope_ids_to_cache)`                         |
| `insert_facts_batch_atomic`     | `FactGraph`          | `ingest.rs:458` batch                                              | `(ids, scope_ids_to_cache)`                             |
| `insert_cosession_edges_atomic` | `FactGraph`          | `graph.rs:71`                                                      | edge ids                                                |
| `apply_cycle_deltas_atomic`     | `ConsolidationStore` | `cycle/apply.rs:80`                                                | supersede-edge triples (for the in-memory graph mirror) |
| `commit_archive_atomic`         | `ColdStorage`        | `archive.rs:255`                                                   | () — genuine verbatim move (no in-memory read mid-tx)   |

- `apply_cycle_deltas_atomic` is **full push-down** incl. `validate_report` (which today runs on the held write conn, `apply.rs:364`, to avoid the self-deadlock — see §6). The engine keeps only the read-only `CycleError` business validation that needs no connection.
- Two **config** methods on `SchemaManager`: `get_config`/`set_config` (cutover needs them at `mod.rs:649,659` + cycle watermarks; currently backend-private).
- Object-safety: automatic under `#[async_trait]` (same pattern as the existing ~90 trait methods, proven by the `backend.rs` callability test).

## 4. Fork C — HNSW into `SqliteBackend`

`SqliteBackend` gains `search_config` + `ann_threshold` + the owned `HnswStrategy` (built `from_db` at construction under `#[cfg(feature="ann")]`). `vector_search` dispatches HNSW-vs-brute internally via the same `active_count() >= ann_threshold` predicate. **Edge case (review):** constructed without a `search_config`, `ann_threshold` defaults to `usize::MAX` ⇒ HNSW never activates (`mod.rs:383-386`) — the backend must replicate this exactly; Stage B tests the boundary. The engine loses `hnsw_strategy`/`should_use_hnsw`/`active_strategy_name`/`search_config`. HNSW snapshot serialization relocates to a backend method (interlocks with §6 Drop/close).

## 5. `hybrid_search` decomposition (the deepest coupling — Stage C)

`query.rs` passes `(&Connection, &dyn VectorSearchStrategy)` into sync `hybrid_search`. Split: **I/O channels below the seam** (`storage.lexical_search().await` + `storage.vector_search().await` → ranked `(id, f64)`); **fusion above the seam** (`rrf_merge` + temporal post-filter (`hybrid.rs:242-251`, operates on `valid_at` context not raw DB) + `match_type` — unchanged). **f32→f64 widening happens at the backend boundary** (`SearchIndex` returns `f64`); the Stage C parity oracle asserts `score == f64::from(orig_f32)` per result, **not just rank order** (review). Land alongside the old `hybrid_search` with a bit-identical parity oracle before the cutover consumes it.

## 6. Critical findings (must-handle; from the review)

1. **`DreamCycle::run` is sync-by-contract (`traits.rs:344`) but reaches the now-async backend** via `DreamContext::with_read` (`cognitive.rs:73-148`). Can't `.await` in a sync trait method; `block_on` deadlocks. **Fix: make `DreamCycle::run` async (`#[async_trait]`)** — all 4 impls are in-crate (no external break); pre-staged in **Stage A2**.
2. **`reqwest::blocking` panics under `#[tokio::main]`** ("cannot start a runtime within a runtime"). The embed crate's `HttpEmbeddingProvider`/`HttpSummaryGenerator`/`HttpDeltaProposer` use it; once the CLI (`consolidate`/`batch-ingest`) and MCP call them on a tokio thread, they panic. **Fix: switch those providers to the async `reqwest` client** — pre-staged in **Stage A2**.
3. **`MemoryEngine::drop` (`mod.rs:826`) calls sync `write_snapshot()` which reads the pool for the DB fingerprint (`mod.rs:538`).** An `async fn` can't run in `Drop`, and a backend read in `Drop` is a `block_in_place`/current-thread-panic hazard. **Fix: `Drop` writes nothing (logs a warning if `close()` was not called); `pub async fn close(&mut self)` on `MemoryEngine` owns the full snapshot incl. fingerprint.** Behavior change (dropped-without-close loses the sidecar → rebuilt next open) — **documented** in the migration note.
4. **`parking_lot::RwLock` guards held across `.await` ⇒ `!Send` futures.** The engine binds `graph`/`scope_tree` write guards to locals (`cycle/apply.rs:310`, `ingest.rs:459`, `forgetting.rs:27`, `archive.rs:94`, …); after async conversion any `.await` between acquire and drop makes the future `!Send` (compiler-caught, but systematic). **Stage E audit bullet: extract the needed value, drop the guard, then `.await`.**
5. **`archive.rs` has 3 un-tabled pool accesses:** `self.pool.try_write()` (read-only guard, `:48`), `self.pool.read()` (`select_archive_candidates`, `:168`), `self.pool.path()` (`archive_dir`, `:342`). `pool.path()` disappears post-cutover. **Fix: the candidate-select read becomes a port read method; `archive_dir` gets the path from `EngineConfig` (stored at open) or a backend path accessor.** Decided in Stage A.
6. **`validate_report` (`apply.rs:364`) moves below the seam too** (part of `apply_cycle_deltas_atomic`'s full push-down) — stated explicitly so it isn't an implementor gap.

## 7. Staging — sub-issues under #628 (each its own PR, green, reviewable)

**Additive prep (engine still on `self.pool`; each leaves `cargo test --workspace` green — Stage B/C helpers stay green only because their parity tests consume them):**

- [ ] **Stage A — atomic port methods + config + archive-read** _(sub-issue, `area:storage`)_. Add the 5 atomic methods (with the scope-ids-return split, §3) + `get_config`/`set_config` + the archive candidate-select read method + decide `archive_dir` path source; implement in `SqliteBackend`. Parity tests drive `SqliteBackend` directly vs current engine behavior: crash-injection rollback differentials + #614 orphan-vector guard.
- [ ] **Stage A2 — async-ready the consumer-trait surface** _(sub-issue, `area:core`)_. Make `DreamCycle::run` async (`#[async_trait]`, fix the 4 in-crate impls); switch the embed crate's `Http{EmbeddingProvider,SummaryGenerator,DeltaProposer}` from `reqwest::blocking` to async `reqwest`. Both additive (the sync engine still works; `AsyncMemoryEngine` already spawn_blocks). Tests: existing dream-cycle + embed tests stay green.
- [ ] **Stage B — HNSW into `SqliteBackend`** _(sub-issue, `area:storage`)_. Move `search_config`/`ann_threshold`/index + dispatch + snapshot. Parity: golden recall + top-k ordering swept across `active_count() == ann_threshold`; the `search_config==None`→never-HNSW boundary; snapshot round-trip.
- [ ] **Stage C — port-driven `hybrid_search`** _(sub-issue, `area:retrieval`)_. Add decomposed version alongside old; value+rank parity oracle (incl. f32→f64 at the boundary) + RRF rank-stability.
- [ ] **Stage D — `async` default-on** _(sub-issue, `area:build`)_. Flip `default=["async"]`; add `tokio` to `memory-engine-cli/Cargo.toml [dependencies]` (`rt-multi-thread`,`macros`). Confirm workspace builds/tests with async default.

**The cutover (this issue #631 — the irreducible big-bang PR):**

- [ ] **Stage E — THE CUTOVER.** Engine struct `pool`→`storage`; DB-touching `pub fn`→`async fn`; translate ~67 direct sites (~150 await points) → `self.storage.*().await` / the atomic methods (apply scope-cache above the seam); switch retrieval to port-driven `hybrid_search`; the `Drop`/`close()` split; the **`!Send`-guard audit** (finding 4); convert ~245 engine tests to `#[tokio::test]` + re-point the ~16-18 pool-reaching helpers via a `#[cfg(test)] fn storage(&self)` accessor; **CLI** tokio runtime (`#[tokio::main]`/`block_on` at command boundary) + `.await`; **MCP** drop `spawn_blocking` (`server.rs:102`), await directly (storage offload preserved by `SqliteBackend`'s internal `spawn_blocking`; the consumer-trait HTTP path is safe now that A2 made it async); **delete `AsyncMemoryEngine`** + fix `lib.rs:82`. One PR (workspace gate is atomic). Pure translation — every primitive/design pre-built in A–D + A2.

**Deferred follow-up:**

- [ ] **Stage F** _(sub-issue, post-#631)_ — remove the now-vacuous `#[cfg(feature="async")]` gates crate-wide (~40 sites). Mechanical; not required for #631 correctness.

**Ops:** worktree (done); file A/A2/B/C/D/F as sub-issues under #628 (`addSubIssue`, link this plan); per stage PR → **internal-subagent diverse-lens review** (under-budget preference) → rebase + re-gate → squash-merge (main moves fast — #614-class semantic breaks lurk).

## 8. Documentation

- [ ] Rustdoc: atomic methods (contract "Ok ⇒ all sub-ops committed; Err ⇒ store byte-identical"); the scope-ids-return split; `SqliteBackend` HNSW ownership; `async fn close()` vs `Drop`; `BackendKind`/`open_storage`.
- [ ] `docs/reference/crate-layout.md` + `docs/design/architecture-overview.md`: engine→port async threading model; remove `AsyncMemoryEngine`.
- [ ] **Migration note (REQUIRED — public API break):** engine API is now async; sample showing the CLI `#[tokio::main]`/`block_on` boundary + an MCP `await`; the dropped-without-`close()` sidecar-rebuild behavior change.
- [ ] `CLAUDE.md` Status: #631 ✅ on merge; #632 next.

## 9. Testing

- **Stages A/A2/B/C**: additive parity/differential tests proving each new primitive matches current engine behavior **while the engine is still sync** (oracle = the code being replaced): crash-injection rollback (atomicity), recall/ordering sweep + `None`-config boundary (HNSW), value+rank bit-identity (hybrid_search), dream-cycle + embed parity (A2).
- **Stage E**: the ~245 existing engine tests (converted to `#[tokio::test]`) ARE the behavior-preservation gate — intent unchanged. #632 conformance is the eventual exhaustive cross-backend proof.
- **e2e**: `cargo test -p memory-engine-cli -p memory-engine-mcp` after their Stage-E migration.
- Excluded: no new levels — behavior-preserving; benchmarks tracked separately (epic piece D).

## 10. Verification (per stage + cutover)

```bash
cargo build --workspace --all-targets
cargo test  --workspace --all-targets          # ~1400-test suite stays green
cargo test  --all-features
cargo clippy --workspace --all-targets --all-features
cargo fmt --check
cargo deny check
# Stage E only — cutover invariants:
! grep -rnE 'self\.pool|with_read|write_conn' src/engine/   # engine no longer touches the pool
! grep -rn  'AsyncMemoryEngine' src/ memory-engine-*/src/   # fully deleted
! grep -rn  'reqwest::blocking' memory-engine-embed/src/    # consumer-trait HTTP path is async
```

Pass criteria per stage: all green; no new clippy allows; `unsafe_code=forbid`; CI `-D warnings` clean. Cutover additionally: the greps return nothing, the converted suite passes, and `cargo test -p memory-engine-cli -p memory-engine-mcp` is green (no nested-runtime panic).
