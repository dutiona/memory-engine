//! The forget retraction (#1023, ADR-0020 §8): retract a fact behind a tombstone and reverse
//! it. Each operation is one port call ([`Retraction`](me_storage::Retraction)); the facade
//! resolves the Homes, bounds the inputs and caches the scope paths it created.

use chrono::Utc;
use me_types::limits::check_str_size;

use crate::error::Result;
use crate::types::{EraseReport, EraseTarget, ForgetOutcome, NewEvent, UnforgetOutcome};

use super::MemoryEngine;

impl MemoryEngine {
    /// Retract fact `id` ("this fact won't come up again") when it is filed at one of the
    /// scope paths `homes`, and append `record`, the call's record, under the scope path
    /// `record_at`, in one transaction. Returns the outcome and the record's id.
    ///
    /// The fact and every active fact with the same content in `homes` leave recall: each
    /// gets a tombstone naming the record and holding `reason`, and its validity ends. The
    /// rows and every event stay, and the tombstone keeps that content out of later imports
    /// and promotions. `homes` resolves exactly, never to a subtree; a path the store does
    /// not know names nothing, and a fact elsewhere answers
    /// [`ForgetOutcome::NotFound`], the same as a missing id.
    ///
    /// # Errors
    ///
    /// `MemoryError::ReadOnly` on a read-only engine; a `PayloadTooLarge` conflict for an
    /// oversized `reason` or record; `MemoryError::Storage` on a write failure. Nothing is
    /// written on any error.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use memory_engine::types::{AddFactRequest, EventType, FactType, ForgetOutcome, NewEvent};
    /// let engine = memory_engine::MemoryEngine::builder(4).build().unwrap();
    /// let fingerprint = memory_engine::EmbeddingFingerprint::new("doc", "test", 4);
    /// let request = AddFactRequest {
    ///     content: "the build is red".into(),
    ///     fact_type: FactType::Semantic,
    ///     source_event_id: None,
    ///     scope: Some("user:alice".into()),
    ///     opts: None,
    /// };
    /// let id = engine
    ///     .add_fact_precomputed(&request, vec![0.1; 4], &fingerprint, None)
    ///     .await
    ///     .unwrap();
    /// let record = NewEvent {
    ///     timestamp: chrono::Utc::now(),
    ///     event_type: EventType::MemoryOp,
    ///     payload: serde_json::json!({ "tool": "forget" }),
    ///     source: "doc".into(),
    ///     session_id: None,
    ///     scope_id: 1,
    ///     origin_node_id: "doc".into(),
    ///     sequence_id: 0,
    ///     created_at: None,
    /// };
    /// let at = "unscoped:laptop";
    /// for (homes, outcome) in [
    ///     ("user:alice", ForgetOutcome::Forgotten),
    ///     ("user:alice", ForgetOutcome::AlreadyForgotten),
    ///     ("user:bob", ForgetOutcome::NotFound),
    /// ] {
    ///     let homes = [homes.to_owned()];
    ///     let (answer, _) = engine.forget_fact(id, &homes, "wrong", at, &record).await.unwrap();
    ///     assert_eq!(answer, outcome);
    /// }
    /// # });
    /// ```
    pub async fn forget_fact(
        &self,
        id: i64,
        homes: &[String],
        reason: &str,
        record_at: &str,
        record: &NewEvent,
    ) -> Result<(ForgetOutcome, i64)> {
        self.ensure_open()?;
        self.ensure_writable()?;
        check_str_size(reason, "forget reason")?;
        let (record, record_found) = self.scrubbed_event(record)?;
        let (reason, reason_found) = crate::redact::scrub_with(reason, &[])?;
        check_str_size(&reason, "forget reason")?;
        let homes: Vec<i64> = {
            let tree = self.scope_tree.read();
            homes
                .iter()
                .filter_map(|path| tree.resolve_path(path))
                .collect()
        };
        let (outcome, written) = self
            .storage
            .forget_fact(id, &homes, &reason, Utc::now(), record_at, &record)
            .await?;
        me_index::insert_scope_chain(&self.scope_tree, written.chain);
        // The record is written whatever the outcome; the reason only with a new tombstone.
        let stored_reason = if outcome == ForgetOutcome::Forgotten {
            reason_found
        } else {
            0
        };
        self.tally(record_found + stored_reason, 0).await;
        Ok((outcome, written.id))
    }

    /// Reverse the forget that retracted fact `id`: every fact that forget retracted and
    /// that still exists gets back the `t_expired` and `t_invalid` it had before, and loses
    /// its tombstone. A maintenance record names what was restored. This is a maintenance
    /// operation, never a model tool (ADR-0020 §8).
    ///
    /// # Errors
    ///
    /// `MemoryError::ReadOnly` on a read-only engine; `MemoryError::Storage` on a write
    /// failure, with nothing written; an `IndexInconsistent` error after the commit when the
    /// live vector index could not take a restored fact back (rebuild it, do not retry).
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use memory_engine::types::UnforgetOutcome;
    /// let engine = memory_engine::MemoryEngine::builder(4).build().unwrap();
    /// assert_eq!(engine.unforget_fact(7).await.unwrap(), UnforgetOutcome::NotFound);
    /// # });
    /// ```
    pub async fn unforget_fact(&self, id: i64) -> Result<UnforgetOutcome> {
        self.ensure_open()?;
        self.ensure_writable()?;
        self.storage.unforget_fact(id, Utc::now()).await
    }

    /// Erase `target`, the store's one hard delete: one fact, or everything a machine
    /// wrote (#1023). What it takes and leaves is
    /// [`Retraction::erase`](me_storage::Retraction::erase)'s contract; the in-memory graph
    /// drops the erased facts' nodes and edges after the commit. This is a maintenance
    /// operation, never a model tool (ADR-0020 §8), and backups keep their copies.
    ///
    /// A missing fact or a machine that wrote nothing deletes nothing, and the purge still
    /// runs, so the same call retries a purge that failed ([`EraseReport::purged`]).
    ///
    /// # Errors
    ///
    /// `MemoryError::ReadOnly` on a read-only engine; a `PolicyParameter` conflict for a
    /// machine name that is not a lowercase DNS label; a `PayloadTooLarge` conflict when
    /// the erasure record would exceed the event limit (erase by fact then);
    /// `MemoryError::Storage` on a write failure. Nothing is deleted on any error.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use memory_engine::types::{AddFactRequest, EraseTarget, FactType};
    /// let engine = memory_engine::MemoryEngine::builder(4).build().unwrap();
    /// let fingerprint = memory_engine::EmbeddingFingerprint::new("doc", "test", 4);
    /// let request = AddFactRequest {
    ///     content: "smoke test".into(),
    ///     fact_type: FactType::Semantic,
    ///     source_event_id: None,
    ///     scope: Some("unscoped:smoke".into()),
    ///     opts: None,
    /// };
    /// let id = engine
    ///     .add_fact_precomputed(&request, vec![0.1; 4], &fingerprint, None)
    ///     .await
    ///     .unwrap();
    /// let report = engine.erase(&EraseTarget::Machine("smoke".into())).await.unwrap();
    /// assert_eq!(report.erased_facts, vec![id]);
    /// assert!(engine.get_fact(id).await.is_err());
    /// # });
    /// ```
    pub async fn erase(&self, target: &EraseTarget) -> Result<EraseReport> {
        self.ensure_open()?;
        self.ensure_writable()?;
        let report = self.storage.erase(target, Utc::now()).await?;
        let mut graph = self.graph.write();
        for &fact in &report.erased_facts {
            graph.remove_edges_by_fact(fact);
            graph.remove_node(fact);
        }
        drop(graph);
        Ok(report)
    }
}
