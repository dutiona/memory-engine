use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::traits::{ConsolidationConfig, ConsolidationStats, SummaryGenerator};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::MemoryEngine;

impl MemoryEngine {
    /// Run three-pass consolidation: local dedup, cluster fusion, global integration.
    ///
    /// # Errors
    ///
    /// Propagates errors from any consolidation pass or the `SummaryGenerator`.
    pub fn consolidate(
        &self,
        generator: &dyn SummaryGenerator,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        let (stats, expired_ids) = {
            let conn = self.write_conn()?;
            let (stats, expired_ids) =
                crate::consolidation::consolidate(&conn, generator, self.embed_dim, config)?;

            // Rebuild graph inside write lock — dedup may have expired facts and their edges
            if stats.duplicates_removed > 0 {
                *self.graph.write() = MemoryGraph::load_from_db(&conn)?;
            }

            (stats, expired_ids)
        }; // DB lock released

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for &id in &expired_ids {
                hnsw.notify_expire(id);
            }
        }

        // Suppress unused variable warning when ann feature is disabled
        #[cfg(not(feature = "ann"))]
        let _ = expired_ids;

        Ok(stats)
    }
}
