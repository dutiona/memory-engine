//! The SQL-touching half of the cluster-fusion pass, moved below the seam (Wave 2 #816
//! / S2, sub-PR 2b). `compute_clusters`/`greedy_cluster` (pure) stay in the facade's
//! `consolidation::cluster`, which re-exports [`apply_clusters`] so its own
//! `#[cfg(test)]`-only `cluster_fusion` wrapper (composing `compute_clusters` +
//! `apply_clusters` for that file's unit tests) keeps resolving unchanged.

use rusqlite::Connection;

use me_types::error::Result;
use me_types::types::{ConsolidationLevel, NewSummary};

use crate::store::summaries::SummaryStore;

/// Apply computed cluster summaries inside the caller's write context (#409).
///
/// Clears the prior cluster-level summaries (idempotent), then inserts the new ones.
/// Call this only when the pass actually ran (`ClusterComputed::ran`, facade-side) — over
/// the cap, existing summaries must be preserved because there are no replacements.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on SQL failure, or `MemoryError::Serialization` on
/// JSON serialization failure.
pub fn apply_clusters(conn: &Connection, embed_dim: usize, summaries: &[NewSummary]) -> Result<()> {
    let summary_store = SummaryStore::new(conn, embed_dim);
    summary_store.delete_by_level(&ConsolidationLevel::Cluster)?;
    for summary in summaries {
        summary_store.insert(summary)?;
    }
    Ok(())
}
