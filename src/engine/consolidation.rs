use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::traits::{ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummaryGenerator};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::MemoryEngine;

impl MemoryEngine {
    /// Run three-pass consolidation: local dedup, cluster fusion, global integration.
    ///
    /// `generator` produces the summary text for each cluster and the global pass;
    /// `embedder` projects that text into the fact vector space (issue #116 — embedding
    /// is no longer duplicated on the `SummaryGenerator` trait).
    ///
    /// # Lock discipline (#409)
    ///
    /// Structured as **read → compute → write** so the engine's single write lock is
    /// NOT held across the consumer `summarize`/`embed` calls (unbounded network IO):
    ///
    /// 1. **Read** (brief lock): snapshot the active set + watermark.
    /// 2. **Compute** (no lock): run dedup/cluster/global, including all consumer IO,
    ///    against the snapshot — producing a `ConsolidationPlan` of pure data.
    /// 3. **Write** (brief lock, one transaction): apply the plan atomically, then
    ///    rebuild the graph if dedup expired anything.
    ///
    /// Atomicity is preserved (a consumer failure aborts in the compute phase, before any
    /// write; the apply is all-or-nothing), but other engine writers — and, for an
    /// in-memory pool, readers — are no longer starved for the IO duration. Because the
    /// lock is released across the compute phase, a concurrent writer (`prune`, conflict
    /// resolution) may expire a planned-for fact in the gap; the apply phase tolerates
    /// that. This method is **not** designed to be invoked concurrently *with itself* —
    /// the per-phase lock no longer serializes whole runs, so two overlapping runs are a
    /// benign last-writer-wins on derived summaries, not a supported pattern.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only. Propagates
    /// errors from any consolidation pass, the `SummaryGenerator`, or the
    /// `EmbeddingProvider`.
    pub fn consolidate(
        &self,
        generator: &dyn SummaryGenerator,
        embedder: &dyn EmbeddingProvider,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        // Phase 1 — READ (brief lock): snapshot the active set + watermark, then release.
        let snapshot = {
            let conn = self.write_conn()?;
            crate::consolidation::load_snapshot(&conn, self.embed_dim, config)?
        };

        // Phase 2 — COMPUTE (NO lock): dedup decision + cluster/global summaries, including
        // the consumer `summarize`/`embed` network IO. This is the work that used to run
        // under the write lock and starve every other writer (#409).
        let plan = crate::consolidation::compute_plan(
            &snapshot,
            generator,
            embedder,
            self.embed_dim,
            config,
        )?;

        // Phase 3 — WRITE (brief lock, one transaction): apply atomically, then rebuild
        // the graph inside the same lock if dedup expired facts (and their edges).
        // `apply_plan` returns the ids it *actually* expired (a concurrent writer may have
        // pre-empted some in the read→compute gap), so the graph rebuild and the vector
        // index notify below key off real changes, not the stale plan.
        let conn = self.write_conn()?;
        let (stats, expired_ids) = crate::consolidation::apply_plan(&conn, &plan, self.embed_dim)?;
        if !expired_ids.is_empty() {
            *self.graph.write() = MemoryGraph::load_from_db(&conn)?;
        }
        drop(conn); // release the write lock before notifying the vector index

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for &id in &expired_ids {
                hnsw.notify_expire(id);
            }
        }

        Ok(stats)
    }
}
