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

/// Orchestrate all 3 consolidation passes.
///
/// 1. Local dedup — expire near-duplicate facts
/// 2. Cluster fusion — group related facts, generate cluster summaries
/// 3. Global integration — summarize all clusters into one global summary
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
) -> Result<ConsolidationStats> {
    let last = get_config(conn, "last_consolidated_at")?
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let now = Utc::now();

    let duplicates_removed = local_dedup(conn, embed_dim, config.dedup_threshold, last, now)?;
    let clusters_created = cluster_fusion(conn, generator, embed_dim, config.min_cluster_size)?;
    let global_summaries = global_integration(conn, generator, embed_dim)?;

    set_config(conn, "last_consolidated_at", &now.to_rfc3339())?;

    Ok(ConsolidationStats {
        duplicates_removed,
        clusters_created,
        global_summaries,
    })
}
