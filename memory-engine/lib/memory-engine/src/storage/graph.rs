//! The core knowledge-graph trait: facts, edges, and the scope hierarchy.
//!
//! Folds the surveyed `FactStore` + `EdgeStore` + `ScopeStore` surface. Scopes
//! fold in because `scope_id` is an FK on facts/edges (they partition the graph,
//! not an independent concern). Method names are **entity-suffixed**
//! (`insert_fact`/`insert_edge`/`insert_scope`) because folding three stores into
//! one trait collides the bare `insert`/`get`/`list_active` names. The
//! `pub(crate)` originals (`merge_metadata`, `mark_dream_cycled`,
//! `list_undreamt_in_period`) become public trait methods — the trait *is* the
//! crate-internal access boundary.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::types::{
    Edge, Fact, FactScoringRow, FactType, NewEdge, NewFact, ScopeNode, SessionFact,
};

/// Core knowledge graph: facts, edges, the scope hierarchy.
///
/// All methods are async (boxed via `async_trait`): the `SQLite` backend wraps
/// sync `rusqlite` in `spawn_blocking`; a Postgres backend is natively async. No
/// SQL or driver type crosses this boundary — filtering is expressed via explicit
/// params / [`crate::storage::FactFilter`], results are domain types from
/// [`crate::types`].
///
/// # Scope filtering (`scope_ids` — read the contract, it is non-uniform)
/// The `scope_ids: &[i64]` parameter has a **method-dependent empty-slice
/// meaning** inherited from the concrete store surface, and a backend MUST
/// preserve it exactly (the #632 conformance suite pins it):
/// - **Empty slice = ALL scopes** (filter disabled): [`list_pinned_facts`](Self::list_pinned_facts),
///   [`list_due_facts`](Self::list_due_facts), [`next_due_time`](Self::next_due_time),
///   [`list_facts_by_importance_score`](Self::list_facts_by_importance_score),
///   [`list_active_facts_by_session`](Self::list_active_facts_by_session),
///   [`list_active_facts_in_period`](Self::list_active_facts_in_period),
///   [`list_undreamt_facts_in_period`](Self::list_undreamt_facts_in_period).
/// - **Empty slice = NO scopes** (empty result): [`list_facts_by_scopes_importance`](Self::list_facts_by_scopes_importance),
///   [`list_facts_by_scopes_recent`](Self::list_facts_by_scopes_recent),
///   [`list_active_facts_by_metadata_key_recent`](Self::list_active_facts_by_metadata_key_recent).
///
/// [`list_dormant_facts`](Self::list_dormant_facts) makes the choice explicit in
/// the type (`Option<&[i64]>`: `None` = all scopes). A future API-hardening pass
/// may unify this (e.g. a `ScopeSelector` type) — tracked separately; A1 mirrors
/// the existing contract verbatim.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](crate::error::MemoryError::Storage)
/// wrapping a [`StorageError`](crate::error::StorageError) on a backend failure,
/// or a more specific `MemoryError` variant where applicable (e.g.
/// [`NotFound`](crate::error::MemoryError::NotFound) for a missing id).
#[async_trait]
pub trait FactGraph: Send + Sync {
    // --- facts: write ---
    async fn insert_fact(&self, fact: &NewFact) -> Result<i64>;
    async fn insert_or_reinforce_fact(&self, fact: &NewFact) -> Result<(i64, bool)>;
    async fn expire_fact(&self, id: i64, now: DateTime<Utc>) -> Result<()>;
    /// Bi-temporally expire **and** invalidate a fact (set both `t_expired` and
    /// `t_invalid`) — conflict resolution's `Update`/`Delete` semantics. Fires the
    /// post-commit HNSW `notify_expire`, like [`expire_fact`](Self::expire_fact).
    async fn expire_and_invalidate_fact(&self, id: i64, now: DateTime<Utc>) -> Result<()>;
    async fn set_fact_pinned(&self, id: i64, pinned: bool) -> Result<()>;
    async fn update_fact_base_importance(&self, id: i64, base_importance: f64) -> Result<()>;
    async fn update_fact_importance_score(&self, id: i64, score: f64) -> Result<()>;
    async fn increment_fact_access(&self, id: i64, now: DateTime<Utc>) -> Result<()>;
    async fn merge_fact_metadata(&self, id: i64, patch: &serde_json::Value) -> Result<()>;
    async fn mark_facts_dream_cycled(
        &self,
        ids: &[i64],
        cycle_id: u64,
        now: DateTime<Utc>,
    ) -> Result<()>;
    async fn stamp_facts_surfaced(
        &self,
        fact_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Result<Vec<(i64, DateTime<Utc>)>>;
    async fn hard_delete_facts(&self, ids: &[i64]) -> Result<usize>;

    // --- facts: read (single / batch / full scan) ---
    async fn get_fact(&self, id: i64) -> Result<Fact>;
    async fn get_facts(&self, ids: &[i64]) -> Result<HashMap<i64, Fact>>;
    async fn list_all_facts(&self) -> Result<Vec<Fact>>;
    /// Stream every fact (including expired) to `f`, one row at a time — the
    /// O(1)-peak-memory primitive behind the JSON dump/export path. The callback
    /// is `Send` so the `#[async_trait]` boxed future stays `Send`.
    async fn for_each_fact(&self, f: &mut (dyn FnMut(Fact) -> Result<()> + Send)) -> Result<()>;
    async fn max_caller_written_fact_id(&self) -> Result<Option<i64>>;

    // --- facts: read (filtered / scored lists — explicit params, NOT FactFilter) ---
    async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>>;
    async fn list_active_facts_scoring(&self) -> Result<Vec<FactScoringRow>>;
    async fn list_active_facts_at(&self, valid_at: DateTime<Utc>) -> Result<Vec<Fact>>;
    /// List dormant facts (active, non-pinned, `importance_score < threshold`,
    /// temporally valid **as of `as_of`**). `as_of` is the injected wall-clock
    /// instant — the facade passes [`Utc::now`](chrono::Utc::now) — so backends
    /// never read the clock themselves and temporal behavior is deterministically
    /// testable (#327). `scope_ids` `None` = all scopes, `Some` = those scopes.
    async fn list_dormant_facts(
        &self,
        importance_threshold: f64,
        scope_ids: Option<&[i64]>,
        as_of: DateTime<Utc>,
    ) -> Result<Vec<Fact>>;
    async fn list_facts_by_scope_importance(
        &self,
        scope_id: i64,
        limit: usize,
    ) -> Result<Vec<Fact>>;
    async fn list_facts_by_scopes_importance(
        &self,
        scope_ids: &[i64],
        min_importance: f64,
        limit: usize,
        exclude_ids: &HashSet<i64>,
    ) -> Result<Vec<Fact>>;
    async fn list_facts_by_importance_score(
        &self,
        scope_ids: &[i64],
        min_score: f64,
        limit: usize,
        exclude: &HashSet<i64>,
    ) -> Result<Vec<Fact>>;
    /// List active pinned facts, ordered by `importance_score` DESC, capped at
    /// `limit` (pushed to the backend so embedding BLOBs past the cap are never
    /// materialized — #395). `None` retrieves all pinned facts — consistent with
    /// [`list_due_facts`](Self::list_due_facts) and
    /// [`list_active_facts`](Self::list_active_facts). `scope_ids` empty = **all
    /// scopes** (see the trait-level contract).
    async fn list_pinned_facts(&self, scope_ids: &[i64], limit: Option<usize>)
    -> Result<Vec<Fact>>;
    /// List active facts "due now" (`t_valid <= now`, not bi-temporally
    /// invalidated), ordered by `t_valid` ASC. `exclude` is an id set removed in
    /// the backend (empty = no exclusion); `limit` caps the result in the backend
    /// (`None` = uncapped, the scheduling contract). Pushing both down (#396)
    /// avoids materializing and decoding embedding BLOBs the caller would discard.
    /// `scope_ids` empty = **all scopes** (see the trait-level contract).
    async fn list_due_facts(
        &self,
        now: DateTime<Utc>,
        scope_ids: &[i64],
        exclude: &[i64],
        limit: Option<usize>,
    ) -> Result<Vec<Fact>>;
    async fn next_due_time(
        &self,
        now: DateTime<Utc>,
        scope_ids: &[i64],
    ) -> Result<Option<DateTime<Utc>>>;
    async fn list_facts_by_scopes_recent(
        &self,
        scope_ids: &[i64],
        limit: usize,
        exclude_ids: &HashSet<i64>,
    ) -> Result<Vec<Fact>>;
    /// List active facts in `scope_ids` whose metadata has top-level `marker_key`
    /// (non-null), most-recent first. `scope_ids` empty = **no scopes** (see the
    /// trait-level scope-filtering contract).
    ///
    /// **Precondition (security):** `marker_key` MUST be a non-empty
    /// `[A-Za-z0-9_]+` identifier — it is interpolated into the backend's JSON
    /// path, so a backend MUST reject anything else with
    /// [`MemoryError::Conflict`](crate::error::MemoryError::Conflict) rather than
    /// build the query (the seam preserves the `SQLite` impl's injection guard).
    async fn list_active_facts_by_metadata_key_recent(
        &self,
        scope_ids: &[i64],
        marker_key: &str,
        limit: usize,
    ) -> Result<Vec<Fact>>;
    async fn list_active_facts_in_period(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        scope_ids: &[i64],
        fact_type: Option<&FactType>,
    ) -> Result<Vec<Fact>>;
    async fn list_undreamt_facts_in_period(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        scope_ids: &[i64],
        fact_type: Option<&FactType>,
    ) -> Result<Vec<Fact>>;
    async fn list_active_facts_by_session(
        &self,
        session_id: &str,
        scope_ids: &[i64],
    ) -> Result<Vec<SessionFact>>;

    // --- edges ---
    async fn insert_edge(&self, edge: &NewEdge) -> Result<i64>;
    async fn get_edge(&self, id: i64) -> Result<Edge>;
    /// Soft-expire a single edge by setting its `t_expired`. Idempotency guard:
    /// only an *active* edge is affected, so `Ok(())` always means this call
    /// transitioned an active edge to expired.
    ///
    /// Returns [`MemoryError::NotFound`](crate::error::MemoryError::NotFound) if
    /// no active edge with `id` exists (unknown id, or already expired) — the
    /// write affected 0 rows. Mirrors [`expire_fact`](Self::expire_fact); a
    /// backend MUST honor this rows-affected contract. The `SQLite` impl honors
    /// it; cross-backend conformance coverage (a `#632`-style arm) is a `#635`
    /// follow-up.
    async fn expire_edge(&self, id: i64, now: DateTime<Utc>) -> Result<()>;
    async fn expire_edges_by_fact(&self, fact_id: i64, now: DateTime<Utc>) -> Result<usize>;
    async fn list_all_edges(&self) -> Result<Vec<Edge>>;
    /// Stream every edge (including expired) to `f` — the streaming dump primitive.
    async fn for_each_edge(&self, f: &mut (dyn FnMut(Edge) -> Result<()> + Send)) -> Result<()>;
    async fn list_active_edges(&self) -> Result<Vec<Edge>>;
    async fn list_active_edges_by_source(&self, source_fact_id: i64) -> Result<Vec<Edge>>;
    async fn list_active_edges_by_target(&self, target_fact_id: i64) -> Result<Vec<Edge>>;
    async fn edge_exists_active(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation_type: &str,
    ) -> Result<bool>;
    async fn list_active_edge_pairs_by_facts(
        &self,
        fact_ids: &[i64],
        relation_type: &str,
    ) -> Result<HashSet<(i64, i64)>>;
    async fn hard_delete_edges_by_facts(&self, fact_ids: &[i64]) -> Result<usize>;

    // --- scopes ---
    async fn get_scope(&self, id: i64) -> Result<ScopeNode>;
    async fn find_scope_by_label(&self, parent_id: i64, label: &str) -> Result<Option<ScopeNode>>;
    async fn insert_scope(&self, parent_id: i64, label: &str, depth: i64) -> Result<ScopeNode>;
    async fn ensure_scope_path(&self, path: &str) -> Result<i64>;
    async fn list_all_scopes(&self) -> Result<Vec<ScopeNode>>;
    /// Stream every scope to `f` — the streaming dump primitive.
    async fn for_each_scope(
        &self,
        f: &mut (dyn FnMut(ScopeNode) -> Result<()> + Send),
    ) -> Result<()>;

    // -------------------------------------------------------------------------
    // Stage A atomic port methods (Fork B, §3 of the #631 plan)
    // -------------------------------------------------------------------------

    /// Atomically stamp the embedding identity (fingerprint) and insert one fact,
    /// in a single `rusqlite` transaction.
    ///
    /// This is the verbatim body of `ingest.rs:228–231` moved below the seam.
    /// The caller (`insert_fact_with_embedding`) is responsible for:
    /// - scope resolution (done before the call, autocommit-separate by design)
    /// - HNSW `notify_insert` (fired post-call, engine-side, on success)
    ///
    /// # Contract
    ///
    /// `Ok ⟹ all sub-ops committed; Err ⟹ store byte-identical (tx rolled back)`.
    ///
    /// **Exactly one exception:** `Err(MemoryError::IndexInconsistent)` is returned
    /// *after* the fact is durably committed — it signals the durable write succeeded
    /// but the post-commit in-memory vector (HNSW) index update tripped a structural
    /// invariant (rebuild the index; do **not** retry the write, which would duplicate
    /// the fact). Every *other* `Err` variant preserves the byte-identical guarantee
    /// (nothing was written).
    ///
    /// The `stamp_fn` closure is called **inside** the transaction before the
    /// fact is inserted — it must either record-if-absent or verify the identity,
    /// depending on the call site (live provider vs precomputed fingerprint). A
    /// failing `stamp_fn` rolls back the entire transaction, so a vector is never
    /// committed without an established, matching identity (the #614 guard).
    ///
    /// # Returns
    ///
    /// The assigned `fact_id` on success.
    async fn insert_fact_atomic(
        &self,
        fact: &NewFact,
        fingerprint: &crate::types::EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<i64>;

    /// Atomically insert a batch of facts in a savepoint, returning their ids and
    /// the scope ids that need to be cached engine-side.
    ///
    /// This is the verbatim DB body of `ingest.rs:397–476` moved below the seam.
    ///
    /// The **scope split** (plan §3): scope resolution and `ScopeStore::ensure_path`
    /// run inside the savepoint and are returned as `scope_ids_to_cache` so the
    /// engine can update `scope_tree.write()` **after** the successful commit,
    /// preventing cache desync on rollback.
    ///
    /// # Contract
    ///
    /// `Ok ⟹ all sub-ops committed; Err ⟹ store byte-identical (savepoint rolled back)`.
    ///
    /// **Exactly one exception:** `Err(MemoryError::IndexInconsistent)` is returned
    /// *after* the whole batch is durably committed — it signals the durable write
    /// succeeded but a post-commit in-memory vector (HNSW) index update tripped a
    /// structural invariant (rebuild the index; do **not** retry the write, which
    /// would duplicate the batch). Every *other* `Err` variant preserves the
    /// byte-identical guarantee (nothing was written).
    ///
    /// # Returns
    ///
    /// `(fact_ids, scope_ids_to_cache)` — `fact_ids` in the same order as `facts`;
    /// `scope_ids_to_cache` is the deduplicated set of **new** scope ids resolved
    /// inside the savepoint that the engine must insert into `scope_tree`.
    async fn insert_facts_batch_atomic(
        &self,
        facts: &[NewFact],
        scope_paths: &[Option<String>],
        fingerprint: &crate::types::EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<(Vec<i64>, Vec<i64>)>;

    /// Atomically insert co-session edges (deduplicating against existing ones)
    /// for the given list of fact ids, in a single transaction.
    ///
    /// This is the verbatim tx body of `engine/graph.rs:71–101` moved below the
    /// seam. The `now` timestamp is passed in so the engine controls the clock
    /// (consistent with the surrounding engine logic).
    ///
    /// The caller is responsible for:
    /// - Resolving `scope_ids` from the scope tree (done before the call)
    /// - Updating the in-memory graph with the returned edge triples after the call
    ///
    /// # Contract
    ///
    /// `Ok ⟹ all sub-ops committed; Err ⟹ store byte-identical`.
    ///
    /// # Returns
    ///
    /// `Vec<(edge_id, src_fact_id, tgt_fact_id)>` — the new edges created (empty
    /// if all candidate pairs already had an active co-session edge).
    async fn insert_cosession_edges_atomic(
        &self,
        fact_ids: &[i64],
        relation: &str,
        weight: f64,
        scope_id: i64,
        now: DateTime<Utc>,
    ) -> Result<Vec<(i64, i64, i64)>>;

    /// Atomically execute an arbitrated conflict-resolution write plan in ONE
    /// transaction — the former conflict-resolution path, now encapsulated below
    /// the storage seam.
    ///
    /// The consumer [`ConflictArbiter`](crate::traits::ConflictArbiter) decision is
    /// made engine-side **before** this call; this method performs only the DB
    /// writes the decision implies, all-or-nothing, so a mid-sequence failure can
    /// never leave a partial bi-temporal state (e.g. an old fact expired+invalidated
    /// with no inserted successor):
    /// - `Add`: insert `new_fact`, then a `relation` edge (new → old).
    /// - `Update`: expire+invalidate `old_id`, cascade-expire its edges, insert
    ///   `new_fact`, then a `relation` edge (new → old).
    /// - `Delete`: expire+invalidate `old_id`, cascade-expire its edges.
    /// - `Noop`: no writes (returns `(None, None)`).
    ///
    /// The edge's `scope_id`/`t_created` are taken from `new_fact.scope_id`/`now`;
    /// `relation` is the engine's stable relation string (unused for Delete/Noop).
    ///
    /// # Contract
    ///
    /// `Ok ⟹ all sub-ops committed; Err ⟹ store byte-identical (tx rolled back)`.
    /// Any HNSW sidecar notification fires **post-commit** inside the impl. The
    /// engine mirrors the in-memory graph from the returned ids **after** this
    /// returns `Ok` (no lock held across the await).
    ///
    /// **Exactly one exception to byte-identical:** `Err(MemoryError::IndexInconsistent)`
    /// is returned *after* the write is durably committed — it signals the durable
    /// write (Add/Update insert) succeeded but the post-commit in-memory vector (HNSW)
    /// index update tripped a structural invariant (rebuild the index; do **not** retry
    /// the write, which would duplicate the fact). Every *other* `Err` variant preserves
    /// the byte-identical guarantee (nothing was written).
    ///
    /// # Returns
    ///
    /// `(new_fact_id, edge_id)` — both `Some` for `Add`/`Update`, both `None` for
    /// `Delete`/`Noop`.
    async fn resolve_conflict_atomic(
        &self,
        decision: crate::traits::CrudDecision,
        old_id: i64,
        new_fact: &NewFact,
        relation: &str,
        weight: f64,
        now: DateTime<Utc>,
    ) -> Result<(Option<i64>, Option<i64>)>;

    /// Select archive candidates (expired, non-pinned facts) and their internal
    /// edges.
    ///
    /// This exposes the read body of `engine/archive.rs:167–176` through the port.
    /// It is a **read method** (uses a read connection) — the write commit is
    /// `ColdStorage::commit_archive_atomic`.
    ///
    /// # Note on `archive_dir`
    ///
    /// `pool.path()` disappears post-cutover (Stage E). In Stage A this method
    /// simply returns the candidate data; the engine owns path resolution from
    /// `EngineConfig` until Stage E rewires it. A backend-side path accessor is
    /// **not** needed at this stage.
    async fn select_archive_candidates(
        &self,
        expired_before: DateTime<Utc>,
    ) -> Result<(Vec<crate::types::Fact>, Vec<crate::types::Edge>)>;

    /// Atomically materialize importance scores for the active set and expire the
    /// sub-threshold facts (cascading edge expiry) in a single transaction.
    ///
    /// This is the write phase of `forgetting::policy::prune` (the importance sweep)
    /// moved below the seam. Scoring reads in-memory graph degrees, so it stays
    /// engine-side; the engine passes the precomputed `scored` pairs (one `(id,
    /// score)` per active fact) and the `to_expire` subset that fell below the
    /// importance threshold.
    ///
    /// # Contract
    ///
    /// `Ok ⟹ all sub-ops committed; Err ⟹ store byte-identical (tx rolled back)`.
    ///
    /// The caller is responsible for mirroring the returned expired ids into the
    /// in-memory graph (`remove_edges_by_fact`) **after** this returns `Ok`.
    ///
    /// # Returns
    ///
    /// `(PruneStats, actually_expired)` — `facts_evaluated = scored.len()`,
    /// `facts_expired = to_expire.len()`; `actually_expired` is the id list the
    /// engine reconciles against its in-memory graph post-commit.
    async fn prune_atomic(
        &self,
        scored: &[(i64, f64)],
        to_expire: &[i64],
        now: DateTime<Utc>,
    ) -> Result<(crate::forgetting::PruneStats, Vec<i64>)>;

    // -------------------------------------------------------------------------
    // Stage E bootstrap seams — the conn-threaded import pipelines run below the
    // seam on the write connection (where a blocking `EmbeddingProvider`/extractor
    // is nested-runtime-safe). The engine resolves the scope id up front and passes
    // owned `Arc` consumer handles; ownership moves straight through the blocking
    // boundary. Each preserves the per-session/per-file savepoint atomicity exactly.
    // -------------------------------------------------------------------------

    /// Import one JSONL session log into historical memory (one savepoint).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ReadOnly`](crate::error::MemoryError::ReadOnly) on a
    /// read-only backend, or an embedding / extraction / store error.
    async fn bootstrap_session_atomic(
        &self,
        reader: Box<dyn std::io::BufRead + Send>,
        embedder: std::sync::Arc<dyn crate::traits::EmbeddingProvider>,
        extractor: std::sync::Arc<dyn crate::bootstrap::SessionExtractor>,
        config: crate::bootstrap::BootstrapConfig,
        classifier: Option<std::sync::Arc<dyn crate::traits::PersistenceClassifier>>,
        scope_id: i64,
    ) -> Result<crate::bootstrap::BootstrapReport>;

    /// Import every top-level `*.jsonl` session log in `dir` (each its own savepoint).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ReadOnly`](crate::error::MemoryError::ReadOnly) on a
    /// read-only backend, [`MemoryError::Io`](crate::error::MemoryError::Io) on a
    /// traversal failure, or an embedding / store error.
    async fn bootstrap_directory_atomic(
        &self,
        dir: std::path::PathBuf,
        embedder: std::sync::Arc<dyn crate::traits::EmbeddingProvider>,
        extractor: std::sync::Arc<dyn crate::bootstrap::SessionExtractor>,
        config: crate::bootstrap::BootstrapConfig,
        classifier: Option<std::sync::Arc<dyn crate::traits::PersistenceClassifier>>,
        scope_id: i64,
    ) -> Result<crate::bootstrap::BootstrapReport>;

    /// Import native `.md` memory files (recursive) from `dir` — autocommit per file.
    /// Stamps the embedding identity meta-first (#643) before the first file.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ReadOnly`](crate::error::MemoryError::ReadOnly) on a
    /// read-only backend, [`MemoryError::Io`](crate::error::MemoryError::Io) on a
    /// traversal failure, or an embedding / store error.
    async fn bootstrap_memory_directory_atomic(
        &self,
        dir: std::path::PathBuf,
        embedder: std::sync::Arc<dyn crate::traits::EmbeddingProvider>,
        config: crate::bootstrap::BootstrapConfig,
        classifier: Option<std::sync::Arc<dyn crate::traits::PersistenceClassifier>>,
        scope_id: i64,
    ) -> Result<crate::bootstrap::BootstrapReport>;
}
