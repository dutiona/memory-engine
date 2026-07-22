//! Forgetting and pinning operations for [`MemoryEngine`].

use chrono::Utc;

use crate::error::Result;
use crate::forgetting::{ForgetPolicy, PruneStats};

use super::MemoryEngine;

impl MemoryEngine {
    /// Prune stale facts using Ebbinghaus decay and graph-aware importance scoring.
    ///
    /// Facts with computed importance below `policy.min_importance` get soft-deleted.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Conflict` if the policy is invalid.
    /// Returns `MemoryError::Storage` on SQL failure.
    pub async fn forget(&self, policy: &ForgetPolicy) -> Result<PruneStats> {
        self.ensure_open()?;
        // The prune walk reads the in-memory graph (degree per fact) *before* any
        // port write and mutates it (`remove_edges_by_fact`) *after* the expiries
        // commit. To keep the future `Send`, the graph guards live entirely inside
        // the async `prune` helper, scoped around each `.await` — no `self.graph`
        // guard is held across an await here. HNSW expiry notification now happens
        // inside the backend's `expire_fact` (Stage B, #713), so the old engine-side
        // `notify_expire` loop is gone.
        let (stats, _pruned_ids) =
            crate::forgetting::prune(self.mem_ctx(), &self.graph, policy, Utc::now()).await?;
        Ok(stats)
    }

    /// Pin a fact (make it unforgettable).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Storage` if the update fails.
    pub async fn pin_fact(&self, id: i64) -> Result<()> {
        self.ensure_writable()?;
        self.storage.set_fact_pinned(id, true).await
    }

    /// Unpin a fact (allow forgetting).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Storage` if the update fails.
    pub async fn unpin_fact(&self, id: i64) -> Result<()> {
        self.ensure_writable()?;
        self.storage.set_fact_pinned(id, false).await
    }
}
