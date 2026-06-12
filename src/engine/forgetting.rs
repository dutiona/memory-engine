//! Forgetting and pinning operations for [`MemoryEngine`].

use chrono::Utc;

use crate::error::Result;
use crate::store::facts::FactStore;
use crate::traits::{ForgetPolicy, PruneStats};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::MemoryEngine;

impl MemoryEngine {
    /// Prune stale facts using Ebbinghaus decay and graph-aware importance scoring.
    ///
    /// Facts with computed importance below `policy.min_importance` get soft-deleted.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the policy is invalid.
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn forget(&self, policy: &ForgetPolicy) -> Result<PruneStats> {
        let (stats, pruned_ids) = {
            let conn = self.write_conn()?;
            let mut graph = self.graph.write();
            crate::forgetting::prune(&conn, &mut graph, policy, self.embed_dim, Utc::now())?
        };

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for &id in &pruned_ids {
                hnsw.notify_expire(id);
            }
        }

        let _ = pruned_ids; // consumed above when ann is enabled; suppress unused warning otherwise
        Ok(stats)
    }

    /// Pin a fact (make it unforgettable).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if the update fails.
    pub fn pin_fact(&self, id: i64) -> Result<()> {
        let conn = self.write_conn()?;
        FactStore::new(&conn, self.embed_dim).set_pinned(id, true)
    }

    /// Unpin a fact (allow forgetting).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if the update fails.
    pub fn unpin_fact(&self, id: i64) -> Result<()> {
        let conn = self.write_conn()?;
        FactStore::new(&conn, self.embed_dim).set_pinned(id, false)
    }
}
