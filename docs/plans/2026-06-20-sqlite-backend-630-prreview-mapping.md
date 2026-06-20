# PR #682 (#630) — `SqliteBackend` delegation-mapping review

**Lens:** behavior-preservation / delegation-mapping fidelity. The PR re-homes the
concrete SQLite stores behind a trait family by delegating to them verbatim. The
danger is a silently mis-mapped method (wrong concrete method, wrong conn, dropped
or reordered arg, lost semantic).

**Verdict: APPROVE.** All 7 impl files reviewed method-by-method against both the
trait contract (`src/storage/*.rs`) and the concrete store
(`src/store/*`, `src/search/{fts,vector}.rs`). Every delegation maps to the
semantically-equivalent concrete method, with the correct conn, faithful args, and
faithful return shape. No mis-mapping, no dropped semantic, no stub. The two
"looks-like-a-read-but-writes" traps (`insert_or_reinforce_fact`,
`ensure_scope_path`) and the third write-via-UPDATE (`stamp_facts_surfaced`) are all
correctly routed to `block_write`. The `scope_ids` non-uniform empty contract is
preserved verbatim everywhere. `embed_dim` is `self.embed_dim` at every call site
(never `0`). The f32→f64 vector-score widening and the `FtsResult`/`VectorResult`
tuple projections are exact.

No BLOCKER / HIGH / MEDIUM findings. Two LOW observations (documentation/robustness,
not behavior deltas) are recorded at the end.

---

## Method-by-method mapping verification

### `graph.rs` — `FactGraph` (49 methods)

Concrete surface cross-check: `FactStore` has exactly 30 `pub`/`pub(crate)` methods
(`grep -oE "pub(\(crate\))? fn"` → 30 unique). All 30 map to exactly one `FactGraph`
fact method; none dropped:

| concrete `FactStore::*`            | trait method                              | conn   | OK |
| ---------------------------------- | ----------------------------------------- | ------ | -- |
| `insert`                           | `insert_fact`                             | WRITE  | ✓  |
| `insert_or_reinforce`              | `insert_or_reinforce_fact`                | WRITE† | ✓  |
| `expire`                           | `expire_fact`                             | WRITE  | ✓  |
| `set_pinned`                       | `set_fact_pinned`                         | WRITE  | ✓  |
| `update_importance`                | `update_fact_importance`                  | WRITE  | ✓  |
| `update_importance_score`          | `update_fact_importance_score`            | WRITE  | ✓  |
| `increment_access`                 | `increment_fact_access`                   | WRITE  | ✓  |
| `merge_metadata`                   | `merge_fact_metadata`                     | WRITE  | ✓  |
| `mark_dream_cycled`                | `mark_facts_dream_cycled`                 | WRITE  | ✓  |
| `stamp_surfaced`                   | `stamp_facts_surfaced`                    | WRITE† | ✓  |
| `hard_delete_ids`                  | `hard_delete_facts`                       | WRITE  | ✓  |
| `get`                              | `get_fact`                                | READ   | ✓  |
| `get_many`                         | `get_facts`                               | READ   | ✓  |
| `list_all`                         | `list_all_facts`                          | READ   | ✓  |
| `for_each`                         | `for_each_fact`                           | READ‡  | ✓  |
| `max_caller_written_fact_id`       | `max_caller_written_fact_id`              | READ   | ✓  |
| `list_active`                      | `list_active_facts`                       | READ   | ✓  |
| `list_active_scoring`              | `list_active_facts_scoring`               | READ   | ✓  |
| `list_active_at`                   | `list_active_facts_at`                    | READ   | ✓  |
| `list_dormant`                     | `list_dormant_facts`                      | READ   | ✓  |
| `list_by_scope_importance`         | `list_facts_by_scope_importance`          | READ   | ✓  |
| `list_by_scopes_importance`        | `list_facts_by_scopes_importance`         | READ   | ✓  |
| `list_by_importance_score`         | `list_facts_by_importance_score`          | READ   | ✓  |
| `list_pinned`                      | `list_pinned_facts`                       | READ   | ✓  |
| `list_due`                         | `list_due_facts`                          | READ   | ✓  |
| `next_due_time`                    | `next_due_time`                           | READ   | ✓  |
| `list_by_scopes_recent`            | `list_facts_by_scopes_recent`             | READ   | ✓  |
| `list_active_by_metadata_key_recent` | `list_active_facts_by_metadata_key_recent` | READ | ✓  |
| `list_active_in_period`            | `list_active_facts_in_period`             | READ   | ✓  |
| `list_undreamt_in_period`          | `list_undreamt_facts_in_period`           | READ   | ✓  |
| `list_active_by_session`           | `list_active_facts_by_session`            | READ   | ✓  |

† **Write-conn trap correctly handled.** `insert_or_reinforce` is SELECT-then-
UPDATE/INSERT (`facts.rs` ~162: `query_row("SELECT id …")` → `execute("UPDATE …")`);
`stamp_surfaced` is `execute("UPDATE facts SET surfaced_at …")` then a SELECT
read-back (`facts.rs` 599-628). Both *write*, so `block_write` is correct and
necessary — a read pool would (correctly) reject them with `ReadOnly`.
‡ `for_each_fact` routes through `for_each_streamed` (which acquires `pool.read()`),
the right primitive for a streaming scan.

`EdgeStore` (12 methods) — all mapped:
`insert→insert_edge` (W), `get→get_edge` (R), `expire→expire_edge` (W),
`expire_by_fact→expire_edges_by_fact` (W), `list_all→list_all_edges` (R),
`for_each→for_each_edge` (R), `list_active→list_active_edges` (R),
`list_active_by_source→list_active_edges_by_source` (R),
`list_active_by_target→list_active_edges_by_target` (R),
`exists_active→edge_exists_active` (R),
`list_active_pairs_by_facts→list_active_edge_pairs_by_facts` (R),
`hard_delete_by_facts→hard_delete_edges_by_facts` (W). All correct.

`ScopeStore` (6 methods) — all mapped:
`get→get_scope` (R), `find_by_label→find_scope_by_label` (R),
`insert→insert_scope` (W), `ensure_path→ensure_scope_path` (**W, trap†**),
`list_all→list_all_scopes` (R), `for_each→for_each_scope` (R).
`ensure_path` is `INSERT OR IGNORE` then SELECT (`scopes.rs` 107-140) — it writes;
`block_write` correct.

**Args:** every borrowed arg is `.to_vec()`/`.to_owned()`/`.clone()`/`.copied()` to
owned with no change of meaning. `Option<&[i64]>` → `.map(<[i64]>::to_vec)` then
`.as_deref()` (`list_dormant_facts`), preserving `None` vs `Some(empty)`. `&[i64]`
scope slices forwarded verbatim — no normalization (the empty=ALL vs empty=NONE
contract is left to the concrete SQL, exactly as before). `marker_key` forwarded
unmodified so the concrete injection guard fires (test
`list_active_by_metadata_key_recent_rejects_invalid_keys` pins it). `dim =
self.embed_dim` at every `FactStore::new(c, dim)` call site.

**Returns:** identity types (`Fact`, `Edge`, `ScopeNode`, `HashMap`, `HashSet`,
tuples) pass through unchanged.

### `event_log.rs` — `EventLog` (7 methods)

`EventStore::new(c, &registry)` is a 2-arg ctor; the backend clones
`Arc<UpcasterRegistry>` into every closure and never lets it cross the trait
boundary (matches the trait doc). Mapping:
`insert→insert_event` (W), `get→get_event` (R), `list→list_events` (R),
`count→count_events` (R), `for_each→for_each_event` (R),
`get_upcasted→get_upcasted_event` (R), `list_upcasted→list_upcasted_events` (R).
Raw vs upcasted reads correctly split (`get` vs `get_upcasted`,
`list` vs `list_upcasted`) — verified by test `upcasted_read_applies_registry`.

### `search_index.rs` — `SearchIndex` (3 methods)

- `lexical_search` → `fts_search(c, &query, k, fact_type.as_ref(), scope_ids.as_deref())`.
  Concrete sig `(conn, query, limit, fact_type, scope_ids)`, arg order exact
  (`k`→`limit`). `FtsResult.score` is `f64` → `(r.fact_id, r.score)` pass-through, no
  cast. ✓
- `vector_search` → `vector_search(c, &q, dim, k, fact_type.as_ref(), scope_ids.as_deref())`.
  Concrete sig `(conn, query_embedding, embed_dim, limit, fact_type, scope_ids)`,
  arg order exact; `dim = self.embed_dim` (the dim-guard `EmbeddingDimension` error
  is raised by the concrete fn — test `vector_wrong_dim_is_embedding_dimension_error`
  pins `expected: DIM`, confirming dim is NOT 0). `VectorResult.score` is `f32` →
  widened `f64::from(r.score)` (value-exact, order-preserving). ✓
- `lexical_count_expired` → `fts_count_expired(c, &query, fact_type.as_ref(), scope_ids.as_deref())`.
  Bypasses `FactFilter` (takes raw `Option<&FactType>` + `Option<&[i64]>`), matching
  the concrete fn shape; the intrinsic `t_expired IS NOT NULL` predicate stays in the
  concrete SQL. ✓

`convert::search_params` projects `FactFilter` → `(fact_type, scope_ids)` and
**errors loud** on any dimension the verbatim SQL cannot honor (`temporal != Active`,
`ids`, `pinned`, `metadata`) instead of silently dropping a predicate — this is the
correct fail-closed choice for a "zero behavior change" seam (the alternative,
silently honoring fewer predicates, *would* be a behavior change). `Some(empty)`
round-trips as `Some(vec![])` (matches nothing), not normalized to `None` — pinned by
`empty_scope_ids_round_trip_preserved` and `lexical_scope_some_empty_matches_nothing_none_finds`.

### `consolidation.rs` — `ConsolidationStore` (12 methods)

`SummaryStore` (7): `insert→insert_summary` (W), `get→get_summary` (R),
`list_by_level→list_summaries_by_level` (R), `list_all→list_all_summaries` (R),
`for_each→for_each_summary` (R), `delete_by_level→delete_summaries_by_level` (W).
`SummaryStore::new(c, dim)` uses `self.embed_dim` for the embedding (de)serialize
path. ✓
`LineageStore` (8): `insert→insert_lineage` (W, forwards both `record` and
`provenance`), `insert_raw→insert_lineage_raw` (W),
`get_by_wisdom_fact→get_lineage_by_wisdom_fact` (R, returns the
`(LineageRecord, PromotionProvenance)` tuple unchanged),
`get_source_fact_ids→get_lineage_source_fact_ids` (R), `delete→delete_lineage` (W),
`has_lineage→has_lineage` (R), `for_each→for_each_lineage` (R). `LineageStore::new(c)`
is a 1-arg ctor (no dim) — correct. ✓

### `session.rs` — `SessionStore` (10 methods)

`ActivityStore` (6): `insert_or_dedup→insert_or_dedup_activity` (W, forwards
`dedup_window_secs`), `get→get_activity` (R),
`list_by_session→list_activities_by_session` (R),
`list_recent_by_scope→list_recent_activities_by_scope` (R, **empty scope_ids ⇒ empty
result, NOT all** — pinned verbatim; test
`list_recent_by_scope_empty_slice_returns_empty`),
`update_status→update_activity_status` (W, forwards `status` + `promoted_fact_id`),
`count_by_session→count_activities_by_session` (R).
`CheckpointStore` (5): `upsert→upsert_checkpoint` (W), `get→get_checkpoint` (R),
`get_by_scope→get_checkpoint_by_scope` (R), `list_recent→list_recent_checkpoints` (R).
The module doc notes the previously `#[cfg(test)]` reads were un-gated in the same
commit so the trait can call them unconditionally — a deliberate, documented change,
not a stub. ✓

### `schema.rs` — `SchemaManager` (8 methods)

- `migrate` → `migrate(c, None)` (W). The concrete sig is
  `migrate(conn, backup_dir: Option<&Path>)`; passing `None` is documented (the pool
  already migrated at open; this is an idempotent at-HEAD re-check, no backup at this
  level). Faithful to the prior open path, which never took a backup at this point.
- `schema_version` → reads `get_config(c, "schema_version")` with `"1"` default,
  parsing to `u32` (R). Matches the concrete default-and-parse used inside
  `validate_schema_version` (`schema/mod.rs` 325). ✓
- `validate_schema_version` → `validate_schema_version(c)` (R). Verified the concrete
  fn is read-only (only `query_row` / `get_config`, no `execute`; `schema/mod.rs`
  293-347) — correctly routed to `block_read`, preserving the read-only open path
  (Key Design Decision #6). ✓
- `capabilities` — sync, returns the fixed SQLite tier
  (`Bm25` / `true_idf = true` / `server_side_vector = false`). Not a delegation; a new
  constant surface introduced by #629's trait. Matches the documented SQLite tier. ✓
- `embedding_meta`: `load→load_embedding_fingerprint` (R),
  `store→store_embedding_fingerprint` (W),
  `record_if_absent→record_embedding_fingerprint_if_absent` (W, forwards
  `candidate` + `expected_dim`; the dim-mismatch `EmbeddingDimension` guard fires in
  the concrete fn — test `record_if_absent_dim_mismatch_yields_embedding_dimension`),
  `require_present→require_embedding_fingerprint_present` (R). ✓

### `cold_storage.rs` — `ColdStorage` (3 methods, cfg archive+async)

`ArchiveManifestStore` (3): `insert→insert_archive_manifest` (W),
`list→list_archive_manifest` (R), `delete→delete_archive_manifest` (W).
The 10-arg `insert` forwarding was checked position-by-position against the concrete
sig (`archive_manifest.rs` 27-58) — `pak_path, created_at, fact_count, edge_count,
fact_id_min, fact_id_max, t_created_min, t_created_max, size_bytes, blake3_hash` — no
swap of the look-alike pairs (`fact_count`/`edge_count`,
`fact_id_min`/`fact_id_max`, `t_created_min`/`t_created_max`). ✓

---

## Seam-primitive fidelity (mod.rs)

- `block_read` → `pool.read()`; `block_write` → `pool.try_write()` (so a read-only
  pool yields `ReadOnly` — Key Design Decision #6). Verified by
  `block_write_on_read_only_pool_yields_read_only` and the per-trait
  read-only-rejects-write tests.
- `map_seam_err` opacifies only `MemoryError::Database` → `Storage(Backend)` (D4);
  semantic variants (`NotFound`, `Migration`, `EmbeddingDimension`, `Conflict`,
  `ReadOnly`, `Internal`, `Lineage`, …) pass through. Pinned by
  `block_read_maps_database_error_to_storage_backend` /
  `block_read_passes_semantic_variant_through` and the per-trait `*_yields_not_found`
  / `*_yields_lineage_error` tests.
- `for_each_streamed` cap-1 backpressure; **callback error wins** over the scan's
  induced send failure (`for_each_streamed_callback_error_wins_and_stops_early`),
  mid-scan SQL error surfaced + remapped
  (`for_each_streamed_surfaces_mid_scan_error`). Faithful to a synchronous
  `for_each` that stops on the first callback `Err`.

## Completeness (no missing / stubbed method)

`backend.rs` R-COMPLETE checklist matches the concrete surface exactly: every
`pub`/`pub(crate)` concrete method maps to exactly one trait method or is an
intentionally-private ctor/index-maintenance concern (HNSW
`build_from_db`/`notify_*`/snapshot, `init_schema`, generic config K/V,
`backup_before_migration`, `.pak` file I/O). The `StorageBackend` umbrella +
blanket impl require all six bounded traits; `realization.rs` constructs a real
`Arc<dyn StorageBackend>` and dispatches at least one method per bounded trait
through the object. No method is `todo!()`/`unimplemented!()`/stubbed.

---

## Findings

### [LOW] `schema_version` default-and-parse is duplicated, not delegated

`src/storage/sqlite/schema.rs:39-49` re-implements the `get_config("schema_version")`
→ `"1"` default → `u32` parse inline rather than calling a shared concrete helper.
It is byte-faithful to the logic inside `schema/mod.rs:325`, but the duplication
means a future change to the default/parse in the concrete path would silently NOT
propagate here. Behavior today is correct; flagging the drift risk. Suggest a
`pub(crate) fn schema_version(conn) -> Result<u32>` in `store::schema` that both the
backend and `validate_schema_version` call. Not blocking.

### [LOW] `migrate(c, None)` hard-codes "no backup" — correct now, fragile later

`src/storage/sqlite/schema.rs:35`. The module doc already calls this out and defers
the decision. The current behavior matches the prior open path (no backup taken at
the post-open re-check). Noting it only so the #631 canonical-constructor work
revisits whether `SchemaManager::migrate` should ever carry a backup dir, rather than
silently inheriting `None`. Not blocking; explicitly documented.

---

## Confirmed-faithful summary

- Right concrete method: **all 49 + 12 + 10 + 8 + 7 + 3 + 3 delegations** map to the
  semantically-equivalent concrete method. No similarly-named wrong target.
- Right conn: every read → `block_read`, every write → `block_write`. The three
  read-shaped writes (`insert_or_reinforce_fact`, `stamp_facts_surfaced`,
  `ensure_scope_path`) are correctly on `block_write`.
- Args faithful: borrow→own only; `scope_ids` slices forwarded verbatim; `Option<&[i64]>`
  preserves `None`/`Some(empty)`; `embed_dim = self.embed_dim` everywhere (never 0).
- Returns faithful: `FtsResult.score` (f64) pass-through; `VectorResult.score` (f32)
  widened with `f64::from`; tuples/maps/sets unchanged.
- No trait method missing or stubbed.
