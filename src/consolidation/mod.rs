//! Three-pass consolidation pipeline: dedup, cluster fusion, global integration.
//!
//! All passes run atomically in a single `SQLite` transaction.

mod cluster;
mod dedup;
mod global;

pub use cluster::cluster_fusion;
pub use dedup::local_dedup;
pub use global::global_integration;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::store::schema::{get_config, set_config};
use crate::traits::{ConsolidationConfig, ConsolidationStats, SummaryGenerator};

/// Orchestrate all 3 consolidation passes atomically.
///
/// 1. Local dedup — expire near-duplicate facts
/// 2. Cluster fusion — group related facts, generate cluster summaries
/// 3. Global integration — summarize all clusters into one global summary
///
/// All passes run within a single transaction. On any failure (including
/// `SummaryGenerator` errors), the entire consolidation is rolled back.
///
/// Reads `last_consolidated_at` from config to scope dedup.
/// Updates `last_consolidated_at` after successful completion.
///
/// # Errors
///
/// Propagates errors from any pass or the `SummaryGenerator`.
pub fn consolidate(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embed_dim: usize,
    config: &ConsolidationConfig,
) -> Result<(ConsolidationStats, Vec<i64>)> {
    let last = get_config(conn, "last_consolidated_at")?
        .map(|s| DateTime::parse_from_rfc3339(&s))
        .transpose()
        .map_err(|e| {
            crate::error::MemoryError::Migration(format!("invalid last_consolidated_at: {e}"))
        })?
        .map(|dt| dt.with_timezone(&Utc));
    let now = Utc::now();

    let tx = conn.unchecked_transaction()?;

    let (duplicates_removed, expired_ids) =
        local_dedup(&tx, embed_dim, config.dedup_threshold, last, now)?;

    // usize::MAX is a sentinel from local_dedup meaning "skipped due to safety cap".
    let dedup_skipped = duplicates_removed == usize::MAX;
    let duplicates_removed = if dedup_skipped { 0 } else { duplicates_removed };

    let clusters_created = cluster_fusion(&tx, generator, embed_dim, config.min_cluster_size)?;
    let global_summaries = global_integration(&tx, generator, embed_dim)?;

    // Only advance the watermark if dedup actually ran. When skipped, facts
    // ingested during the over-cap period must be retried on the next run.
    if !dedup_skipped {
        set_config(&tx, "last_consolidated_at", &now.to_rfc3339())?;
    }

    tx.commit()?;

    Ok((
        ConsolidationStats {
            duplicates_removed,
            clusters_created,
            global_summaries,
        },
        expired_ids,
    ))
}
