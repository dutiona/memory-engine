use chrono::Utc;

use crate::error::Result;
use crate::traits::{ConflictArbiter, ConflictResolution};
use crate::types::NewFact;

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::MemoryEngine;

impl MemoryEngine {
    /// Resolve a conflict between an existing fact and a candidate new fact.
    ///
    /// Delegates the decision to the consumer-provided [`ConflictArbiter`].
    /// Mutations happen in a single transaction; graph is updated only after commit.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::NotFound` if the old fact doesn't exist.
    /// Propagates errors from the arbiter or database operations.
    pub fn resolve_conflict(
        &self,
        arbiter: &dyn ConflictArbiter,
        old_id: i64,
        new_fact: &NewFact,
    ) -> Result<ConflictResolution> {
        // The candidate fact is persisted verbatim on an Add/Update decision, so
        // it is a consumer ingest path and must respect the same size bound as
        // `add_fact` (issue #572 / L10).
        crate::limits::check_new_fact(new_fact)?;

        #[cfg(feature = "ann")]
        let embedding = new_fact.embedding.clone();
        let resolution = {
            let conn = self.write_conn()?;
            let mut graph = self.graph.write();
            crate::conflict::resolve_conflict(
                &conn,
                &mut graph,
                arbiter,
                old_id,
                new_fact,
                self.embed_dim,
                Utc::now(),
            )?
        }; // DB lock + graph lock released

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            use crate::traits::CrudDecision;
            if matches!(
                &resolution.decision,
                CrudDecision::Update | CrudDecision::Delete
            ) {
                hnsw.notify_expire(old_id);
            }
            if matches!(
                &resolution.decision,
                CrudDecision::Update | CrudDecision::Add
            ) {
                if let Some(new_id) = resolution.new_fact_id {
                    hnsw.notify_insert(new_id, &embedding);
                }
            }
        }

        Ok(resolution)
    }
}
