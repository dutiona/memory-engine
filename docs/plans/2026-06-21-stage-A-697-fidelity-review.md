# Stage A (#697) Fidelity Review — Atomic Port Methods

Date: 2026-06-21  
Reviewer: adversarial clean-slate  
Scope: `src/storage/sqlite/graph.rs`, `src/storage/sqlite/consolidation.rs`,
`src/storage/sqlite/cold_storage.rs`, `src/storage/sqlite/schema.rs`
vs. original engine sites `src/engine/ingest.rs`, `src/engine/cycle/apply.rs`,
`src/engine/archive.rs`, `src/engine/graph.rs`.

---

## Verdict by method

| Method                          | Verdict                                                   |
| ------------------------------- | --------------------------------------------------------- |
| `insert_fact_atomic`            | FAITHFUL (one confirmed divergence: minor, annotated)     |
| `insert_facts_batch_atomic`     | FAITHFUL with one MEDIUM semantic gap                     |
| `insert_cosession_edges_atomic` | FAITHFUL                                                  |
| `apply_cycle_deltas_atomic`     | FAITHFUL with one HIGH divergence in `Promote` handling   |
| `commit_archive_atomic`         | FAITHFUL with one HIGH divergence: `created_at` timestamp |
| `get_config` / `set_config`     | FAITHFUL (trivial pass-through)                           |
| `select_archive_candidates`     | FAITHFUL                                                  |

---

## Method 1 — `insert_fact_atomic`

**New code:** `src/storage/sqlite/graph.rs:541–563`  
**Original:** `src/engine/ingest.rs:228–231` + `record_embedding_identity` (mod.rs:444–452)

### Transaction structure

Original:

```
let tx = conn.unchecked_transaction()?;
stamp_identity(&tx)?;
let id = FactStore::new(&tx, self.embed_dim).insert(&new_fact)?;
tx.commit()?;
```

Port:

```rust
let tx = conn.unchecked_transaction()?;
crate::store::embedding_meta::record_if_absent(&tx, &fingerprint, expected_dim)?;
let id = FactStore::new(&tx, dim).insert(&fact)?;
tx.commit()?;
```

One transaction, stamp before insert, rollback on any failure. **Identical structure.**

### Stamp logic

The engine uses `stamp_identity` which is a closure that calls either:

- `self.record_embedding_identity(conn, embedder)` → calls `record_if_absent(conn, &fp, self.embed_dim)`
- `record_if_absent(conn, declared, self.embed_dim).map(drop)` (precomputed path)

The port calls `record_if_absent` directly. This is correct — the port takes the fingerprint as an explicit parameter, collapsing both call sites. The `.map(drop)` in the precomputed path (discarding the return value) is equivalent to the port's `?` (which discards on success). **Faithful.**

### #614 guard

`record_if_absent` in `embedding_meta.rs:102` calls `ensure_match` which returns `MemoryError::EmbeddingModelMismatch` on a disagreement. The test at graph.rs:1170 confirms this variant propagates and that the fact row is absent after rollback. **Correct.**

### [LOW] Return value discarded

The original `record_if_absent` returns `Result<EmbeddingFingerprint>`. The precomputed engine path calls `.map(drop)`, the port calls it with `?` and discards the returned `EmbeddingFingerprint`. Both behaviours are identical (the returned fingerprint is not used at either call site). Not a bug.

---

## Method 2 — `insert_facts_batch_atomic`

**New code:** `src/storage/sqlite/graph.rs:568–641`  
**Original:** `src/engine/ingest.rs:396–488` (Phase 3 only — DB body)

### Savepoint structure

Original:

```
conn.execute_batch("SAVEPOINT batch_insert")?;
// ... inner closure ...
match result {
    Ok((ids, scope_ids_to_cache)) => {
        conn.execute_batch("RELEASE batch_insert")?;
        // <--- scope_tree.write() ABOVE THE SEAM ---
        ids
    }
    Err(e) => {
        conn.execute_batch("ROLLBACK TO batch_insert")?;
        conn.execute_batch("RELEASE batch_insert")?;
        return Err(e);
    }
}
```

Port:

```rust
conn.execute_batch("SAVEPOINT batch_insert")?;
match result {
    Ok(pair) => { conn.execute_batch("RELEASE batch_insert")?; Ok(pair) }
    Err(e) => {
        let _ = conn.execute_batch("ROLLBACK TO batch_insert");
        let _ = conn.execute_batch("RELEASE batch_insert");
        Err(e)
    }
}
```

Savepoint semantics identical. `scope_tree.write()` correctly remains engine-side (above the seam).

### Scope resolution

Original uses `resolve_batch_scopes` which iterates `prepared` (which is a `Vec<PreparedBatchEntry>` embedding entries' `AddFactRequest.scope`) and returns `(scope_ids, scope_ids_to_cache)`.

Port receives `scope_paths: &[Option<String>]` — the caller must extract the paths from the `AddFactRequest` vector before calling. This is the declared interface split. The iteration logic (`None => 1 (root)`, dedup via `HashMap`) is identical.

### [MEDIUM] `scope_ids_to_cache` includes ONLY new paths, not root-scope

Original `resolve_batch_scopes`: paths that map to `None` go to scope_id=1 (root), and only paths that go through `scope_store.ensure_path` are tracked in `scope_cache`. Only those paths end up in `unique_scope_ids = scope_cache.into_values()`. The root scope (1) is never in `scope_ids_to_cache`.

Port: same logic — `None => 1` does not touch `scope_cache`, only `Some(path)` entries do. Root scope is never cached. This matches the original. **Faithful.**

**However:** the original engine also calls `scope_store.get(sid)` on each cached id inside the `scope_tree.write()` loop before inserting into the tree. The port only returns the raw ids; it is the engine caller's responsibility to call `.get()` on each before inserting. This is above-the-seam and not the port's concern — it is documented in the trait. No bug here, but the caller must not skip the `get()` step.

### Stamp position

Original stamps identity at position 1 of the savepoint body, before scope resolution. Port does the same (line 587, before `scope_store` creation). **Faithful.**

### [LOW] `scope_ids_to_cache` returns `into_values()` with no deterministic order

The original also uses `scope_cache.into_values().collect()`, so the ordering is non-deterministic in both. Not a semantic issue (the caller iterates all of them), but worth noting for tests that might compare slices directly.

---

## Method 3 — `insert_cosession_edges_atomic`

**New code:** `src/storage/sqlite/graph.rs:644–688`  
**Original:** `src/engine/graph.rs:70–102`

### Transaction structure

Original uses `conn.unchecked_transaction()` wrapping the dedup query + edge inserts. Port does the same. **Identical.**

### Loop logic

Original iterates `facts` (a `Vec<Fact>` from `list_active_by_session`), accesses `.id`. Port receives `fact_ids: &[i64]` directly — the caller extracts ids before calling. Loop bounds (`for i in 0..fact_ids.len(); for j in (i+1)..`)`, bidirectional pairs, dedup via `existing.contains`, `NewEdge` construction: **byte-identical.**

### Constant values

Original uses `Self::CO_SESSION_RELATION`, `Self::CO_SESSION_WEIGHT`, `Self::CO_SESSION_SCOPE_ID`. Port receives `relation`, `weight`, `scope_id` as parameters — the caller supplies the same constants. The engine caller is responsible for passing the correct values. No semantic gap introduced by the port; the constants are defined in the engine and must be passed correctly.

### In-memory graph update

The post-commit `self.graph.write()` loop remains engine-side. The port only returns the new edge triples for the caller to mirror. **Correct.**

### Rollback test

`insert_cosession_edges_atomic_rollback_on_error` (graph.rs:1359) drops the `edges` table, calls the method, and asserts an error. It only checks `is_err()` — it does NOT verify the `facts` table is unchanged (which is not meaningful here since only edges are written). The rollback assertion is adequate for this method.

---

## Method 4 — `apply_cycle_deltas_atomic`

**New code:** `src/storage/sqlite/consolidation.rs:169–559`  
**Original:** `src/engine/cycle/apply.rs:70–340`

This is the highest-risk method. Analysis proceeds variant by variant.

### Connection discipline (TOCTOU / deadlock)

Original acquires one `write_conn()` across both `validate_report` (line 75) and the apply transaction (line 79–302). This prevents:

1. A TOCTOU gap between validate and apply (same connection sees a consistent snapshot).
2. Self-deadlock on a non-reentrant SQLite connection mutex.

Port uses a single `block_write` closure. Both validate and apply execute on the same `conn` reference. **No TOCTOU gap reintroduced.** The comment at line 200 (`// Validation pass ... on the held connection`) confirms this is intentional.

### [HIGH] `Promote` variant: semantic divergence from the original

**Original** (`apply.rs:147–171`):

```rust
CycleDelta::Promote { fact_id, provenance } => {
    let source = FactStore::new(&tx, self.embed_dim).get(*fact_id)?;
    let req = PromoteRequest { ... scope: None, ... };
    let promoted = self.promote_in_conn(&tx, &req)?;
    result.promoted += 1;
    result.promoted_fact_ids.push(promoted.fact_id);
    #[cfg(feature = "ann")]
    to_index.push((promoted.fact_id, source.embedding));
}
```

`promote_in_conn` (`cognitive.rs:379–455`) does:

1. Validates embedding dimension.
2. Calls `require_present(conn)` — #613/#615 guard.
3. Normalises metadata.
4. Injects `promotion_provenance` into metadata.
5. Resolves scope (always `None` → `scope_id=1`).
6. Inserts the promoted fact with `is_pinned: true`.
7. Inserts the lineage record (with source fact id, NOT `source_fact_ids: vec![*fact_id]` — wait, the `req.source_fact_ids` includes the `*fact_id`).
8. Returns `PromotionResult { fact_id, lineage_id }`.

**Port** (`consolidation.rs:373–429`):

```rust
CycleDelta::Promote { fact_id, provenance } => {
    let source = FactStore::new(&tx, embed_dim).get(*fact_id)?;
    // #613/#615 — promotion identity guard
    crate::store::embedding_meta::require_present(&tx)?;
    // Inject provenance into metadata
    let mut metadata = ...;
    if let serde_json::Value::Object(ref mut map) = metadata {
        map.insert("promotion_provenance".to_owned(), ...);
    }
    let prov_clone = provenance.clone();
    let promote_fact = NewFact { ..., scope_id: 1, is_pinned: true };
    let promoted_id = FactStore::new(&tx, embed_dim).insert(&promote_fact)?;
    LineageStore::new(&tx).insert(
        &NewLineageRecord {
            wisdom_fact_id: promoted_id,
            source_fact_ids: vec![*fact_id],
        },
        &prov_clone,
    )?;
    result.promoted += 1;
    result.promoted_fact_ids.push(promoted_id);
    to_index.push((promoted_id, source.embedding));
}
```

The DB operations (fact insert + lineage insert) are **reproduced correctly**. However:

**Sub-finding [HIGH-a]:** The port's `to_index.push` is **unconditional** (no `#[cfg(feature = "ann")]` guard). The original only pushes to `to_index` under `#[cfg(feature = "ann")]`. This means in non-ANN builds, the port collects `to_index` entries that the caller (currently the engine) will never consume, and the engine's `notify_insert` call is also guarded by `#[cfg(feature = "ann")]`. This is a **semantic mismatch**:

- In the current Stage A the engine still owns the apply path and the port method is not yet called by the engine — the return value `to_index` is unused by the engine. So it is not a data-corruption bug in Stage A.
- When Stage E wires the engine to call this method, the unconditional `to_index` population could cause unnecessary memory allocation in non-ANN builds, but will not cause corruption (the caller's HNSW notify call is cfg-guarded).
- Classification: **[MEDIUM]** rather than HIGH in the current stage, but must be resolved before Stage E.

**Sub-finding [HIGH-b]:** `promote_in_conn` also validates the embedding dimension (`req.embedding.len() != self.embed_dim`). The port does NOT reproduce this check for the `Promote` variant. In `validate_report` (the pre-apply validation that runs before this closure), the `Promote` arm does NOT call `self.validate_new_fact(nf)` — it only checks `ensure_active`. The dimension check for `Promote` embeddings is entirely absent from both the validation pass AND the apply pass in the port.

Original flow:

- `validate_report` for Promote: only `require_fact` + `ensure_active` — no dim check.
- `promote_in_conn` for Promote: dim check at line 386.

Port flow:

- Validation pass: same as original — no dim check on Promote.
- Apply pass: NO dim check (the port inlines `promote_in_conn` but skips the dim check).

**Impact:** If a `CycleDelta::Promote` is applied via `apply_cycle_deltas_atomic` and `source.embedding.len() != embed_dim`, the fact will be inserted with a wrong-dimension embedding. This can only happen if the DB already contains a wrong-dimension embedding (which would be a pre-existing data integrity violation), so the real-world risk is low — but the guard is still missing and should be present for defence-in-depth.

Classification: **[HIGH]** — missing dimension guard on the `Promote` path in the port's apply pass.

### Validation pass

Port inlines `validate_report` (consolidation.rs:201–315). Line-by-line comparison:

- `AddFact`: dim check ✓, importance check ✓, `check_str_size` ✓, `check_json_size` ✓.
- `AdjustScore`: `abs() > MAX_ADJUSTMENT` ✓, `require_fact` ✓, `ensure_active` ✓.
- `Quarantine`: `require_fact` ✓, `ensure_active` ✓, `expired_in_report.insert` ✓.
- `Promote`: `require_fact` ✓, `ensure_active` ✓. (dim check is missing here too — but also absent in original's `validate_report`, so this is faithful to the original).
- `TagOutcome`: `require_fact` ✓ (no active-state check — matches original).
- `Supersede`: `require_fact` on old and new ✓, `ensure_active` on both ✓, `expired_in_report.insert(old_id)` ✓.
- `Synthesize`: dim check ✓, importance check ✓, `check_str_size` ✓, `check_json_size` ✓, `SynthesizeNoSources` ✓, per-source `require_fact` + `ensure_active` + `expired_in_report.insert` ✓.
- `processed_ids` loop ✓.

**Validation pass is faithful.**

### Apply pass — remaining variants

- `AddFact`: `FactStore::insert` ✓, `result.new_fact_ids.push` ✓, `result.facts_added += 1` ✓, `to_index.push` (see HIGH-a above).
- `AdjustScore`: `store.get(*fact_id)?.importance` ✓, `mul_add(IMPORTANCE_STEP, current)` ✓, `.clamp(0.0, 1.0)` ✓, `result.scores_adjusted += 1` ✓.
- `Quarantine`: `store.expire` ✓, `store.merge_metadata` with same JSON shape ✓, `expired_ids.push` ✓, `result.quarantined += 1` ✓.
- `TagOutcome`: `NewEvent` with same fields ✓, `EventStore::new(&tx, &upcaster_registry).insert` ✓, `result.outcomes_tagged += 1` ✓.
- `Supersede`: `store.get(*new_id)` ✓, `store.expire(*old_id, now)` ✓, `EdgeStore::insert` with same `NewEdge` fields ✓, `expired_ids.push(*old_id)` ✓, `supersede_edges.push((*new_id, *old_id, edge_id))` ✓, `supersede_new_ids.push(*new_id)` ✓, `result.superseded += 1` ✓.
- `Synthesize`: `store.insert(new_fact)` ✓, range_start/range_end loop ✓, `store.expire(*src, now)` ✓, `EdgeStore::insert` per source ✓, `expired_ids.push(*src)` ✓, `supersede_edges.push((synth_id, *src, edge_id))` ✓, `PromotionProvenance` construction identical ✓, `LineageStore::insert` ✓, `synthesize_new_ids.push` ✓, `result.synthesized_fact_ids.push` ✓, `result.synthesized += 1` ✓, `to_index.push` (see HIGH-a above).

### Invariant M (dream-mark)

Port lines 519–532: `to_mark = processed_ids ∪ new_fact_ids ∪ promoted_fact_ids ∪ supersede_new_ids ∪ synthesize_new_ids`, sort, dedup, `mark_dream_cycled`. **Identical to original.**

### Watermark and history

`set_config(LAST_DREAM_CYCLE_AT, ...)` ✓  
`append_cycle_history` inlined (lines 541–553) — identical body to `apply.rs:467–479` ✓  
`tx.commit()` ✓

### `#613` guard position

Original: after validation pass, before `unchecked_transaction()`:

```
if report.deltas.iter().any(|d| matches!(d, CycleDelta::AddFact(_) | CycleDelta::Synthesize { .. })) {
    crate::store::embedding_meta::require_present(&tx)?;  // <-- inside the tx
}
```

Wait — re-reading the original: the `require_present` call is at apply.rs:91–94 with `&tx` (the transaction object). But `tx` is created at line 80, and the `require_present` call at line 93 is INSIDE the transaction. The port does the same: `require_present(conn)` at consolidation.rs:323–328, BEFORE `conn.unchecked_transaction()` — i.e., NOT inside the transaction.

This is a subtle difference: the original checks `require_present` inside the transaction (`&tx`), the port checks it on the bare connection (`conn`) before starting the transaction. For SQLite this is semantically identical because both the bare connection and the transaction see the same database state (autocommit mode for the bare check vs. snapshot inside the tx are the same for a READ-only operation on a fresh tx). But it is technically a deviation.

**Classification: [LOW]** — no functional impact on SQLite because `require_present` is a read-only check. On a hypothetical concurrent-write scenario the transaction gives a slightly stronger guarantee, but this engine is single-writer.

---

## Method 5 — `commit_archive_atomic`

**New code:** `src/storage/sqlite/cold_storage.rs:72–124`  
**Original:** `src/engine/archive.rs:230–279` (the `commit_archive` method)

### Transaction structure

Both: `conn.unchecked_transaction()` → `ArchiveManifestStore::insert` → `EdgeStore::hard_delete_by_facts` → `FactStore::hard_delete_ids` → `tx.commit()`. **Identical order, same FK-safety guarantee (edges first, then facts).**

### [HIGH] `created_at` timestamp divergence

**Original `commit_archive` (archive.rs:239)**:

```rust
let now = Utc::now();
// ...
ArchiveManifestStore::new(&tx).insert(pak_filename, now, facts.len() as i64, ...)
```

The original computes `created_at = Utc::now()` itself inside `commit_archive`.

**Port `commit_archive_atomic` signature:**

```rust
async fn commit_archive_atomic(
    &self,
    pak_filename: &str,
    created_at: DateTime<Utc>,
    ...
)
```

The port receives `created_at` as a parameter. The caller must supply it. If the engine caller passes `Utc::now()` at the call site (outside the transaction), the timestamp will differ from the original's (which captures `now` inside the lock/transaction). This is a **semantic deviation**: the original timestamps the manifest row at the moment the write lock + transaction begin, the port timestamps it at the caller's discretion.

This does not cause data corruption (the manifest row will still be inserted), but the `created_at` field in the manifest entry will reflect the call site's clock, not the transaction's clock. The field is used for ordering (`list_archive_manifest` returns entries `ORDER BY created_at ASC`).

In the current Stage A the engine still calls `commit_archive` directly (the port method is not wired), so there is no regression. But the caller contract must be documented explicitly for Stage E: the caller MUST NOT pass a pre-computed timestamp from before the `.pak` write; the timestamp should be captured at the transaction boundary.

**Classification: [HIGH]** — caller contract change with ordering implications. Must be noted for Stage E wiring.

### No in-memory read mid-tx

The original `commit_archive` (archive.rs) computes `fact_id_min`, `fact_id_max`, `t_created_min`, `t_created_max` **outside** the transaction (from `facts` and `edges` slices, pre-fetched by `select_archive_candidates`). The port receives these values as parameters. **No in-memory read mid-transaction in either.**

### Rollback (crash-injection)

There is no `commit_archive_atomic` rollback test in `cold_storage.rs`. The existing archive.rs tests (`archive_cleans_up_pak_when_commit_fails`) exercise the engine path, not the port method.

**Classification: [MEDIUM]** — missing parity/rollback test for `commit_archive_atomic` directly. The engine-level test covers the commit failure path, but does not exercise the port path's atomicity guarantee in isolation. Should be added.

---

## Method 6 — `get_config` / `set_config`

**New code:** `src/storage/sqlite/schema.rs:103–114`  
**Original:** `src/store/schema/mod.rs:123,159` (free functions)

Port is a direct pass-through via `block_read`/`block_write`. **Trivially faithful.**

---

## Method 7 — `select_archive_candidates`

**New code:** `src/storage/sqlite/graph.rs:691–704`  
**Original:** `src/engine/archive.rs:167–177`

Port uses `block_read` (correctly — read-only). Original uses `self.pool.read()`. Both call:

- `FactStore::new(conn, dim).list_archive_candidates(expired_before)`
- `EdgeStore::new(conn).list_internal_by_facts(&candidate_ids)`

**Faithful.** The parity test at graph.rs:1394 confirms oracle equality.

---

## Cross-cutting checks

### `block_write` vs `block_read` usage

All write methods use `block_write`. `select_archive_candidates` uses `block_read`. **Correct.**

### Borrowed args → `'static` closures

All `&str`, `&[T]`, `&T` arguments are cloned/converted to owned values before entering the `move` closure. Confirmed by inspection:

- `fact.clone()` / `fingerprint.clone()` / `facts.to_vec()` / `scope_paths.to_vec()` (graph.rs)
- `report.clone()` / `upcaster_registry.clone()` (consolidation.rs)
- `pak_filename.to_owned()` / `blake3_hash.to_owned()` / `fact_ids.to_vec()` (cold_storage.rs)

**No aliasing bugs found.**

### ReadOnly preservation

`block_write` routes through the pool's `try_write()` which returns `MemoryError::ReadOnly` on a read-only pool. The H4 test at graph.rs:782 confirms this. **Correct.**

### Tests assert byte-identical rollback (not just `is_err()`)

- `insert_fact_atomic_rollback_on_fingerprint_mismatch` (graph.rs:1170): asserts `all.is_empty()` after rollback. ✓ byte-identical.
- `insert_facts_batch_atomic_rollback_on_stamp_error` (graph.rs:1277): asserts `list_all_facts().len() == 1` (only seed remains). ✓ byte-identical.
- `insert_cosession_edges_atomic_rollback_on_error` (graph.rs:1359): asserts `is_err()` only — does NOT assert that no edges were written (it can't, because the `edges` table was dropped). The rollback proof is implicit (the table is gone), but a test that asserts the facts table is unchanged would be stronger. **[LOW] test gap.**
- `commit_archive_atomic`: **no rollback test** (see MEDIUM above).
- `apply_cycle_deltas_atomic`: no dedicated rollback test in `consolidation.rs`. The engine-level tests in `apply.rs` (e.g., `out_of_range_adjustment_is_rejected_and_leaves_store_unchanged`) cover the original engine path, not the port method. **[MEDIUM] test gap.**

---

## Summary of findings

| ID  | Classification | Location (new)                          | Location (original)    | Description                                                                                                                |
| --- | -------------- | --------------------------------------- | ---------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| F1  | [HIGH]         | `consolidation.rs:373–429`              | `cognitive.rs:385–390` | `Promote` variant missing embedding dimension check in apply pass                                                          |
| F2  | [HIGH]         | `cold_storage.rs:72–124`                | `archive.rs:239`       | `created_at` timestamp is caller-supplied, not captured at tx start                                                        |
| F3  | [MEDIUM]       | `consolidation.rs:344–348`              | `apply.rs:89–94`       | `require_present` called on bare `conn` before tx, not inside tx (low functional impact on SQLite, deviation in principle) |
| F4  | [MEDIUM]       | `consolidation.rs:344–349` + `466–513`  | `apply.rs:99,121,269`  | `to_index.push` is unconditional (no `#[cfg(feature = "ann")]`); no-op in Stage A but must be resolved before Stage E      |
| F5  | [MEDIUM]       | `cold_storage.rs` (tests absent)        | —                      | No crash-injection / rollback test for `commit_archive_atomic`                                                             |
| F6  | [MEDIUM]       | `consolidation.rs` (tests absent)       | —                      | No dedicated rollback test for `apply_cycle_deltas_atomic` below-the-seam                                                  |
| F7  | [LOW]          | `graph.rs:1359–1385`                    | —                      | `insert_cosession_edges_atomic` rollback test only checks `is_err()`, not byte-identical table state                       |
| F8  | [LOW]          | `consolidation.rs:323` vs `apply.rs:93` | —                      | `require_present` called before `unchecked_transaction()` in port vs. inside tx in original (SQLite-equivalent)            |

### Faithful (no issues)

- `insert_fact_atomic`: faithful. Stamp-before-insert invariant preserved. Rollback tested byte-identically.
- `insert_facts_batch_atomic`: faithful. Savepoint structure, stamp position, scope-cache logic all match.
- `insert_cosession_edges_atomic`: faithful. Dedup, bidirectional loop, in-memory graph update correctly left engine-side.
- `select_archive_candidates`: faithful.
- `get_config` / `set_config`: trivially faithful.
- Validation pass in `apply_cycle_deltas_atomic`: faithful to `validate_report`.
- Invariant M (dream-mark), watermark, history in `apply_cycle_deltas_atomic`: faithful.

### Required action before Stage E

- **F1** (`Promote` dim check): add `if source.embedding.len() != embed_dim { return Err(EmbeddingDimension { ... }) }` in the port's `Promote` apply arm.
- **F2** (`commit_archive_atomic` timestamp): document that `created_at` must be captured at the call site immediately before the method call; or change the method to capture it internally.
- **F4** (`to_index` cfg guard): wrap the `to_index.push(...)` lines in the port's `AddFact`, `Promote`, `Synthesize` arms with `#[cfg(feature = "ann")]`.
- **F5 + F6**: add crash-injection rollback tests for both `commit_archive_atomic` and `apply_cycle_deltas_atomic` in the respective `sqlite/` test modules.
