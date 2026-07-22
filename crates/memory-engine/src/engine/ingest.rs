use std::sync::Arc;

use crate::error::{MemoryError, Result};
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{AddFactRequest, EmbeddingFingerprint, NewEvent};

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Ingest ---

    /// Append an event to the event log. Returns the assigned event id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Storage` on insert failure.
    pub async fn ingest(&self, event: &NewEvent) -> Result<i64> {
        me_ingest::ingest(self.mem_ctx(), event).await
    }

    /// Add a fact: compute embedding via `embedder`, compute blake3 content hash,
    /// and insert into the fact store. Returns the assigned fact id.
    ///
    /// Embedding is computed **before** acquiring the write lock, so slow
    /// embedding calls (network API) don't block readers.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Conflict(ConflictError::PolicyParameter)` if
    /// `opts.base_importance` is set and outside `[0, 1]` (or non-finite); the
    /// request is rejected before any embedding, event, or fact is written.
    /// Returns errors from embedding computation, dimension validation, or DB insert.
    pub async fn add_fact(
        &self,
        req: &AddFactRequest,
        embedder: Arc<dyn EmbeddingProvider>,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<i64> {
        me_ingest::add_fact(self.mem_ctx(), &self.scope_tree, req, embedder, classifier).await
    }

    /// Add a fact with a caller-supplied **pre-computed** embedding.
    ///
    /// Unlike [`add_fact`](Self::add_fact), this performs no embedding (the vector is
    /// given) — the caller supplies the **declared** identity of the model that produced
    /// it (#615, §Design.3). That declared fingerprint is treated exactly like a live
    /// provider's: `record_if_absent`
    /// records it on a fresh store (so a precomputed-only workflow can bootstrap an
    /// identity) and compares it (full-tuple `Eq`) against the stored identity on a
    /// populated one, hard-rejecting a mismatch. This closes the same-dim foreign-vector
    /// hole the old `PassthroughEmbedder` sentinel left open — the caller can no longer
    /// slip a vector from a different model into the store's vector space unchecked.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingModelMismatch`] if `declared` disagrees with the
    /// store's recorded identity, `MemoryError::EmbeddingDimension` if `declared.dim`
    /// (the vector length) differs from the engine dimension, or any write error.
    pub async fn add_fact_precomputed(
        &self,
        req: &AddFactRequest,
        embedding: Vec<f32>,
        declared: &EmbeddingFingerprint,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<i64> {
        me_ingest::add_fact_precomputed(
            self.mem_ctx(),
            &self.scope_tree,
            req,
            embedding,
            declared,
            classifier,
        )
        .await
    }

    /// Add multiple facts atomically: batch-embed all texts in a single call,
    /// classify outside the lock, then insert all facts in one transaction.
    ///
    /// Returns all assigned fact IDs on success. On any insert failure the
    /// entire batch is rolled back (all-or-nothing).
    ///
    /// # Performance
    ///
    /// - 1 embedding call (batch) instead of N
    /// - 1 `SQLite` transaction (savepoint) instead of N
    /// - Scope resolution inside savepoint for atomicity; `scope_tree`
    ///   cache deferred to after `RELEASE` (two-phase commit).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Conflict(ConflictError::PolicyParameter)` if any
    /// entry's `opts.base_importance` is set and outside `[0, 1]` (or non-finite);
    /// the whole batch is rejected up front, so no entry is embedded or written.
    /// Returns errors from batch embedding, dimension validation, or DB insert.
    /// Returns `MemoryError::Internal` if the embedder returns a different
    /// number of embeddings than input entries.
    pub async fn add_facts_batch(
        &self,
        entries: &[AddFactRequest],
        embedder: Arc<dyn EmbeddingProvider>,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<Vec<i64>> {
        me_ingest::add_facts_batch(
            self.mem_ctx(),
            &self.scope_tree,
            entries,
            embedder,
            classifier,
        )
        .await
    }

    /// Partial-success variant of [`add_facts_batch`](Self::add_facts_batch):
    /// returns a per-record result so a single invalid record no longer poisons
    /// its whole batch (#663).
    ///
    /// Each input entry maps positionally to one `Result<i64, MemoryError>`:
    /// `Ok(id)` if it was embedded and inserted, `Err(e)` if it was rejected by
    /// per-record validation (importance range, oversized content/metadata). The
    /// **valid** partition is embedded and inserted in one atomic batch, so the
    /// invalid records are simply skipped rather than failing their neighbours.
    ///
    /// # Errors
    ///
    /// Returns an **outer** `Err` for batch-level failures that prevent the valid
    /// partition from being persisted at all: a consumer-`embedder` failure, an
    /// `embed_batch` count mismatch, a rollback of the atomic insert itself
    /// (a rare DB-constraint failure rolls the whole valid set back together —
    /// per-record isolation covers *validation* rejection, the realistic poison,
    /// not a mid-insert constraint violation), or a backend that returns a
    /// different number of ids than valid entries (a contract violation). Every
    /// internal-invariant breach surfaces as the outer `Err`; the returned `Vec`
    /// carries **only** per-record validation failures, never an outer-`Err` cause.
    pub async fn add_facts_batch_partial(
        &self,
        entries: &[AddFactRequest],
        embedder: Arc<dyn EmbeddingProvider>,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<Vec<std::result::Result<i64, MemoryError>>> {
        me_ingest::add_facts_batch_partial(
            self.mem_ctx(),
            &self.scope_tree,
            entries,
            embedder,
            classifier,
        )
        .await
    }
}

// `Utc::now()` is called directly by the (unchanged) test bodies below; the
// production `Utc::now()` calls moved into me-ingest along with the extracted
// bodies, so this import is test-only now — `#[cfg(test)]`-gated to avoid an
// unused-import warning in the plain (non-test) build.
#[cfg(test)]
use chrono::Utc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ConflictError;
    use crate::limits::MAX_PAYLOAD_BYTES;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactOptions, AddFactRequest, EventType, FactType};

    const DIM: usize = 4;

    /// An oversized event payload is rejected by `ingest` before it touches the
    /// write path.
    #[tokio::test]
    async fn ingest_rejects_oversized_payload() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let big = "x".repeat(MAX_PAYLOAD_BYTES + 10);
        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::Value::String(big),
            source: "test".into(),
            session_id: None,
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };
        let err = engine.ingest(&event).await.unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(ConflictError::PayloadTooLarge {
                kind: "event payload",
                ..
            })
        ));
    }

    /// An oversized fact `metadata` is rejected by `add_fact`, and a normal one
    /// is accepted (guard does not regress the happy path).
    #[tokio::test]
    async fn add_fact_rejects_oversized_metadata() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let big = "x".repeat(MAX_PAYLOAD_BYTES + 10);
        let req = AddFactRequest {
            content: "fact".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: Some(AddFactOptions {
                metadata: Some(serde_json::json!({ "blob": big })),
                ..Default::default()
            }),
        };
        let err = engine
            .add_fact(
                &req,
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(ConflictError::PayloadTooLarge {
                kind: "fact metadata",
                ..
            })
        ));

        // Small metadata is accepted.
        let ok_req = AddFactRequest {
            content: "fact".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: Some(AddFactOptions {
                metadata: Some(serde_json::json!({ "k": "v" })),
                ..Default::default()
            }),
        };
        assert!(
            engine
                .add_fact(
                    &ok_req,
                    std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                        as std::sync::Arc<dyn EmbeddingProvider>,
                    None
                )
                .await
                .is_ok()
        );
    }

    /// An oversized fact `content` body is rejected by `add_fact` — the content
    /// String is the larger unbounded vector, guarded alongside metadata.
    #[tokio::test]
    async fn add_fact_rejects_oversized_content() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let req = AddFactRequest {
            content: "x".repeat(MAX_PAYLOAD_BYTES + 1),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        let err = engine
            .add_fact(
                &req,
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(ConflictError::PayloadTooLarge {
                kind: "fact content",
                ..
            })
        ));
    }

    /// #663: one oversized-content record is skipped **per-record** by
    /// `add_facts_batch_partial`; its valid batch-mates are still ingested
    /// (the all-or-nothing `add_facts_batch` would have lost all of them).
    #[tokio::test]
    async fn add_facts_batch_partial_isolates_invalid_record() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let mk = |content: String| AddFactRequest {
            content,
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        let entries = vec![
            mk("good-1".to_string()),
            mk("x".repeat(MAX_PAYLOAD_BYTES + 1)), // poison: oversized content
            mk("good-2".to_string()),
            mk("good-3".to_string()),
        ];

        let results = engine
            .add_facts_batch_partial(
                &entries,
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 4, "one result per input, positionally");
        assert!(results[0].is_ok(), "good-1 ingested");
        match &results[1] {
            Err(MemoryError::Conflict(ConflictError::PayloadTooLarge { kind, .. })) => {
                assert_eq!(*kind, "fact content");
            }
            other => panic!("expected per-record PayloadTooLarge at [1], got {other:?}"),
        }
        assert!(results[2].is_ok(), "good-2 ingested");
        assert!(results[3].is_ok(), "good-3 ingested");

        // The 3 valid neighbours are persisted — the bad record did NOT poison them.
        let active = engine.list_active_facts(None).await.unwrap();
        assert_eq!(active.len(), 3, "3 valid records survived the poison");
    }

    /// #663 edge case: an all-invalid batch returns all `Err` and persists nothing
    /// (no spurious insert, no outer error).
    #[tokio::test]
    async fn add_facts_batch_partial_all_invalid_persists_nothing() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let entries = vec![AddFactRequest {
            content: "x".repeat(MAX_PAYLOAD_BYTES + 1),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        }];

        let results = engine
            .add_facts_batch_partial(
                &entries,
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].is_err(), "the only record is rejected");
        assert_eq!(
            engine.list_active_facts(None).await.unwrap().len(),
            0,
            "nothing persisted"
        );
    }
}
