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
    async fn set_fact_pinned(&self, id: i64, pinned: bool) -> Result<()>;
    async fn update_fact_importance(&self, id: i64, importance: f64) -> Result<()>;
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
    async fn list_dormant_facts(
        &self,
        importance_threshold: f64,
        scope_ids: Option<&[i64]>,
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
    async fn list_pinned_facts(&self, scope_ids: &[i64]) -> Result<Vec<Fact>>;
    async fn list_due_facts(&self, now: DateTime<Utc>, scope_ids: &[i64]) -> Result<Vec<Fact>>;
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
}
