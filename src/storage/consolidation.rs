//! Consolidation outputs: cluster/global summaries + wisdom-promotion lineage.
//!
//! Folds `store/summaries.rs` + `store/lineage.rs`. Lineage is Phase-5 wisdom
//! provenance (which facts a wisdom fact was synthesized from) — a consolidation
//! *output*, so it lives here.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{
    ConsolidationLevel, LineageRecord, LineageSnapshotEntry, NewLineageRecord, NewSummary,
    PromotionProvenance, Summary,
};

/// Consolidation outputs: summaries + wisdom lineage.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](crate::error::MemoryError::Storage)
/// on a backend failure (or [`NotFound`](crate::error::MemoryError::NotFound) /
/// [`Lineage`](crate::error::MemoryError::Lineage) where applicable).
#[async_trait]
pub trait ConsolidationStore: Send + Sync {
    // --- summaries ---
    async fn insert_summary(&self, summary: &NewSummary) -> Result<i64>;
    async fn get_summary(&self, id: i64) -> Result<Summary>;
    async fn list_summaries_by_level(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>>;
    async fn list_all_summaries(&self) -> Result<Vec<Summary>>;
    /// Stream every summary to `f` — the O(1)-peak-memory dump primitive.
    async fn for_each_summary(
        &self,
        f: &mut (dyn FnMut(Summary) -> Result<()> + Send),
    ) -> Result<()>;
    async fn delete_summaries_by_level(&self, level: &ConsolidationLevel) -> Result<usize>;

    // --- lineage ---
    async fn insert_lineage(
        &self,
        record: &NewLineageRecord,
        provenance: &PromotionProvenance,
    ) -> Result<i64>;
    async fn insert_lineage_raw(&self, entry: &LineageSnapshotEntry) -> Result<()>;
    async fn get_lineage_by_wisdom_fact(
        &self,
        wisdom_fact_id: i64,
    ) -> Result<(LineageRecord, PromotionProvenance)>;
    async fn get_lineage_source_fact_ids(&self, wisdom_fact_id: i64) -> Result<Vec<i64>>;
    async fn delete_lineage(&self, wisdom_fact_id: i64) -> Result<bool>;
    async fn has_lineage(&self, wisdom_fact_id: i64) -> Result<bool>;
    /// Stream every lineage row to `f` — the snapshot/export dump primitive.
    /// (`LineageStore` has no `list_all` today, so this is the full-scan.)
    async fn for_each_lineage(
        &self,
        f: &mut (dyn FnMut(LineageSnapshotEntry) -> Result<()> + Send),
    ) -> Result<()>;

    // -------------------------------------------------------------------------
    // Stage A atomic port method (Fork B, §3 of the #631 plan)
    // -------------------------------------------------------------------------

    /// Atomically apply a validated [`CycleReport`]'s DB-touching deltas in a
    /// single `rusqlite` transaction, returning the supersede-edge triples that
    /// the engine must mirror into its in-memory graph after commit.
    ///
    /// ## Push-down scope (full push-down — plan §3 + §6.6)
    ///
    /// This method receives a **pre-validated** report (the engine's pure-Rust
    /// `validate_report` runs on the already-held write connection before calling
    /// this). Both `validate_report` AND `apply` move below the seam in a single
    /// transaction, so the original self-deadlock (a separate read guard would be
    /// needed for validation on an in-memory engine) is avoided — the single
    /// `block_write` closure owns both phases.
    ///
    /// ## `Promote` variant blocker — PARTIAL push-down
    ///
    /// `CycleDelta::Promote` calls `promote_in_conn`, which calls
    /// `ensure_scope_with_conn`, which writes to `self.scope_tree` — engine-owned
    /// in-memory state that is not accessible below the seam. Full push-down of
    /// the `Promote` variant is therefore **blocked** until Stage E wires the
    /// engine to use the port for scope resolution.
    ///
    /// **Decision (Stage A):** the `Promote` variant's promotion + lineage insert
    /// are handled below the seam (the `FactStore`/`LineageStore` writes), but
    /// scope resolution for a non-None `req.scope` must be performed by the engine
    /// before calling this method and passed as a resolved `scope_id`. Since the
    /// existing `engine/cycle/apply.rs` always passes `scope: None` for promoted
    /// wisdom (root scope = 1), this is a no-op in practice — the verbatim
    /// push-down uses `scope_id = 1` for all promotions, matching the current
    /// behavior exactly.
    ///
    /// ## Contract
    ///
    /// `Ok ⟹ all sub-ops committed; Err ⟹ store byte-identical (tx rolled back)`.
    ///
    /// The caller is responsible for:
    /// - `CycleError` business validation (pure-Rust, no conn needed)
    /// - HNSW `notify_insert`/`notify_expire` (fired post-commit, engine-side)
    /// - Mirroring supersede edges into the in-memory graph (from the return value)
    ///
    /// ## Returns
    ///
    /// `(apply_result, supersede_edges, expired_ids, to_index)` where:
    /// - `apply_result` — the `ApplyResult` filled during apply
    /// - `supersede_edges` — `Vec<(new_id, old_id, edge_id)>` for graph mirroring
    /// - `expired_ids` — ids of all expired facts (for HNSW tombstoning)
    /// - `to_index` — `(fact_id, embedding)` pairs for HNSW `notify_insert`
    async fn apply_cycle_deltas_atomic(
        &self,
        report: &crate::engine::cycle::CycleReport,
        embed_dim: usize,
        upcaster_registry: &crate::store::upcaster::UpcasterRegistry,
    ) -> Result<(
        crate::engine::cycle::ApplyResult,
        Vec<(i64, i64, i64)>,
        Vec<i64>,
        Vec<(i64, Vec<f32>)>,
    )>;
}
