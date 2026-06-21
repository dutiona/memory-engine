use std::sync::Arc;

use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::traits::{ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummaryGenerator};

use super::{MemoryEngine, spawn_join_err};

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
    pub async fn consolidate(
        &self,
        generator: Arc<dyn SummaryGenerator>,
        embedder: Arc<dyn EmbeddingProvider>,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        // Phase 1 — READ: snapshot the active set + watermark below the seam.
        let snapshot = self
            .storage
            .load_consolidation_snapshot(config.clone())
            .await?;

        // Phase 2 — COMPUTE (no lock): dedup decision + cluster/global summaries,
        // including the consumer `summarize`/`embed` IO. Offloaded to the blocking
        // pool so the (possibly blocking HTTP) consumer calls never park the async
        // executor (#409) — and a `reqwest::blocking` provider stays nested-runtime-safe.
        let plan = {
            let config = config.clone();
            let embed_dim = self.embed_dim;
            tokio::task::spawn_blocking(move || {
                crate::consolidation::compute_plan(
                    &snapshot,
                    &*generator,
                    &*embedder,
                    embed_dim,
                    &config,
                )
            })
            .await
            .map_err(spawn_join_err)??
        };

        // Phase 3 — WRITE: apply atomically below the seam. `apply_plan` returns the
        // ids it *actually* expired (a concurrent writer may have pre-empted some in
        // the read→compute gap) and fires the HNSW `notify_expire` internally (Stage
        // B), so the engine rebuilds its in-memory graph off the real change set only.
        let (stats, expired_ids) = self.storage.apply_plan(plan).await?;
        if !expired_ids.is_empty() {
            // Rebuild the in-memory graph from the active edge set (port read first,
            // then take the write guard — no guard held across `.await`).
            let edges = self.storage.list_active_edges().await?;
            *self.graph.write() = MemoryGraph::from_active_edges(&edges);
        }

        Ok(stats)
    }
}
