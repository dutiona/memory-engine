use chrono::Utc;

use crate::error::Result;
use crate::traits::{ConflictArbiter, ConflictResolution};
use crate::types::NewFact;

use super::MemoryEngine;

impl MemoryEngine {
    /// Resolve a conflict between an existing fact and a candidate new fact.
    ///
    /// Delegates the decision to the consumer-provided [`ConflictArbiter`].
    /// Graph is updated only after the persistence operations succeed.
    ///
    /// **Arbiter input caveat:** the `new_fact` passed to
    /// [`ConflictArbiter::arbitrate`] is a pre-insert synthetic `Fact` built via
    /// [`Fact::from_new_for_arbiter`](crate::types::Fact). Its `id` is always `0`
    /// (not yet assigned by the DB) and `importance_score` is the
    /// [`Fact::UNSCORED_IMPORTANCE`](crate::types::Fact::UNSCORED_IMPORTANCE)
    /// sentinel (`0.5`), NOT the eventual stored score. Arbiters must rely on
    /// `content`, `fact_type`, `base_importance`, and `metadata` — never on `id`
    /// or `importance_score`.
    ///
    /// **Graph/DB consistency:** the in-memory graph is updated only after the DB
    /// commit succeeds. A panic in the small window between the commit and the
    /// graph mirror leaves the graph and DB transiently diverged for the rest of
    /// the session; the next `open()` recovers the graph via
    /// `MemoryGraph::load_from_db`.
    ///
    /// # Errors
    ///
    /// - [`MemoryError::ReadOnly`](crate::error::MemoryError::ReadOnly) — the
    ///   engine was opened in read-only mode.
    /// - [`MemoryError::Conflict`](crate::error::MemoryError::Conflict) wrapping
    ///   [`ConflictError::PayloadTooLarge`](crate::error::ConflictError::PayloadTooLarge)
    ///   — the candidate `new_fact` exceeds the size bound enforced by
    ///   `check_new_fact`. Checked before the arbiter is called.
    /// - [`MemoryError::NotFound`](crate::error::MemoryError::NotFound) — `old_id`
    ///   is missing or already expired. The lookup itself retrieves expired facts,
    ///   so an already-expired `old_id` is rejected later, *inside the atomic
    ///   transaction*: `Update`/`Delete` change zero rows under their
    ///   `t_expired IS NULL` expiry, and `Add` re-validates `old_id` is still
    ///   active before creating its edge (the #335 read→write TOCTOU guard). A
    ///   missing `old_id` is rejected at lookup. All cases are indistinguishable to
    ///   the caller (each yields `NotFound`).
    /// - Propagates any error returned by the [`ConflictArbiter`] or the
    ///   underlying database operations.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use memory_engine::{MemoryEngine, NewFact, FactType, Fact, Result};
    /// use memory_engine::traits::{ConflictArbiter, CrudDecision};
    ///
    /// struct AlwaysUpdate;
    /// impl ConflictArbiter for AlwaysUpdate {
    ///     fn arbitrate(&self, _old: &Fact, _new: &Fact) -> Result<CrudDecision> {
    ///         Ok(CrudDecision::Update)
    ///     }
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let engine = MemoryEngine::builder(4).build()?;
    ///
    ///     // Suppose `old_id` was obtained from a prior `add_fact` or `ingest` call.
    ///     let old_id: i64 = 1;
    ///     let candidate = NewFact::builder("updated content", vec![0.1; 4], FactType::Semantic)
    ///         .build();
    ///
    ///     let resolution = engine.resolve_conflict(&AlwaysUpdate, old_id, &candidate).await?;
    ///     println!("decision: {:?}, new fact id: {:?}", resolution.decision, resolution.new_fact_id);
    ///     Ok(())
    /// }
    /// ```
    pub async fn resolve_conflict(
        &self,
        arbiter: &dyn ConflictArbiter,
        old_id: i64,
        new_fact: &NewFact,
    ) -> Result<ConflictResolution> {
        me_resolve::resolve_conflict(
            self.mem_ctx(),
            &self.graph,
            arbiter,
            old_id,
            new_fact,
            Utc::now(),
        )
        .await
    }
}
