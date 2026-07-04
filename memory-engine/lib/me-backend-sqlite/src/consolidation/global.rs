//! The SQL-touching half of the global-integration pass, moved below the seam (Wave 2
//! #816 / S2, sub-PR 2b). `compute_global` (pure) stays in the facade's
//! `consolidation::global`, which re-exports [`apply_global`] so its own
//! `#[cfg(test)]`-only `global_integration` wrapper (composing `compute_global` +
//! `apply_global` for that file's unit tests) keeps resolving unchanged.

use rusqlite::Connection;

use me_types::error::Result;
use me_types::types::{ConsolidationLevel, NewSummary};

use crate::store::summaries::SummaryStore;

/// Apply the computed global summary inside the caller's write context (#409).
///
/// Clears the prior global summary (idempotent), then inserts the new one if present.
/// `None` leaves the store with no global summary (the cleared state).
///
/// # Errors
///
/// Returns `MemoryError::Storage` on SQL failure, or `MemoryError::Serialization` on
/// JSON serialization failure.
pub fn apply_global(
    conn: &Connection,
    embed_dim: usize,
    summary: Option<&NewSummary>,
) -> Result<()> {
    let summary_store = SummaryStore::new(conn, embed_dim);
    summary_store.delete_by_level(&ConsolidationLevel::Global)?;
    if let Some(s) = summary {
        summary_store.insert(s)?;
    }
    Ok(())
}
