# Correctness Review — #631 "wire engine to `Arc<dyn StorageBackend>`"

> Reviewer: adversarial correctness lens (behavior-preservation, async cutover).  
> Scope: verifying plan §3–6 claims against the live source tree.  
> Date: 2026-06-21.

---

## Verdict

The plan is **substantially correct** in its design choices and staging strategy.
Two findings require action before Stage A ships: one BLOCKER and one HIGH.
Three additional items are MED/LOW and must be documented before Stage E.
Everything else checks out.

---

## Finding 1 — [BLOCKER] `add_facts_batch` reads `scope_tree` INSIDE the write lock, after the savepoint releases

**File:** `src/engine/ingest.rs:458–465`

```rust
// after RELEASE batch_insert:
let scope_store = ScopeStore::new(&conn);
let mut tree = self.scope_tree.write();          // ← RwLock write acquired
for sid in scope_ids_to_cache {
    if let Ok(node) = scope_store.get(sid) {
        tree.insert(node);
    }
}
drop(tree);
```

This happens _after_ `RELEASE batch_insert` (the savepoint committed), but _while
`conn` (the write lock) is still held_. The scope-tree update is therefore bound
to the write-lock lifetime, not to the transaction itself.

The plan states that `insert_facts_batch_atomic` will move the "existing body
verbatim" into `block_write(|conn| { … })`. If that closure boundary ends at
tx.commit / RELEASE, the deferred scope-tree update (lines 458–465) is outside
the closure and therefore **above the seam**, running on the engine side. This is
actually the correct architecture — that's what the plan intends — but the plan
does NOT acknowledge this: it says "verbatim tx-body move" for this method,
implying the entire function body goes below the seam.

**The actual split required:**

- Below the seam (inside `block_write`): the savepoint + fact inserts + scope DB
  operations + return `(ids, scope_ids_to_cache)`.
- Above the seam (engine side): scope-tree RwLock write using the returned
  `scope_ids_to_cache`.

This is not a "verbatim body move" — it is a deliberate split with a return value
carrying `scope_ids_to_cache` upward. The plan must specify this explicitly, or
Stage A implementors will either (a) leave the scope-tree update hanging outside
the closure with unclear ownership, or (b) try to call
`self.scope_tree.write()` from inside the backend closure, which is impossible
(the closure runs below the seam, no `self` access).

The same issue exists for the single-fact path (`insert_fact_with_embedding`):
the `ensure_scope_with_conn` helper (mod.rs:621–639) also calls
`self.scope_tree.write()` _while holding `conn`_. That scope write must likewise
return above the seam as a return value or side-channel.

**Action:** Stage A must define return types for `insert_fact_atomic` and
`insert_facts_batch_atomic` that carry the scope IDs for engine-side cache
population. The plan's "verbatim body move" claim is inaccurate for both batch
and single-fact methods.

---

## Finding 2 — [HIGH] `archive` reads `self.pool` directly, not via `write_conn`/`with_read`

**File:** `src/engine/archive.rs`

- Line 48: `drop(self.pool.try_write()?)` — direct pool access for the read-only
  guard check.
- Line 168: `let conn = self.pool.read()?` — direct pool read in
  `select_archive_candidates`.
- Line 342: `self.pool.path()` — direct path access in `archive_dir`.

The plan assigns `commit_archive_atomic` to `ColdStorage` (§3, table row 5) and
says "only manifest-insert + hard-delete is atomic; pak file I/O stays
engine-side orchestration". However, it does not address that:

1. `archive_dir()` calls `self.pool.path()` — this method no longer exists on
   the engine after the cutover (pool is gone). `archive_dir()` must become a
   backend method (`storage.db_path()` or similar) or be built from `EngineConfig`
   at construction time.
2. The read-only guard check at line 48 (`self.pool.try_write()`) is a pool-level
   mechanism. After the cutover, the engine needs a backend capability check —
   either `storage.is_read_only()` or `storage.try_write_guard()`.
3. `select_archive_candidates` at line 168 calls `self.pool.read()` directly —
   this is a read that must become `storage.block_read(|conn| …)`.

None of these cross-cutting `archive.rs` pool accesses are called out in the plan.
They are not part of the 5 "coarse atomic methods" table but they ARE direct pool
accesses that must be ported. The verification grep (`! grep -rnE 'self\.pool'`)
will catch them, but they should be planned, not discovered at Stage E.

**Action:** Enumerate these 3 archive.rs pool accesses in the Stage A or Stage E
work item. Confirm `archive_dir()` resolution strategy (backend path accessor vs
config-stored path).

---

## Finding 3 — [HIGH] `write_snapshot` uses `self.pool.read()` and `self.pool.path()` — directly incompatible with Drop plan

**File:** `src/engine/mod.rs:530–568`

`write_snapshot` calls:

- `self.pool.path()` (line 531)
- `self.pool.is_read_only()` (line 534)
- `self.pool.read()` (line 538) — to read the DB fingerprint
- `h.to_snapshot(&conn, self.embed_dim)` for HNSW (line 545)

The plan (§6) says "the in-memory graph/scope-tree serialization stays sync (no
I/O to the port); the HNSW-index snapshot becomes backend-owned and is persisted
via a sync backend hook at drop". But `write_snapshot` as it stands also needs
**a DB fingerprint read** (`snapshot::read_fingerprint(&conn)`) to stamp the
sidecar header. That fingerprint read requires a pool/backend read connection —
it cannot be elided.

The plan's Drop split works correctly for graph/scope serialization (pure
in-memory, no port access). But the DB fingerprint stamping MUST also happen
for the snapshot to be valid (a stale fingerprint causes a cold-start rebuild on
next open). This means:

- Either `Drop` calls `write_snapshot` which calls `storage.block_read(fingerprint)`
  synchronously — but in an async context, calling `block_read` from `Drop` is a
  `block_in_place` hazard (blocking a tokio thread inside Drop).
- Or `async fn close()` handles the full snapshot (including fingerprint + HNSW)
  and `Drop` only writes graph/scope WITHOUT the fingerprint header — meaning the
  sidecar written at drop is either invalid (missing fingerprint) or intentionally
  stale (which triggers a rebuild anyway).

The plan acknowledges that `Drop`-time snapshot is "best-effort" but does not
resolve what happens to the fingerprint. If `close()` is not called, the snapshot
is either corrupt or missing — a regression vs. the current sync behavior where
`Drop` always writes a valid sidecar.

**Action:** Specify explicitly what the Drop-path snapshot contains and whether it
is self-consistent (valid fingerprint + no DB I/O). The safest resolution: Drop
does NOT write a snapshot at all (logs a warning if `close()` was never called),
and `close()` writes the full snapshot. This is a behavior change (dropped engines
lose their snapshot) but it is documented and deliberate. The alternative —
calling `block_in_place` from `Drop` — works in tokio multi-thread but panics in
current_thread scheduler and is explicitly discouraged.

---

## Finding 4 — [MEDIUM] `apply_cycle_report` validate pass runs on the same connection as the apply pass: this is correct and MUST be preserved

**File:** `src/engine/cycle/apply.rs:72–83`

```rust
// Single lock acquisition for the whole operation (validate + apply).
let conn = self.write_conn()?;
// --- Validation pass (read-only, on the held connection) ---
self.validate_report(&conn, report)?;
// --- Apply pass (one transaction; rollback on any error) ---
let tx = conn.unchecked_transaction().map_err(MemoryError::Database)?;
```

The plan (§6 hazard) correctly identifies that validate + apply must share one
connection to avoid self-deadlock on in-memory engines. The in-memory engine uses
`wal_autocheckpoint=0` + WAL mode, but more critically, `parking_lot::Mutex` is
non-reentrant: a second `write_conn()` call on the same thread would deadlock.

**Confirmed correct:** the plan's "validate-engine read-only / apply-atomic
push-down, return supersede-edge triples" approach is the right split. Validate
runs engine-side (reads via the already-held connection passed down, or via a
backend read that doesn't compete with the write lock), and the entire apply body
moves to `apply_cycle_deltas_atomic` as a single `block_write` call.

**One gap in the plan:** `validate_report` at line 364 takes `conn: &Connection`
directly. After the cutover, the engine no longer holds a raw connection — it
calls `storage.apply_cycle_deltas_atomic(...)`. The validate pass must either:
(a) be pushed entirely below the seam (backend validates then applies), or (b)
run against a read handle from the backend before the write lock is acquired.

Option (b) is UNSAFE on an in-memory engine: SQLite in-memory mode has a single
shared connection, so acquiring a read then trying a write in the same thread
deadlocks with `parking_lot::Mutex`. The plan correctly says "full push-down".
This is confirmed. But the plan should also say explicitly that `validate_report`
moves below the seam (into `apply_cycle_deltas_atomic` on the backend), because
the comment at apply.rs:364 ("runs on the already-held write connection") is the
exact invariant that must be preserved by moving everything into one backend call.

**Verdict for §6:** The plan's hazard analysis is correct. No new risk found here
beyond the one already documented.

---

## Finding 5 — [MEDIUM] `hybrid_search` decomposition: score surfaced for `SearchMode::Vector` changes type

**File:** `src/search/hybrid.rs:192–204`

In `SearchMode::Vector`, the ranked list is:

```rust
SearchMode::Vector => vec_results
    .iter()
    .map(|r| (r.fact_id, f64::from(r.score)))  // f32 → f64 widening
    .collect(),
```

The `score` field in `SearchResult` is `f64`. After the decomposition, the
backend returns `Vec<(i64, f64)>` for both FTS and vector results. The plan must
ensure the backend's `vector_search` returns `(i64, f64)` (not `(i64, f32)` as
the current `VectorSearchStrategy::search` does), otherwise the engine-side
fusion step must do the widening — which is a behavioral no-op but must be
specified to avoid a score-type mismatch.

The RRF merge itself is safe: `rrf_merge` takes `(i64, f64)` for FTS and
`(i64, f32)` for vector — the f32→f64 widening is embedded in `rrf_merge`. If
the port's `vector_search` already widens to `f64` (as the plan's
`SearchIndex = Vec<(i64, f64)>` from the #629 trait suggests), the parity oracle
can verify this directly.

**Action:** Confirm in Stage C that `storage.vector_search()` returns `Vec<(i64,
f64)>` and that the score produced for `SearchMode::Vector` results is identical
to `f64::from(original_f32_score)`. Add this as a specific assertion in the
parity oracle (not just rank-order: also assert `score == expected_f64`).

---

## Finding 6 — [MEDIUM] HNSW dispatch parity: condition uses `search_config.ann_threshold`, which moves to the backend

**File:** `src/engine/mod.rs:379–387`

```rust
#[cfg(feature = "ann")]
fn should_use_hnsw(&self) -> bool {
    self.hnsw_strategy.as_ref().is_some_and(|hnsw| {
        hnsw.active_count()
            >= self
                .search_config
                .as_ref()
                .map_or(usize::MAX, |c| c.ann_threshold)
    })
}
```

The plan (§4) says the backend gains `search_config + ann_threshold + HnswStrategy`
and dispatches internally via the same predicate. This is correct in principle.

**Edge case:** when `search_config` is `None` (no ANN config provided), the
current engine returns `false` (threshold = `usize::MAX`, never satisfied). The
backend must replicate this: if no `search_config` is provided at backend
construction, `vector_search` must always use brute-force. The plan does not
explicitly state what `SqliteBackend::new()` does when `search_config` is `None`.

**Action:** Stage B must test the `search_config == None` case explicitly: HNSW
should never activate regardless of `active_count`. This is a low-risk item but
an easy regression.

---

## Finding 7 — [LOW] Test helpers that reach `engine.pool` directly

**File:** `src/engine/cycle/apply.rs` (tests) and multiple test files

Tests use `engine.with_read(|conn| FactStore::new(conn, DIM).get(id))` and
`engine.set_config(...)` / `engine.get_config(...)` via public API, not raw pool
access. The `with_read` helper is engine-side; after the cutover it becomes
`storage.block_read(|conn| …)`, but since `with_read` is a private method the
tests go through the public method, not the pool field.

However, `src/engine/archive.rs:tests` uses `engine.write_conn()` directly:

```rust
let conn = engine.write_conn().unwrap();
let store = FactStore::new(&conn, DIM);
```

This is a test-internal pool access pattern that will break. The plan notes
`~50–80 test helpers reach engine.pool/with_read directly` — the archive test
is one concrete instance. Stage E must add `#[cfg(test)] fn storage(&self)` OR
expose a `write_raw(|conn|)` test accessor to handle these.

**Verdict:** The plan's estimate of 50–80 affected helpers is plausible but
unverified. A pre-Stage-E grep of all `engine.write_conn()` / `engine.pool`
test usages is warranted to bound the actual count before committing to the
conversion.

---

## What the Plan Gets Right

- **Single connection for validate+apply** (`apply_cycle_report`): confirmed
  correct and the push-down design is the only safe choice.
- **Post-commit HNSW/graph updates outside the lock**: all three atomic methods
  (`insert_fact`, `insert_facts_batch`, `link_session_facts`) do this correctly
  today, and the plan preserves it.
- **`commit_archive` atomicity**: manifest insert + edge hard-delete + fact
  hard-delete in one `unchecked_transaction` — the plan's verbatim-body claim is
  accurate for this method. No in-memory state access mid-transaction.
- **`hybrid_search` decomposition**: RRF stays engine-side, I/O goes to port.
  The temporal post-filter (lines 242–251 in hybrid.rs) stays above the seam
  (engine-side) because it requires the query's `valid_at` context, not raw DB
  access. This is architecturally correct.
- **Drop/close split intent**: the plan correctly identifies that async Drop is
  not feasible. The only gap is the DB fingerprint access in `write_snapshot`
  (Finding 3).
- **Staging strategy**: front-loading additive preps (A–D) before the big-bang
  cutover is the correct approach. Each prep stage is independently testable.

---

## Summary Table

| #   | Severity | Location                                     | Issue                                                                                     |
| --- | -------- | -------------------------------------------- | ----------------------------------------------------------------------------------------- |
| 1   | BLOCKER  | `ingest.rs:458–465`, `mod.rs:621–639`        | `scope_tree.write()` inside write-lock lifetime — not a verbatim body move; must split    |
| 2   | HIGH     | `archive.rs:48,168,342`                      | Three direct `self.pool` accesses not covered by the 5 coarse methods table               |
| 3   | HIGH     | `mod.rs:531–568` (`write_snapshot` + `Drop`) | DB fingerprint read in `write_snapshot` — block_read in Drop is tokio-hazardous           |
| 4   | MEDIUM   | `cycle/apply.rs:72,364`                      | Plan correctly requires full push-down; explicitly state `validate_report` moves too      |
| 5   | MEDIUM   | `hybrid.rs:192–204`                          | Vector score type f32→f64 widening; parity oracle must assert score values, not just rank |
| 6   | MEDIUM   | `mod.rs:379–387`                             | `search_config == None` HNSW-never-active edge case must be tested in Stage B             |
| 7   | LOW      | `archive.rs:tests`, generally                | `engine.write_conn()` in tests; pre-Stage-E grep to bound the real count                  |
