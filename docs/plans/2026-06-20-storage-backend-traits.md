# Implementation Plan — #629 (A1): `StorageBackend` trait family + cross-cutting types

- **Issue:** #629 — `feat(storage): define StorageBackend trait family + FactFilter/capabilities/errors`
- **Epic:** #628 — pluggable `StorageBackend` (SQLite + Postgres). A1 is the **foundation leaf**; #630 (`SqliteBackend`), #631 (engine wiring), #632 (conformance suite), #633–#640 all inherit this contract.
- **Labels:** `type:feature`, `area:storage`
- **Worktree:** `/home/mroynard/dev/memory-engine/.worktrees/feat-629-storage-traits` (exists; branch `feat-629-storage-traits`)
- **Scope:** Traits + cross-cutting types **ONLY**. No backend impls, no engine wiring, no conformance suite. `src/search/hybrid.rs` (RRF) untouched.
- **Spec:** `docs/superpowers/specs/2026-06-19-pluggable-storage-backend-design.md` §Section 1, §Section 3.
- **Engagement / severity:** Thorough / Default (production public-API foundation; user: "critical, we cannot get those wrong").

## Synthesis provenance

This plan is the cherry-picked synthesis of three lens drafts (`-drafts/`):

- **architecture-first** → module layout (one file per trait), the `StorageBackend` blanket impl, tight crate-root re-exports, full `FactGraph` surface.
- **risk-first** → spike-first sequencing, the non-vacuous-assertion negative control, the `async-trait`-is-not-a-core-dep correction, `EventFilter` relocation, the MCP typed-arm (no unilateral deferral), `StorageError` living in `src/error.rs`.
- **mvp-first** → scope discipline (the in-lane diff gate), "MVP = scope not surface", consolidated trait-level `# Errors` docs, the `capabilities()`-sync rationale.

Two arbitration **overrides** of the drafts:

1. `validate_schema_version` **stays on** `SchemaManager` (2 of 3 drafts dropped it). It is a distinct read-only operation — the survey shows it checks epoch + version + config-table existence _without writing_ (the read-only open path, a CLAUDE.md Key Design Decision). It is NOT reconstructible engine-side from `schema_version()` alone, and #631's read-only path needs it. Behavior-preservation wins.
2. The **ADR is NOT in this PR**. The drafts wanted to co-land it; but the epic tree has a dedicated sub-issue (#640, `docs(storage): ADR — pluggable storage backend`). One logical concern per issue (CLAUDE.md). #629 references the spec; #640 writes the ADR.

## Frozen decisions (resolved with the user — do NOT re-litigate)

1. **Error model — driver-opaque `StorageError`, reuse the rest.** New `StorageError { Backend(String), CapabilityUnavailable { needed, backend } }` only; wire via `MemoryError::Storage(#[from] StorageError)`; reuse existing `MemoryError::{Migration, Pool, Serialization, NotFound, …}`. No driver type crosses the seam.
2. **Full method surface** from the survey (`-drafts/_research-context.md`), not a skeleton.
3. **Test surface = compile-tests (object-safety) + concrete-type unit tests** (no backend exists yet).

## The seven open sub-decisions — resolved (all three lenses converged)

| #   | Sub-decision                                 | Resolution                                                                                                                                                                                                                               | Why                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| --- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| D1  | `for_each<F>` reshape                        | **Replace the generic `for_each<F>` with an object-safe streaming method `async fn for_each_X(&self, f: &mut (dyn FnMut(X) -> Result<()> + Send)) -> Result<()>`. Keep `list_all_X` ONLY where it exists today.**                        | Generic `F` breaks object-safety, but dropping streaming entirely is WRONG: `for_each` is the O(1)-peak-memory primitive behind the JSON dump (`facts.rs:677`, "100K+ facts") — `list_all -> Vec<T>` materializes the whole corpus in RAM (caught in review; all three lens drafts missed it). `&mut dyn FnMut` is object-safe (trait object, not generic); the `+ Send` bound is required so the `#[async_trait]` boxed future stays `Send`. **Codex review focus.**                                                                                                    |
| D2  | `#[cfg(test)]`-only store methods            | **Include unconditionally on the trait.**                                                                                                                                                                                                | A `cfg(test)` trait method forks the trait's vtable shape between test/release builds — an object-safety + conformance-suite footgun. Cheap reads.                                                                                                                                                                                                                                                                                                                                                                                                                       |
| D3  | `UpcasterRegistry` placement                 | **Backend construction detail; trait exposes raw + upcasted reads, no registry param.**                                                                                                                                                  | The registry is schema-evolution policy, not storage; passing it across the seam re-couples every backend to the upcaster type.                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| D4  | `ColdStorage` supertrait vs separate         | **Separate feature-gated trait; engine holds `Option<Arc<dyn ColdStorage>>`. NOT a `StorageBackend` supertrait bound.**                                                                                                                  | A `#[cfg]` supertrait bound makes the umbrella's _type_ feature-dependent → `Arc<dyn StorageBackend>` differs with/without `archive`.                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| D5  | Which `SchemaManager` fns on the trait       | **`migrate`, `schema_version`, `validate_schema_version`, `capabilities` (sync), PLUS the embedding-fingerprint surface (see below).** OFF: `open_connection*`, `init_schema`, generic `get/set/list_config`, `backup_before_migration`. | The trait owns _running_ lifecycle the engine drives post-open; _opening/constructing_ is backend-specific (rusqlite flags vs deadpool URL) and would leak the driver. **The embedding-fingerprint config IS on the port (Codex MEDIUM): it's live production behavior, not hypothetical — `store::embedding_meta::{load,store,record_if_absent,require_present}` is called by engine open/write/promotion (`engine/mod.rs:400`, `bootstrap.rs:53`, `cognitive.rs:392`, `cycle/apply.rs:91`); omitting it forces #631 to reach through to SQLite or reshape the trait.** |
| D6  | `FactFilter` vs explicit params              | **`FactFilter` is the `SearchIndex` filter only.** `FactGraph` list methods keep explicit params (`min_importance`, `limit`, `exclude_ids`, scope slices).                                                                               | `importance_score >= ?` / ordering / limit are list/scan concerns, never search predicates; folding them in bloats the closed filter (spec non-goal: no general query language).                                                                                                                                                                                                                                                                                                                                                                                         |
| D7  | `capabilities()` sync under `#[async_trait]` | **Keep sync.**                                                                                                                                                                                                                           | Capabilities are fixed at open, not a per-call round-trip; `#[async_trait]` passes sync `fn`s through untouched.                                                                                                                                                                                                                                                                                                                                                                                                                                                         |

**Config-on-the-port watch-item (documented, not deferred silently):** `get/set/list_config` stays backend-private in A1 (all three lenses agree). If #631 reveals the engine must read backend config (e.g. the `embedding_meta`/`embed_dim` keys from #642) _through_ the port, `get_config`/`set_config` get added to `SchemaManager` then — an additive change (more methods, no shape change), not a contract reshape. Recorded in §Risks.

## Module layout (target)

```text
src/storage/                      NEW — the persistence PORT (infrastructure; distinct from src/traits.rs consumer port)
  mod.rs              module doc (the seam narrative) + `pub use` facade of every public item
  filter.rs           FactFilter, TemporalFilter, MetadataPredicate (+ builders + tests)
  capabilities.rs     LexicalRanker, BackendCapabilities (+ tests)
  graph.rs            trait FactGraph       (folds FactStore + EdgeStore + ScopeStore)
  event_log.rs        trait EventLog        (raw + upcasted reads)
  search_index.rs     trait SearchIndex     (lexical_search / vector_search -> Vec<(i64, f64)>; lexical_count_expired)
  consolidation.rs    trait ConsolidationStore (SummaryStore + LineageStore)
  session.rs          trait SessionStore    (ActivityStore + CheckpointStore)
  schema.rs           trait SchemaManager   (migrate / schema_version / validate_schema_version / capabilities)
  cold_storage.rs     trait ColdStorage     (#[cfg(feature = "archive")])
  backend.rs          trait StorageBackend (umbrella) + blanket impl + obj-safety asserts
```

`StorageError` lives in **`src/error.rs`** (NOT `src/storage/`) — alongside its 5 sibling sub-error enums (`ConflictError`, `RerankerError`, `ArchiveError`, `MigrationError`, `CycleError`), keeping the `#[non_exhaustive]` taxonomy + `#[from]` wiring + Display tests in one file. It reaches the crate root via the existing `pub use error::*;` (lib.rs:111).

**Type relocation (the one justified edit to non-error existing files).** The port must not reference types that physically live inside the SQLite `store` module (wrong dependency direction). Relocate to `src/types.rs` (the leaf, re-exported via `pub use types::*;`), leaving `pub use` shims in the original modules for back-compat:

- `EventFilter` (`store/events.rs:10`) → `types.rs`; shim `pub use crate::types::EventFilter;` in `store/events.rs`.
- `FactScoringRow` (`store/facts.rs:42`), `SessionFact` (`store/facts.rs:1154`) → `types.rs`; shims in `store/facts.rs`.
- **Fallback:** if any of these carries store-coupled inherent/trait impls that resist a clean move, keep the def in `store` and reference it from the trait via its `store` path, documenting why in the PR. Decided at implementation; the 1178 existing tests staying green proves zero behavior change either way.

## Crate-root re-exports (`src/lib.rs`)

`pub mod storage;` in the public-modules block (lines 82–89). Tight flat re-export — only the umbrella + consumer-relevant cross-cutting types (mirroring the existing `pub use traits::{…}` block at lib.rs:117):

```rust
pub use storage::{
    BackendCapabilities, FactFilter, LexicalRanker, MetadataPredicate,
    StorageBackend, TemporalFilter,
};
```

The seven bounded traits are reachable as `memory_engine::storage::FactGraph` but **not** flat-re-exported — they are the internal port surface backends implement and tests mock, not types a typical consumer names. `StorageError` flat-exports automatically via `pub use error::*;`.

---

## The contract (file-by-file)

Signatures are transcribed 1:1 from `-drafts/_research-context.md` "REAL METHOD SURFACE", with uniform transforms: every method `async fn` except `SchemaManager::capabilities`; `&self`; return `crate::Result<T>`; entity-suffixed names to avoid collisions when folding stores (`insert_fact`/`insert_edge`/`insert_scope`); generic `for_each<F>` → object-safe `for_each_X(&mut (dyn FnMut(X) -> Result<()> + Send))` preserving streaming, `list_all_X` kept only where it exists today (D1); `pub(crate)` originals become public trait methods (the trait itself is the access boundary); `#[cfg(test)]` originals unconditional (D2).

### `src/error.rs` — `StorageError` + `MemoryError::Storage`

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// An opaque backend-driver failure with no more-specific MemoryError home.
    /// rusqlite::Error / tokio_postgres::Error are mapped to a String INSIDE the
    /// backend so no driver type crosses the seam (continues the #560 arc).
    #[error("{0}")]
    Backend(String),
    /// A required capability is unavailable on the open backend — the
    /// LexicalMode::RequireBm25 fail-fast (spec §5). Surfaced at open, opt-in.
    #[error("backend `{backend}` lacks required capability `{needed}`")]
    CapabilityUnavailable { needed: &'static str, backend: &'static str },
}
```

Add to `MemoryError` (near the other `#[from]` sub-enum variants):

```rust
    /// A failure at the pluggable-storage seam; see [`StorageError`].
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
```

### `src/storage/filter.rs`

`FactFilter { fact_type: Option<FactType>, scope_ids: Option<Vec<i64>>, ids: Option<Vec<i64>>, temporal: TemporalFilter, pinned: Option<bool>, metadata: Vec<MetadataPredicate> }` — `derive(Debug, Clone, Default, PartialEq)` + a chainable `#[must_use]` owned-`self` builder (`new`, `fact_type`, `scope_ids`, `ids`, `temporal`, `pinned`, `with_metadata`).
`TemporalFilter { #[default] Active, AsOf(DateTime<Utc>), ValidDue(DateTime<Utc>), IncludeExpired }` — `derive(Debug, Clone, Copy, PartialEq, Eq, Default)`; per-variant doc carries the grounding SQL shape.
`MetadataPredicate { KeyAbsent(String), KeyPresent(String), KeyEquals(String, serde_json::Value) }` — `derive(Debug, Clone, PartialEq)`.

### `src/storage/capabilities.rs`

`LexicalRanker { Bm25, TsRankCd }` — `derive(Debug, Clone, Copy, PartialEq, Eq, Hash)`.
`BackendCapabilities { lexical_ranker: LexicalRanker, server_side_vector: bool, true_idf: bool }` — same derives. No methods (the engine decides what "degraded" means; the port only reports facts).

### `src/storage/graph.rs` — `FactGraph` (folds FactStore + EdgeStore + ScopeStore)

Full surveyed surface, entity-suffixed. Facts (write): `insert_fact`, `insert_or_reinforce_fact`, `expire_fact`, `set_fact_pinned`, `update_fact_importance`, `update_fact_importance_score`, `increment_fact_access`, `merge_fact_metadata`, `mark_facts_dream_cycled`, `stamp_facts_surfaced`, `hard_delete_facts`. Facts (read): `get_fact`, `get_facts`, `list_all_facts`, `max_caller_written_fact_id`, `for_each_fact` (streaming, object-safe), `list_active_facts`, `list_active_facts_scoring`, `list_active_facts_at`, `list_dormant_facts`, `list_facts_by_scope_importance`, `list_facts_by_scopes_importance`, `list_facts_by_importance_score`, `list_pinned_facts`, `list_due_facts`, `next_due_time`, `list_facts_by_scopes_recent`, `list_active_facts_by_metadata_key_recent`, `list_active_facts_in_period`, `list_undreamt_facts_in_period`, `list_active_facts_by_session`. Edges: `insert_edge`, `get_edge`, `expire_edge`, `expire_edges_by_fact`, `list_all_edges`, `for_each_edge` (streaming), `list_active_edges`, `list_active_edges_by_source`, `list_active_edges_by_target`, `edge_exists_active`, `list_active_edge_pairs_by_facts`, `hard_delete_edges_by_facts`. Scopes: `get_scope`, `find_scope_by_label`, `insert_scope`, `ensure_scope_path`, `list_all_scopes`, `for_each_scope` (streaming). (Exact signatures = the research-context surface, verbatim. The implementer transcribes signature-by-signature.)

### `src/storage/event_log.rs` — `EventLog`

`insert_event`, `get_event`, `list_events(&EventFilter)`, `count_events(&EventFilter)`, `for_each_event` (streaming, object-safe — replaces `for_each`; EventStore has no `list_all` today), `get_upcasted_event`, `list_upcasted_events`. No registry param (D3).

### `src/storage/search_index.rs` — `SearchIndex`

```rust
// Return SCORED ranked pairs (id, score), best-first (user decision, Codex BLOCKER — overrides the
// originally-locked Vec<i64>). f64 matches SearchResult.score (hybrid.rs:50) and the engine's existing
// single-channel Vec<(i64, f64)>. RRF (hybrid.rs) still fuses by RANK only — it ignores the score — so the
// cross-backend bm25-vs-ts_rank incommensurability stays invisible to fusion (the lock's real intent);
// single-channel FTS/Vector modes keep the user-visible score (CLI query.rs:113, MCP depth.rs:96).
async fn lexical_search(&self, query: &str, filter: &FactFilter, k: usize) -> Result<Vec<(i64, f64)>>;
async fn vector_search(&self, embedding: &[f32], filter: &FactFilter, k: usize) -> Result<Vec<(i64, f64)>>;
/// Count facts matching the lexical query that are EXPIRED (t_expired IS NOT NULL) — the
/// `diagnostics.expired_matches` probe (transcribes `fts_count_expired`, fts.rs:95). filter
/// supplies fact_type/scope_ids; the expired predicate is baked in (filter.temporal ignored).
async fn lexical_count_expired(&self, query: &str, filter: &FactFilter) -> Result<usize>;
```

Doc states: `lexical_search`/`vector_search` return ranked `Vec<(i64, f64)>` (best-first; `f64` score matches `SearchResult.score`); **RRF (`hybrid.rs`) fuses by RANK only — it ignores the score — so the cross-backend bm25-vs-ts_rank gap stays invisible to fusion (the lock's intent); single-channel FTS/Vector modes keep the user-visible score**; scores are **backend-native, not cross-comparable** (doc caveat — use rank for cross-backend reasoning; piece-D measures the gap); query parsing is backend-owned; malformed query → empty result, not error (mirrors today's FTS5-syntax swallow). Brute-force-vs-HNSW + `ann.rs` `#[cfg(feature="ann")]` are impl-internal, never a trait concern. **`lexical_count_expired` added from the survey (BLOCKER caught in review): the engine calls `fts_count_expired` at `query.rs:302` to populate `diagnostics.expired_matches`; `lexical_search`'s `limit`+`Vec` cannot count all expired matches, so #631 wiring needs this distinct method.** (The HNSW index-maintenance hooks `notify_insert`/`notify_expire` and `build_from_db`/snapshot methods stay impl-private — they are SQLite-ann internals the backend runs on fact mutation, not a port contract.)

### `src/storage/consolidation.rs` — `ConsolidationStore` (SummaryStore + LineageStore)

Summaries: `insert_summary`, `get_summary`, `list_summaries_by_level`, `list_all_summaries` (was `for_each`), `delete_summaries_by_level`. Lineage: `insert_lineage`, `insert_lineage_raw`, `get_lineage_by_wisdom_fact`, `get_lineage_source_fact_ids`, `delete_lineage`, `has_lineage`, `for_each_lineage` (streaming, object-safe — replaces `for_each`; LineageStore has no `list_all` today). Summaries also gain `for_each_summary` (streaming) alongside `list_all_summaries`.

### `src/storage/session.rs` — `SessionStore` (ActivityStore + CheckpointStore)

Activities: `insert_or_dedup_activity`, `get_activity`, `list_activities_by_session`, `list_recent_activities_by_scope`, `update_activity_status`, `count_activities_by_session`. Checkpoints: `upsert_checkpoint`, `get_checkpoint`, `get_checkpoint_by_scope`, `list_recent_checkpoints`. (`#[cfg(test)]` originals unconditional — D2.) _Codex flagged the ex-`#[cfg(test)]` reads (`get_activity`, `count_activities_by_session`, `list_activities_by_session`, `get_checkpoint`, `list_recent_checkpoints`) as the main trim candidate. **Kept, with justification:** the #632 cross-backend conformance suite tests backends *through* the trait and needs these direct read-backs to assert writes; and a `#[cfg(test)]` trait method forks the vtable shape between builds (D2). The cost is a handful of methods the engine never calls in release — acceptable for a contract that #632 exercises._

### `src/storage/schema.rs` — `SchemaManager`

```rust
async fn migrate(&self) -> Result<()>;
async fn schema_version(&self) -> Result<u32>;
async fn validate_schema_version(&self) -> Result<()>;   // read-only compat path (override: kept)
fn capabilities(&self) -> BackendCapabilities;           // sync (D7)
// embedding-fingerprint identity (Codex MEDIUM — live, not hypothetical; transcribes store::embedding_meta)
async fn load_embedding_fingerprint(&self) -> Result<Option<EmbeddingFingerprint>>;
async fn store_embedding_fingerprint(&self, fp: &EmbeddingFingerprint) -> Result<()>;
async fn record_embedding_fingerprint_if_absent(&self, candidate: &EmbeddingFingerprint, expected_dim: usize) -> Result<EmbeddingFingerprint>;
async fn require_embedding_fingerprint_present(&self) -> Result<()>;
```

`EmbeddingFingerprint` already lives in `crate::types`/`crate::traits` (#626/#642) — no relocation. The fingerprint _comparison/identity_ logic stays engine-side and backend-agnostic (CLAUDE.md / #626); only the **persistence** of the fingerprint (load/store/record/require over the config table) crosses the port. The generic `get/set/list_config` stay backend-private — only this typed identity surface is promoted, because it is what the engine actually calls cross-cuttingly.

### `src/storage/cold_storage.rs` — `ColdStorage` (`#[cfg(feature = "archive")]`)

Separate trait (D4). Manifest CRUD only: `insert_archive_manifest` (10-arg, `#[allow(clippy::too_many_arguments)]` — mirrors the verbatim manifest row; file a collateral `type:refactor` for a `NewArchiveManifest` struct), `list_archive_manifest`, `delete_archive_manifest`. `.pak` file I/O (`write_pak_and_hash`/`read_pak`/`hash_file`/`verify_pak`) stays SQLite-private free fns.

### `src/storage/backend.rs` — `StorageBackend` umbrella + blanket impl

```rust
pub trait StorageBackend:
    FactGraph + EventLog + SearchIndex + ConsolidationStore + SessionStore + SchemaManager {}

impl<T> StorageBackend for T
where T: FactGraph + EventLog + SearchIndex + ConsolidationStore + SessionStore + SchemaManager {}
```

The blanket impl is the keystone: backends never write `impl StorageBackend` — implementing the six parts _is_ being a `StorageBackend`. `ColdStorage` is deliberately not a bound (D4).

### `Cargo.toml`

`async-trait = "0.1"` in `[dependencies]` (confirmed absent today; pinned to the major already transitively in the lockfile). The only dependency #629 adds.

---

## TDD execution order (risk-retiring sequence; each phase ends green)

The "test" for a trait-definitions PR is overwhelmingly the **compiler**: `fn _assert_obj_safe(_: &dyn Trait) {}` fails to compile until the trait exists and is object-safe — that red→green is the TDD loop. Behavioral unit tests attach to the cross-cutting types (the only things with behavior). **Object-safety asserts use the free-function `&dyn Trait` form for vtable formation; ADDITIONALLY (Codex review BLOCKER) one `#[tokio::test]` actually `.await`s a method through `&dyn`/`Arc<dyn>` to prove async methods are _callable_ through the object — vtable-forms ≠ callable under `#[async_trait]`'s hidden `Self: Sync` future bound. Every trait carries `: Send + Sync` (required for the boxed `Future: Send`). The awaited-call test is gated `#[cfg(feature = "async")]` and runs in the `--all-features` gate, so default builds need no tokio.**

**P1 — Object-safety + dependency spike (THE load-bearing step).** Retire the epic-sinking risk first, on a 2-method skeleton, before transcribing ~90 methods.

1. Add `async-trait = "0.1"`; `cargo build` (resolves + locks).
2. `src/storage/{mod,backend}.rs` with a 2-method skeleton (`FactGraph::get_fact` + `SchemaManager::{schema_version (async), capabilities (sync)}`) + the umbrella + blanket impl + a stub `BackendCapabilities`.
3. Object-safety asserts: `fn _assert_obj_safe(_: &dyn StorageBackend) {}`, `fn _arc(_: Arc<dyn StorageBackend>) {}`, per-leaf `&dyn` asserts. **PLUS a callability proof (Codex BLOCKER):** a tiny `Dummy` impl of the skeleton + a `#[cfg(feature="async")] #[tokio::test]` that does `let b: Arc<dyn StorageBackend> = Arc::new(Dummy); b.schema_version().await?;` — proving an async method is actually invokable+awaitable through the trait object, not merely that the vtable forms.
4. `pub mod storage;` in lib.rs.
5. **Gate:** `cargo build && cargo build --all-features && cargo test --lib storage:: && cargo test --all-features --lib storage::`. The compiling `Arc<dyn StorageBackend>` coercion proves object-safety; the awaited call under `--all-features` proves callability.
6. **Non-vacuous control (do once, do NOT commit):** temporarily add `async fn _g<F: FnMut()>(&self);` to a skeleton trait, confirm `cargo build` now ERRORS on the `&dyn` assert (proving the assert actually guards object-safety), then delete it. The highest-signal 2-minute check in the plan. **If P1 fails, STOP and fix the spike — do not widen the surface.**

**P2 — `StorageError` + `MemoryError::Storage` + MCP arm (no driver leak, no silent degradation).**

1. RED: write the `error.rs` tests first (Display both variants; `From<StorageError> for MemoryError`; `"storage error: "` prefix) — mirror the `ArchiveError`/`MigrationError` test blocks.
2. GREEN: add `StorageError` (sibling in `error.rs`) + `MemoryError::Storage(#[from] …)`.
3. **MCP typed arm (explicit mapping):** add `MemoryError::Storage(err) => ErrorData::internal_error(err.to_string(), None),` **above** the `other =>` arm at `memory-engine-mcp/src/error.rs:69`, + a `storage_maps_to_internal` test. _Note (agy review correction):_ the existing wildcard already produces the correct string (`MemoryError::Storage`'s `Display` prepends `"storage error: "`, and the wildcard does `other.to_string()`), so this is NOT a degradation fix — it is an **explicit, greppable, future-proof** typed arm that protects against later wildcard changes and documents the mapping intentionally. Use `err.to_string()` (not a re-prefixed `format!`) to avoid any double-prefix.
4. **Gate:** `cargo test --lib error:: && cargo test -p memory-engine-mcp && cargo build --workspace`.

**P3 — Cross-cutting value types (behavior → real unit tests).**

1. `capabilities.rs`: RED tests (construct both tiers, `Copy`/`Eq`); GREEN defs (replace P1 stub).
2. `filter.rs`: RED tests (`default()` is empty+`Active`; builder chain populates every field; `TemporalFilter` default + variant construction; `MetadataPredicate` incl. `KeyEquals` with a `serde_json::Value`); GREEN defs + builder.
3. **Gate:** `cargo test --lib storage:: && cargo build --all-features`.

**P4 — Type relocation (EventFilter, FactScoringRow, SessionFact → types.rs).**

1. Move defs to `types.rs`; add `pub use` shims in `store/events.rs` + `store/facts.rs`.
2. **Gate:** `cargo test --workspace` (the 1178 baseline must stay green — proves the relocation is behavior-neutral). Fallback to in-place + `store`-path reference if a type resists a clean move.

**P5 — Full bounded-trait surface (retire R-COMPLETE).** With dyn-safety proven, widen each trait to the full surveyed surface (mechanical, signature-by-signature, applying the uniform transforms). Extend the obj-safety asserts to all seven bounded traits + `ColdStorage` (under `#[cfg(feature="archive")]`). Add a **coverage checklist** comment at the top of `backend.rs` mapping every surveyed concrete method → its trait method (or "streaming → for_each_X" / "stays backend-private: <reason>") — the grep-able R-COMPLETE proof reviewed in super-review. The streaming methods (`for_each_X`) preserve the O(1)-peak-memory dump/export property (`facts.rs:677`); a doc note on each cites the streaming contract.
**Gate:** `cargo build && cargo build --all-features && cargo test --lib storage::`.

**P6 — Crate-root re-exports + smoke test.** Add the tight `pub use storage::{…}` block; extend lib.rs `reexports_are_accessible` with `fn _accepts_storage_backend(_: &dyn crate::StorageBackend) {}` + `size_of` probes for `FactFilter`/`BackendCapabilities`/`StorageError`/`TemporalFilter`/`LexicalRanker`/`MetadataPredicate`.

**P7 — Full workspace gate (§Verification).**

---

## Documentation

**Not N/A — scoped to the port; no new narrative/Sphinx page.**

1. **Rustdoc** on every trait/method/type, matching `src/traits.rs` density: module-level `//!` on `storage/mod.rs` (the port-vs-`traits.rs` distinction; the seven bounded traits + umbrella; closed `FactFilter`; driver-opaque `StorageError`; the "timestamps cross as `DateTime<Utc>`, lexicographic-TEXT is now SQLite-private" invariant relocation). Per-trait doc names the concrete store(s) it abstracts. `SearchIndex` "ranked-not-scored / query-parsing-backend-owned" and `EventLog` upcasting notes are load-bearing for #634/#635 — explicit.
2. **`# Errors` docs:** `missing_errors_doc` is _pedantic = warn_ (not deny, confirmed). Use **one consolidated `# Errors` paragraph per trait** ("all methods return `MemoryError::Storage` wrapping a `StorageError` on backend failure, or a more specific `MemoryError` variant"); add per-method stanzas only if clippy still warns. Avoids ~90 copy-pasted stanzas.
3. **`docs/reference/crate-layout.md`** — add the `src/storage/` module row (persistence port).
4. **No ADR in this PR** (it is #640). **No `CLAUDE.md` Status edit** (moves with the epic, not per sub-issue).

## Testing

**Not N/A — the core deliverable.** Two tiers (frozen decision #3):

- **Compile-tests (load-bearing):** per-leaf `fn _assert_obj_safe(_: &dyn Trait)` (×6 + `ColdStorage` under `archive`); capstone `&dyn StorageBackend` + `Arc<dyn StorageBackend>` coercion (the contract #631 depends on); `SchemaManager` sync-method coexistence; the P1 negative control (run once, proves non-vacuous); crate-root reachability via `reexports_are_accessible`.
- **Concrete-type unit tests (the only behavior present):** `StorageError` Display + `From` + prefix; `FactFilter` default + builder chain + `PartialEq`; `TemporalFilter` default + variants; `MetadataPredicate` incl. `KeyEquals`; `BackendCapabilities`/`LexicalRanker` both tiers + `Copy`.
- **Feature matrix:** tests pass under default, `--features archive` (exercises `ColdStorage` + its assert), and `--all-features`.
- **Explicitly NOT here (deferred, stated so the absence is a decision):** behavioral trait tests ("expire then query Active excludes it") → #632 conformance; SQL/translation tests (no SQL in this PR); async-runtime tests (nothing `.await`s; `async` feature not required to build/test A1).

## Verification (the gate — touches `error.rs` + public API ⇒ full workspace gate per CLAUDE.md)

```bash
cd /home/mroynard/dev/memory-engine/.worktrees/feat-629-storage-traits
cargo fmt --check                                   # edition-aware; trust cargo fmt, not bare rustfmt (worktree gotcha)
cargo build  --workspace
cargo build  --workspace --all-features             # archive/ann/async gate ColdStorage + cfg'd asserts
cargo test   --workspace                            # re-capture baseline AFTER rebase (was 1178); this PR ADDS tests, removes none
cargo test   --workspace --all-features
cargo clippy --workspace --all-targets
cargo clippy --workspace --all-targets --all-features
cargo doc --no-deps                                 # intra-doc links in the new module resolve
# In-lane scope check (mvp discipline): diff must touch only the expected paths
git --no-pager diff --stat main -- . ':(exclude)docs/plans'
# Expect ONLY: Cargo.toml, Cargo.lock, src/error.rs, src/lib.rs, src/types.rs, src/storage/**,
#   src/store/events.rs + src/store/facts.rs (relocation SHIMS only), memory-engine-mcp/src/error.rs
```

**Green criteria:** all four crates build/test/clippy clean under default AND `--all-features`; `fmt --check` clean; test count strictly > the rebased baseline (was 1178; additions only, zero deletions); the relocation leaves the baseline green; the in-lane diff shows no `src/engine/**`, `src/search/**`, or non-shim `src/store/**` changes. Clippy watch-items: `too_many_arguments` (`ColdStorage::insert_archive_manifest` — `#[allow]` + justification), `module_name_repetitions` (`storage::StorageBackend`/`StorageError` — the deliberate public idiom; scoped `#[allow]` if it fires), `missing_errors_doc` (trait-level paragraph).

## Operational git / PR workflow

1. **Plan issue:** publish this plan as a `type:plan` + `area:storage` issue, title `plan(storage): #629 StorageBackend trait family + cross-cutting types`; link under epic #628 via `addSubIssue` (`replaceParent: true`). Or post to #629's thread — maintainer's call.
2. **Branch:** already on `feat-629-storage-traits`. Rebase on latest `main` first (#641/#642 recently moved `error.rs`/schema).
3. **Atomic commits** (Conventional Commits, WHY in body, no co-author), per phase:
   - `chore(storage): add async-trait dependency`
   - `feat(storage): add driver-opaque StorageError + MemoryError::Storage; map in MCP`
   - `feat(storage): add FactFilter/TemporalFilter/MetadataPredicate + BackendCapabilities`
   - `refactor(storage): relocate EventFilter/FactScoringRow/SessionFact to types for the dialect-free port`
   - `feat(storage): define bounded-context storage traits + StorageBackend umbrella`
   - `feat(storage): crate-root re-exports + obj-safety gate`
   - **Auto-close guard (memory):** keep `close/fix/resolve #628` (the epic) and bare `Closes #629` out of commit messages/PR body — use `Part of #628`, `Implements #629`. Let the squash-merge close #629 deliberately; #628 must stay open.
4. **Run the full gate (§Verification)**; paste tails into the PR.
5. **Open PR** → base `main`, title `feat(storage): define StorageBackend trait family + FactFilter/capabilities/errors`. Body: spec + plan links, ships/defers list, gate output, in-lane diff proof, "A1 of #628; blocks #630/#631/#632; behavior-neutral additive port".
6. **`/super-review`.** Reviewer focus: (a) `Arc<dyn StorageBackend>` coerces, (b) no surveyed method missing (coverage checklist), (c) zero `impl Trait for` backend / zero `rusqlite` under `src/storage/`, (d) gemini-code-assist[bot] auto-review threads (expect type-blind FPs on the `async_trait` expansion — triage, don't auto-apply).
7. **Address review; re-run the gate** (CLEAN ≠ semantically safe — re-gate after any change/rebase).
8. **Rebase on `main` if moved, re-gate, squash-merge.** Squash subject = `feat(storage): define StorageBackend trait family + FactFilter/capabilities/errors (#629)`.
9. **Post-merge:** verify #629 closed, #630/#631/#632 unblocked on board #6.

## Risks & mitigations

| Risk                                                                                | Mitigation                                                                                                                                                                                                                                         |
| ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Object safety fails** (generic method / `Self`-by-value / AFIT) → whole epic dead | P1 spike proves `Arc<dyn StorageBackend>` coerces on a skeleton BEFORE the full surface; non-vacuous negative control; `_assert_obj_safe` gates `cargo build` forever after.                                                                       |
| **A surveyed method missed** → mid-epic trait reshape                               | Full surface transcribed 1:1 from the research context; P5 coverage checklist is the grep-able proof, reviewed in super-review.                                                                                                                    |
| **New `MemoryError` variant breaks workspace**                                      | MCP match has a wildcard arm (compiles); typed arm added in P2 (no silent degradation); full-workspace gate confirms cli/mcp/embed.                                                                                                                |
| **Driver leak past the seam**                                                       | `StorageError` carries only `String`/`&'static str`; trait methods return `crate::Result`; `Database(#[from] rusqlite::Error)` stays SQLite-internal only.                                                                                         |
| **Feature-gate divergence** (`archive`/`ann`/`async`)                               | `ColdStorage` separate (not supertrait) keeps the umbrella type feature-invariant; gate runs default AND `--all-features`.                                                                                                                         |
| **Type relocation ripples**                                                         | `pub use` back-compat shims keep call sites resolving; 1178 baseline green proves behavior-neutral; in-place fallback documented.                                                                                                                  |
| **Config-on-the-port (embedding fingerprint)** — live engine behavior               | RESOLVED (Codex MEDIUM): the typed embedding-fingerprint surface (`load/store/record_if_absent/require_present`) is now on `SchemaManager` (D5), so #631 wires through the port, not a SQLite reach-through. Generic config stays backend-private. |
| **Scope creep into #630** (writing a backend "to test")                             | Hard rule: zero `impl Trait for` backend; obj-safety proven by `&dyn` reference types alone; in-lane diff-stat gate enforces it.                                                                                                                   |

## Definition of Done

- [ ] `async-trait` in `[dependencies]`; `Cargo.lock` updated.
- [ ] `src/storage/{mod,filter,capabilities,graph,event_log,search_index,consolidation,session,schema,backend}.rs` + `cold_storage.rs` (cfg archive) exist; `StorageError` in `src/error.rs`.
- [ ] Every method in the REAL METHOD SURFACE maps to exactly one trait method (coverage checklist audited).
- [ ] `MemoryError::Storage(#[from] StorageError)` wired; MCP typed arm + test; `"storage error: "` prefix tested.
- [ ] All six leaf `_assert_obj_safe` + `ColdStorage` (cfg) + `StorageBackend` umbrella + `Arc<dyn StorageBackend>` coercion compile; blanket impl in place.
- [ ] Concrete-type unit tests pass (StorageError, FactFilter builder, TemporalFilter, MetadataPredicate, BackendCapabilities).
- [ ] EventFilter/FactScoringRow/SessionFact relocated to `types.rs` with shims; 1178 baseline green.
- [ ] Tight crate-root re-exports; `reexports_are_accessible` extended + passing.
- [ ] `crate-layout.md` lists `src/storage/`.
- [ ] Full workspace gate green: build/test/clippy `--workspace --all-targets`, `--all-features`, `fmt --check`, `doc --no-deps`. Test count > 1178, zero deletions. In-lane diff.
- [ ] PR reviewed (`/super-review`), re-gated post-rebase, squash-merged with the A1 title + `(#629)`.
- [ ] Zero backend impl, zero engine wiring, zero conformance suite; ADR left to #640.

```

```
