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
}
