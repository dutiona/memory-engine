use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::traits::{ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummaryGenerator};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::MemoryEngine;

impl MemoryEngine {
    /// Run three-pass consolidation: local dedup, cluster fusion, global integration.
    ///
    /// `generator` produces the summary text for each cluster and the global
    /// pass; `embedder` projects that text into the fact vector space. Embedding
    /// is no longer duplicated on the `SummaryGenerator` trait (issue #116).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Propagates errors from any consolidation pass, the `SummaryGenerator`, or
    /// the `EmbeddingProvider`.
    pub fn consolidate(
        &self,
        generator: &dyn SummaryGenerator,
        embedder: &dyn EmbeddingProvider,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        let (stats, expired_ids) = {
            let conn = self.write_conn()?;
            let (stats, expired_ids) = crate::consolidation::consolidate(
                &conn,
                generator,
                embedder,
                self.embed_dim,
                config,
            )?;

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
