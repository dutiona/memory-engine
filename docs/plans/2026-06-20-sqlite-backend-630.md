# Implementation Plan — #630 `SqliteBackend` behind the storage traits

> `refactor(storage): implement SqliteBackend behind traits (zero behavior change)`
> Epic **#628** · depends on **#629** (A1, MERGED `24f9d65`) · feeds **#631** (engine rewire), **#632** (conformance suite), **#634** (PgBackend).
> Spec: `docs/superpowers/specs/2026-06-19-pluggable-storage-backend-design.md` §4 (module layout) + §7 (backend impls).

Synthesized from three max-effort lens drafts (`docs/plans/2026-06-20-sqlite-backend-630-drafts/`: architecture / risk / mvp) and reviewed by a three-lens internal subagent panel (`*-review-async.md`, `*-review-refactor.md`, `*-review-arch.md`; consolidated in `*-subagent-review.md`). All BLOCKER/HIGH findings are folded in below; D3 was decided by the user (defer).

---

## 1. BLUF — what #630 is, and the four decisions

**What:** Add `SqliteBackend`, a concrete implementation of the six #629 bounded-context traits (`FactGraph`, `EventLog`, `SearchIndex`, `ConsolidationStore`, `SessionStore`, `SchemaManager`) plus `ColdStorage` (feature `archive`). It **delegates** to the existing `src/store/*` structs and `src/search/*` free functions — reusing their SQL **verbatim** — wrapping each sync `rusqlite` call in `spawn_blocking`. The crate's `MemoryEngine` is **not** touched (that is #631); `SqliteBackend` is added purely additively and proven by new parity tests.

**Why delegation, not absorption:** the concrete stores are `&Connection`-scoped and are the SQL's single source of truth, shared by today's fast in-process unit tests. `SqliteBackend` is an _adapter_ that re-homes the borrow→own and sync→async concerns. #634's `PgBackend` reuses zero SQLite bodies, so absorbing the SQL would only have to be undone. (Retiring `src/store/` once Postgres parity exists is a separate, deliberate future call — explicitly out of scope.)

**Decisions:**

| #   | Fork                      | Decision                                                                                                                                                                                                                                                                                                                                                                        | Anchored by                                                         |
| --- | ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| D1  | async gating              | **Gate the whole `storage::sqlite` subtree behind `#[cfg(feature = "async")]`.** `spawn_blocking` needs tokio; default builds stay runtime-free.                                                                                                                                                                                                                                | `backend.rs:84-94` names #630's impl as `async`-gated.              |
| D2  | `for_each_*` streaming    | **`tokio::sync::mpsc::channel(1)` bridge**: the `spawn_blocking` scan `blocking_send`s rows; the async side `recv().await`s and invokes the non-`'static` callback. O(1) peak memory, mid-stream early-exit error propagation, never parks an executor thread. (Reject `std::sync::mpsc` — its blocking `recv` parks the executor; reject collect-to-Vec; reject inline-block.) | streaming contract `graph.rs:82-85`; async-lens BLOCKER             |
| D3  | ANN/HNSW                  | **Defer to #631.** `SqliteBackend::vector_search` is the verbatim **brute-force** free function. HNSW + its engine-owned build/dispatch policy (`search_config`, `ann_threshold`) move into the backend during the #631 rewire, as one coherent step. The backend is unused until #631, so there is no interim regression; tracked as a #631 task, not a silent gap.            | user decision; B1 (policy lives in `engine/mod.rs:264-272,370-378`) |
| D4  | error mapping at the seam | **Map `MemoryError::Database(rusqlite::Error)` → `Storage(StorageError::Backend(e.to_string()))` at the seam; pass semantic variants through unchanged.** The concrete stores keep emitting `Database` internally; the trait boundary must not leak a driver type.                                                                                                              | `error.rs:345-368` (driver errors → opaque `String` at the seam)    |

**Scope OUT:** rewire the engine (#631); cross-backend conformance battery (#632); `PgBackend` / new `backend-*` features (#633/#634); HNSW-in-backend (deferred to #631 per D3). The engine path stays byte-identical (`git diff --stat src/engine/` empty for production code).

---

## 2. Module layout (additive — no engine edits)

```
src/storage/
  mod.rs              (exists) — ADD `#[cfg(feature = "async")] pub mod sqlite;`
  backend.rs filter.rs capabilities.rs cold_storage.rs
  graph.rs event_log.rs search_index.rs consolidation.rs session.rs schema.rs   (exist — UNCHANGED contracts)
  sqlite/             NEW — one file per bounded trait, mirroring the contract 1:1
    mod.rs            SqliteBackend struct + ctors + delegation core
                      (block_read / block_write / for_each_streamed / map_join / map_seam_err)
    convert.rs        FactFilter → (Option<FactType>, Option<Vec<i64>>) projection + fail-loud assertions
    graph.rs          impl FactGraph         (delegates store/facts, edges, scopes)
    event_log.rs      impl EventLog          (delegates store/events + UpcasterRegistry)
    search_index.rs   impl SearchIndex       (delegates search/fts, vector — brute force)
    consolidation.rs  impl ConsolidationStore (delegates store/summaries, lineage)
    session.rs        impl SessionStore      (delegates store/activities, checkpoints)
    schema.rs         impl SchemaManager     (delegates store/schema + embedding_meta)
    cold_storage.rs   #[cfg(all(feature="async", feature="archive"))] impl ColdStorage (delegates store/archive_manifest)
```

One file per trait so a reviewer diffs `storage/sqlite/graph.rs` against `storage/graph.rs` side by side, and #634 mirrors it as `storage/postgres/graph.rs`. `mod.rs` is the only file that knows the struct internals.

### The ownership struct (`storage/sqlite/mod.rs`)

`SqliteBackend` absorbs the SQLite-private fields the engine holds as siblings today, so #631's construction site is simple. It stores the pool as `Arc<ConnectionPool>` (the `'static` `spawn_blocking` closures need an owned handle) even though the engine holds it by value today (`MemoryEngine { pool: ConnectionPool }`, `engine/mod.rs:157`). `UpcasterRegistry` is `Arc`-wrapped (cloned into every `EventLog` closure).

```rust
pub struct SqliteBackend {
    pool: Arc<ConnectionPool>,
    embed_dim: usize,
    upcaster_registry: Arc<UpcasterRegistry>,
}
```

> **D3 note:** no `hnsw` / `search_config` fields. `vector_search` is brute-force; HNSW ownership + policy is a #631 move (the engine still owns its `hnsw_strategy` and dispatch threshold until then). This keeps #630's struct free of `ann`-gated state and avoids duplicating engine query-policy.

`Send + Sync` is required (the `async_trait` futures are `Send`, `&self` is captured). T1 adds `static_assertions::assert_impl_all!(SqliteBackend: Send, Sync)` and `assert_impl_all!(UpcasterRegistry: Send, Sync)` so a non-`Sync` field is a loud, early failure.

Constructors: `from_pool(Arc<ConnectionPool>, embed_dim, Arc<UpcasterRegistry>)` (the path #631 uses) + thin `open`/`open_memory`/`open_read_only` wrappers the parity tests use.

---

## 3. The delegation seam (the DRY core, `storage/sqlite/mod.rs`)

Five private items carry the entire borrow→own + sync→async + error-mapping transition. Trait methods are ~3 lines each.

```rust
#[cfg(feature = "async")]
fn map_join(e: tokio::task::JoinError) -> MemoryError {              // panic/cancel → Pool
    MemoryError::Pool(format!("task join error: {e}"))              // matches async_engine::join_err
}

/// D4: confine `rusqlite` below the seam. Raw driver failures → opaque Storage(Backend);
/// semantic variants (NotFound, Migration, EmbeddingDimension, Conflict, ReadOnly, Internal, …)
/// pass through — they have a precise home (verified: no method emits raw `Database` for a
/// semantic condition — get→NotFound, marker→Conflict, fingerprint-absent→Internal).
fn map_seam_err<T>(r: Result<T>) -> Result<T> {
    match r {
        Err(MemoryError::Database(e)) =>
            Err(MemoryError::Storage(StorageError::Backend(e.to_string()))),
        other => other,
    }
}

impl SqliteBackend {
    #[cfg(feature = "async")]
    async fn block_read<T, F>(&self, f: F) -> Result<T>
    where T: Send + 'static, F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static {
        let pool = Arc::clone(&self.pool);
        let out = tokio::task::spawn_blocking(move || {
            let conn = pool.read()?;        // ReadConn derefs to &Connection; !Send → acquired inside
            f(&conn)
        }).await.map_err(map_join)?;
        map_seam_err(out)
    }

    #[cfg(feature = "async")]
    async fn block_write<T, F>(&self, f: F) -> Result<T>
    where T: Send + 'static, F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static {
        let pool = Arc::clone(&self.pool);
        let out = tokio::task::spawn_blocking(move || {
            let conn = pool.try_write()?;   // MutexGuard; errs ReadOnly on a read-only pool (preserves DD#6)
            f(&conn)
        }).await.map_err(map_join)?;
        map_seam_err(out)
    }
}
```

Load-bearing design points:

- **Guard acquired inside the closure** — `ReadConn`/`MutexGuard` are `!Send`; acquiring outside would not compile and would serialize the executor.
- **`try_write`, not `write`** — preserves Key Design Decision #6 (read-only → `MemoryError::ReadOnly`) for free, on every write method (H7).
- **`map_seam_err` at one point** — D4 enforced once for every delegated call; impossible to forget per-method.
- **`Send + 'static`** is always satisfiable (every trait arg is owned/`Copy` or cloned-to-owned); the `for_each_*` callback is the sole exception (D2).

### D2 — `for_each_streamed` (the one intricate method; async-lens BLOCKER fix folded in)

```rust
#[cfg(feature = "async")]
async fn for_each_streamed<T, S>(&self, scan: S, cb: &mut (dyn FnMut(T) -> Result<()> + Send)) -> Result<()>
where T: Send + 'static,
      S: FnOnce(&rusqlite::Connection, &tokio::sync::mpsc::Sender<T>) -> Result<()> + Send + 'static {
    let pool = Arc::clone(&self.pool);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<T>(1);   // cap-1 backpressure ⇒ O(1) peak
    let handle = tokio::task::spawn_blocking(move || { let conn = pool.read()?; scan(&conn, &tx) });
    // Drain on the async side; invoke the borrowing callback here. Capture an early
    // callback error and PREFER it over the scan's resulting send failure.
    let mut cb_err: Option<MemoryError> = None;
    while let Some(row) = rx.recv().await {
        if let Err(e) = cb(row) { cb_err = Some(e); break; }
    }
    drop(rx);                                               // unblocks/aborts the scan's next blocking_send
    let scan_res = map_seam_err(handle.await.map_err(map_join)?);
    match cb_err { Some(e) => Err(e), None => scan_res }    // callback error wins; else surface scan SQL error
}
```

Scan closures use `tx.blocking_send(row)` (legal on the blocking thread). Requires tokio feature `"sync"` (added to the `async` feature in `Cargo.toml`). Caveat documented in code: in-memory pools collapse reads onto the write mutex (`connection_pool.rs:225-230`), so for an in-memory store the scan briefly holds the write lock — acceptable (dump/export path), noted for completeness.

### convert.rs — the `FactFilter` projection (fail loud)

`SearchIndex` takes a rich `FactFilter`; the verbatim `fts_search`/`vector_search` honor only `(fact_type, scope_ids)` and hard-code `t_expired IS NULL` (Active). `convert::search_params(filter)` projects onto what the SQL accepts and **errors loud** (`MemoryError::Internal`) if `temporal != Active`, or `ids`/`pinned`/`metadata` are set — rather than silently dropping a predicate. The engine query path never sets those on a search filter; honoring them would be _new behavior_ #630 must not introduce. The `scope_ids: Some(empty) = matches-nothing` quirk is preserved by passing the slice straight to the unchanged `serialize_scope_ids`. (Collateral: full FactFilter→SQLite-search translation tracked separately.)

---

## 4. Hazard register → the differential proof

"Zero behavior change," but the engine is not rewired ⇒ **no existing test exercises `SqliteBackend`**; the danger is a _green_ suite that proves nothing. Each impl ships its parity assertions in the **same commit**. Note (review F3): because the backend delegates to the same SQL the oracle calls, parity is identity-by-construction _below_ the wrapper — these tests catch **wrapper drift** (wrong conn, lost error, bad projection, broken streaming), not SQL correctness (that is the stores' own unit tests + #632). Ranked, retire top-down:

| Id  | Hazard                                                                                                                                               | Catch (the assertion that fails on drift)                                                                                                                                                                                                                                                                                               |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| H1  | Search-seam drift: `FactFilter`→SQL infidelity, empty-scope inversion, `f32→f64` order, malformed-query swallow, `count_expired` not taking a filter | Per-dimension parity vs `fts_search`/`vector_search`/`fts_count_expired` (value + order; **distinct scores** to avoid `select_nth_unstable_by` tie flake, or set-compare ties); `Some(&[])`⇒empty vs `None`⇒found; `"\"unbalanced`⇒`Ok(empty)`; wrong-dim⇒`EmbeddingDimension`                                                          |
| H2  | Non-uniform `scope_ids` empty-slice on 11 `FactGraph` methods (empty=ALL×7, =NONE×3, `Option`×1)                                                     | Parametric table: `backend.m(&[]) == FactStore::m(&[])` for all 11 on a 2-scope fixture                                                                                                                                                                                                                                                 |
| H3  | `marker_key` injection guard (security)                                                                                                              | Reject-set `["", "in'sight", "$.x", "a b", "a;b"]` ⇒ `Conflict(QueryValidation)`; happy-path parity                                                                                                                                                                                                                                     |
| H4  | Error-variant fidelity (D4)                                                                                                                          | Exact-variant matrix: **propagating** write fail (e.g. `insert_event` constraint violation, NOT an FTS swallow) ⇒ `Storage(Backend)`; `get_fact(missing)`⇒`NotFound`; `require_*_present` fresh ⇒ `Internal`; `record_*_if_absent` dim-mismatch ⇒ `EmbeddingDimension`; read-only write ⇒ `ReadOnly`; future-epoch ⇒ `UnsupportedEpoch` |
| H5  | Transaction-boundary collapse (latent for #631)                                                                                                      | #630 leaves engine untouched ⇒ `git diff --stat src/engine/` empty for prod code; doc note that single-op trait methods are independent writes; #631 hazard filed as collateral                                                                                                                                                         |
| H6  | `spawn_blocking` `'static`: streaming + arg cloning                                                                                                  | Streaming test asserts BOTH precedences: callback `Err` at row _k_ ⇒ that exact `Err` + exactly _k_ rows seen; and a mid-scan SQL error (no callback error) ⇒ `Storage(Backend)`. Clone-to-owned is value-identical (no interior mutability)                                                                                            |
| H7  | `read_only` enforcement preserved                                                                                                                    | Read-only backend: each representative mutator ⇒ `ReadOnly`; reads still succeed                                                                                                                                                                                                                                                        |
| H8  | Read-vs-write conn selection per method                                                                                                              | **File-backed** concurrent-read routing test (in-memory collapses both conns and masks a mis-route)                                                                                                                                                                                                                                     |
| H10 | Schema/migration + `VACUUM INTO` backup + FK-rebuild + epoch                                                                                         | `schema_version`⇒12; migrate idempotent-at-HEAD; old-version migrates with backup; future-epoch⇒`UnsupportedEpoch`; corrupt config ⇒ `Migration`; `string→u32` parse                                                                                                                                                                    |

(H9 HNSW-freshness removed from #630 scope per D3 — folded into the #631 collateral note.)

---

## 5. Sequenced tasks (retire top risks first; each commit carries its parity tests)

- [ ] **T0 — Baseline.** Worktree `.worktrees/refactor-630-sqlite-backend` (off `main`, done). Gate green before touching code: `cargo build/test/clippy --workspace --all-targets` + `cargo test --all-features`.
- [ ] **T1 — Seam core + the hardest method first.** `Cargo.toml`: add `"sync"` to the `async` feature's tokio list. `storage/sqlite/mod.rs` with the struct, ctors, `block_read`/`block_write`/`map_join`/`map_seam_err`/`for_each_streamed`, the `Send+Sync` static asserts, all `#[cfg(feature="async")]` (D1, D2, D4). Implement **only** `EventLog::{insert_event, get_event, for_each_event}` to exercise the D2 bridge before replicating it. **Prove:** helper unit tests (`block_write`→`ReadOnly`, panic→`Pool`, `for_each_streamed` callback-error precedence + mid-scan-error precedence + no-hang + order); `get_event(missing)`⇒`NotFound`; `insert_event` constraint violation ⇒ `Storage(Backend)` (D4 witness via a propagating method).
- [ ] **T2 — `SearchIndex` + `convert.rs` (retire H1).** `lexical_search`/`vector_search` (brute-force, D3)/`lexical_count_expired`; `convert::search_params` projection + fail-loud. **Prove:** the full H1 matrix vs the free-fn oracle (distinct scores for order; set-compare ties); `Some(&[])`⇒empty; malformed⇒empty; wrong-dim⇒`EmbeddingDimension`; `convert` rejects non-`Active`/ids/pinned/metadata ⇒ `Internal`.
- [ ] **T3 — `FactGraph` facts (retire H2, H3, H4, H7, H8).** All fact read/write methods delegating to `FactStore`, with `// READ`/`// WRITE` conn-selection markers. **Prove:** the 11-method empty-`scope_ids` parity table (H2); `marker_key` reject-set + happy path (H3); the error-variant matrix (H4); read-only mutators ⇒ `ReadOnly` (H7); a **file-backed** concurrent-read routing test (H8).
- [ ] **T4 — `FactGraph` edges + scopes.** Delegate to `EdgeStore`/`ScopeStore`; `for_each_edge`/`for_each_scope` via the bridge. **Prove:** `edge_exists_active`, `list_active_edge_pairs_by_facts` (HashSet), `ensure_scope_path`, edge/scope streaming early-exit.
- [ ] **T5 — `ConsolidationStore` + `SessionStore`.** Delegate to `SummaryStore`/`LineageStore` and `ActivityStore`/`CheckpointStore` (the `#[cfg(test)]` reads become unconditional). **Prove:** summary upsert/list-by-level; lineage insert/get + `Lineage` error on missing; activity dedup-window; `list_recent_activities_by_scope` empty-slice⇒empty; checkpoint upsert/get-by-scope.
- [ ] **T6 — `SchemaManager` (retire H4-schema, H10).** Delegate `migrate`/`schema_version` (String→u32 parse, parse-fail ⇒ `Migration`)/`validate_schema_version` to `store::schema`; the four fingerprint methods to `store::embedding_meta`; sync `capabilities()` ⇒ `{ lexical_ranker: Bm25, true_idf: true, server_side_vector: false }`. **Prove:** the H10 matrix; `require_*_present` fresh ⇒ `Internal`; `record_*_if_absent` dim-mismatch ⇒ `EmbeddingDimension`.
- [ ] **T7 — `ColdStorage`** under `#[cfg(all(async, archive))]`. Delegate manifest CRUD to `ArchiveManifestStore`; pak I/O stays free functions. **Prove:** manifest insert/list(oldest-first)/delete parity; `cargo test --features async,archive` green.
- [ ] **T8 — `Arc<dyn StorageBackend>` realization proof (capped).** Construct a real `Arc<dyn StorageBackend>` from `SqliteBackend` and make **one** dispatch call per bounded trait through it — the value-level upgrade of #629's vtable-only test (`backend.rs:86-90` defers this to #630). One test driving `&dyn FactGraph = &backend`. (Semantic parity is #632, not here — review F5.)
- [ ] **T9 — Workspace gate + lint** (see §7). Confirm `git diff --stat src/engine/` shows no production change (H5).
- [ ] **T10 — Docs** (see §8).
- [ ] **T11 — Collateral issues** filed + linked to #628 (see §9).
- [ ] **T12 — Plan reference** posted as a comment on #630 (link the in-repo plan doc; one logical issue, no separate plan issue).
- [ ] **T13 — PR** `Fixes #630`, labels `type:refactor` + `area:storage`, linked under epic #628 (`addSubIssue`, `replaceParent: true`).
- [ ] **T14 — `/super-review`** (or 2–3 adversarial subagent reviewers per the under-budget preference). Reviewer focus: (1) the pinned seam-leak grep; (2) SQL truly verbatim (diff each delegated call vs its sync caller); (3) `for_each_streamed` hang/leak/precedence; (4) `convert.rs` fail-loud vs silent-drop; (5) D4 remap vs `error.rs:345-368`. Triage gemini-code-assist[bot] typed FPs. Re-gate after any rebase.
- [ ] **T15 — Squash-merge** once green + approved.

---

## 6. Verification

```bash
cargo build  --workspace --all-targets          # default build: storage::sqlite fully cfg'd out, zero tokio
cargo test   --workspace --all-targets
cargo clippy --workspace --all-targets --all-features   # pedantic + nursery clean
cargo test   --all-features                      # async + ann + archive: exercises SqliteBackend + parity
cargo test   --features async                    # backend without ann/archive
cargo test   --features async,archive
cargo fmt --check                                # real cargo fmt, NOT rtk (rtk fmt summaries have masked exit codes)
git diff --stat src/engine/                      # MUST be empty for production code (H5)

# Seam-leak gate (review F2) — pinned, with the one legitimate exception (private helper bounds):
! grep -rnE 'pub( |\().*\b(rusqlite|Connection)\b' src/storage/sqlite/ \
  | grep -vE 'block_read|block_write|for_each_streamed'   # expect: no matches
```

Pass criteria: all green; no new clippy `allow`s beyond those the trait files already carry; the seam-leak grep yields nothing; `unsafe_code = "forbid"` holds. The `{async} × {archive}` matrix is non-negotiable — `ColdStorage` only compiles under `archive`. Drift-catching lives in the **parity assertions** (wrapper drift), not the compile.

## 7. Documentation

- [ ] **Module rustdoc** on `storage/sqlite/mod.rs`: ownership story; delegation-over-`block_read`/`block_write`; SQL lives in `src/store/*`+`src/search/*` (this adapts, does not own SQL); conn-selection rule (reads→`pool.read`, writes→`try_write`); D2 streaming semantics + the in-memory caveat; D4 error-mapping rule; the H5 caveat (single-op methods are independent writes; engine composes atomicity above the seam); D3 (brute-force vector_search; HNSW is #631). State D1–D4 inline for #634's author.
- [ ] **`docs/reference/crate-layout.md`** — add the `src/storage/sqlite/*` subtree (one-file-per-trait; delegation-not-absorption note).
- [ ] **`CLAUDE.md` Status** — mark #630 ✅ (A2) on merge; note #631 unblocked.
- [ ] **ADR:** `N/A` — the seam ADR is a separate spec deliverable; #630 implements an already-decided seam.
- [ ] **Narrative (Sphinx) docs:** `N/A` — #630 introduces no user-facing API (engine untouched until #631).

## 8. Testing

- **Positive differential proof** (`#[cfg(feature="async")] #[tokio::test]`): per trait-method group, drive `SqliteBackend` and assert results **identical** to the concrete-store/free-fn oracle on a shared fixture. These catch **wrapper** drift (the SQL is shared with the oracle; SQL correctness is the stores' own tests + #632). Property/proptest where one row is insufficient (FactFilter translation; the 11-method empty-scope table; streaming precedence).
- **Error-variant fidelity** (H4): assert the _exact_ `MemoryError` variant per failure mode, not `is_err()`; the raw-driver witness uses a **propagating** method (FTS swallows errors to `Ok(empty)` and cannot witness it).
- **File-backed routing** (H8) + **read-only enforcement** (H7): require a file-backed pool variant (in-memory collapses read/write conns).
- **Feature-matrix builds**: `async`, `async,archive`, default (unchanged), `--all-features`.
- **Existing suite stays green, untouched** — the addition is purely additive; the regression gate is "no existing test changes + all pass."
- **Levels excluded:** e2e `N/A` (no process/IO boundary introduced; MCP/CLI binaries unaffected until #631). Benchmarks `N/A` for #630 (the efficiency×quality benchmark is the epic's piece D / memarch-bench).

## 9. Collateral / follow-ups (separate issues, `type:*`+`area:*`, linked to #628)

1. **`SchemaManager` trait-doc error-menu drift** (`type:docs`/`area:storage`): the trait rustdoc (`storage/schema.rs:22-27`) lists `EmbeddingDimension` but omits the `Internal` that `require_embedding_fingerprint_present` actually returns — reconcile the trait error menu.
2. **Full `FactFilter`→SQLite-search translation** (`type:enhancement`/`area:retrieval`): `convert::search_params` fails loud on `temporal`/`ids`/`pinned`/`metadata`; whoever needs those on the SQLite search path implements + tests the richer SQL.
3. **HNSW-in-backend + transaction atomicity (#631 notes, not new issues):** record on #631 that (a) per D3 it must move the engine's `hnsw_strategy` + `search_config` + `ann_threshold` dispatch policy into the backend when it flips `vector_search` over (else a real O(N) regression lands); (b) `add_fact`/`consolidate`'s multi-store `unchecked_transaction` cannot be replaced by two backend calls without a seam-level transaction primitive (or keeping it engine-side) — else the #614 orphan-vector window reopens.
4. **`scope_ids` unification (#652-style, already tracked):** #630 mirrors the non-uniform empty contract verbatim; the H2 parity table is the regression net any future `ScopeSelector` unification must keep green.
