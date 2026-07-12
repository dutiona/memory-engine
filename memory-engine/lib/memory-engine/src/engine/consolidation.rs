//! Consolidation delegate for [`MemoryEngine`].
//!
//! Extracted into the [`me_consolidate`] crate (Wave 2 #816 / S4, sub-PR 4 — the final
//! S4 sub-PR, closes #940). `MemoryEngine::consolidate` resolves this engine's
//! `MemoryCtx` + in-memory graph, then delegates to `me_consolidate::consolidate`.
//! Base's pre-carve `consolidate` called only `self.ensure_open()` (the #742
//! read-fence) — no explicit read-only pre-check — so the delegate stays a one-line
//! forward with no pre-flight of its own; see `me_consolidate::consolidate`'s own doc
//! for why `ReadOnly` is caught below the seam instead.

use std::sync::Arc;

use crate::error::Result;
use crate::traits::{ConsolidationConfig, ConsolidationStats, EmbeddingProvider, SummaryGenerator};

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
    pub async fn consolidate(
        &self,
        generator: Arc<dyn SummaryGenerator>,
        embedder: Arc<dyn EmbeddingProvider>,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        me_consolidate::consolidate(self.mem_ctx(), &self.graph, generator, embedder, config).await
    }
}
