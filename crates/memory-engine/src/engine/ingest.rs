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
        let (event, found) = self.scrubbed_event(event)?;
        let id = me_ingest::ingest(self.mem_ctx(), &event).await?;
        self.tally(found, 0).await;
        Ok(id)
    }

    /// Append an event under the scope `path`, creating the path in the event's own
    /// transaction: a failed append leaves no new scope node. `event.scope_id` is replaced
    /// by the path's leaf. Returns the assigned event id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only, a
    /// `PayloadTooLarge` conflict for an oversized payload, `MemoryError::Storage` on a
    /// write failure. Nothing is written on any error.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use memory_engine::types::{EventType, NewEvent};
    /// let engine = memory_engine::MemoryEngine::builder(4).build().unwrap();
    /// let event = NewEvent {
    ///     timestamp: chrono::Utc::now(),
    ///     event_type: EventType::MemoryOp,
    ///     payload: serde_json::json!({}),
    ///     source: "doc".into(),
    ///     session_id: None,
    ///     scope_id: 1,
    ///     origin_node_id: "doc".into(),
    ///     sequence_id: 0,
    ///     created_at: None,
    /// };
    /// engine.ingest_at("unscoped:laptop", &event).await.unwrap();
    /// assert!(engine.resolve_scope_path("unscoped:laptop").is_some());
    /// # });
    /// ```
    pub async fn ingest_at(&self, path: &str, event: &NewEvent) -> Result<i64> {
        let (event, found) = self.scrubbed_event(event)?;
        let id = me_ingest::ingest_at(self.mem_ctx(), &self.scope_tree, path, &event).await?;
        self.tally(found, 0).await;
        Ok(id)
    }

    /// Move fact `id` from scope `from` to the scope path `to` and append `record` under
    /// the scope path `record_at`, in one transaction that creates both paths. Only a
    /// live fact (not expired, valid now) still at `from` moves, so of two concurrent
    /// moves the second answers `NotFound`. Returns the record's id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` when no live fact `id` sits at `from`,
    /// `MemoryError::ReadOnly` on a read-only engine, a `PayloadTooLarge` conflict for an
    /// oversized record, `MemoryError::Storage` on a write failure. Nothing is written on
    /// any error.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use memory_engine::types::{EventType, NewEvent};
    /// let engine = memory_engine::MemoryEngine::builder(4).build().unwrap();
    /// let record = NewEvent {
    ///     timestamp: chrono::Utc::now(),
    ///     event_type: EventType::MemoryOp,
    ///     payload: serde_json::json!({}),
    ///     source: "doc".into(),
    ///     session_id: None,
    ///     scope_id: 1,
    ///     origin_node_id: "doc".into(),
    ///     sequence_id: 0,
    ///     created_at: None,
    /// };
    /// // No fact 7 sits at the root: nothing moves and nothing is created.
    /// let err = engine.move_fact(7, 1, "user:alice", "unscoped:laptop", &record).await;
    /// assert!(matches!(err, Err(memory_engine::MemoryError::NotFound(_))));
    /// assert_eq!(engine.resolve_scope_path("user:alice"), None);
    /// # });
    /// ```
    pub async fn move_fact(
        &self,
        id: i64,
        from: i64,
        to: &str,
        record_at: &str,
        record: &NewEvent,
    ) -> Result<i64> {
        let (record, found) = self.scrubbed_event(record)?;
        let written = me_ingest::move_fact(
            self.mem_ctx(),
            &self.scope_tree,
            id,
            from,
            to,
            record_at,
            &record,
        )
        .await?;
        self.tally(found, 0).await;
        Ok(written)
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
        let (req, found) = self.scrubbed_request(req)?;
        let id = me_ingest::add_fact(self.mem_ctx(), &self.scope_tree, &req, embedder, classifier)
            .await?;
        self.tally(found, 0).await;
        Ok(id)
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
        // #1024: the caller computed `embedding` from `req.content`, so that text must already
        // be clean; the metadata, which is not embedded, is scrubbed.
        let (scrubbed, found) = self.scrubbed_request(req)?;
        if scrubbed.content != req.content {
            crate::redact::require_clean(&req.content)?;
        }
        let id = me_ingest::add_fact_precomputed(
            self.mem_ctx(),
            &self.scope_tree,
            &scrubbed,
            embedding,
            declared,
            classifier,
        )
        .await?;
        self.tally(found, 0).await;
        Ok(id)
    }

    /// Add a fact now and queue its vector (#1021): the write never waits on an embedder.
    ///
    /// The fact is stored with no vector and one embedding-queue entry, in one
    /// transaction, and gets its ID at once. Full-text search finds it at once; vector
    /// search finds it once the embedding drain has given it a vector. After the commit
    /// the engine wakes [`embed_work_arrived`](Self::embed_work_arrived). It runs
    /// [`add_fact`](Self::add_fact)'s gates and validation, consults no classifier (the
    /// fact is pinned only when `opts.pinned` says so), and records no embedding
    /// identity: there is no vector yet.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only,
    /// `MemoryError::Conflict` if the request fails validation (importance out of
    /// range, oversized content or metadata), or any write error. The fact and its queue
    /// entry are written together or not at all.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine::{AddFactRequest, FactType, MemoryEngine, MemoryQuery};
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let engine = MemoryEngine::builder(4).build()?;
    /// let id = engine
    ///     .add_fact_deferred(&AddFactRequest {
    ///         content: "the deploy key rotates weekly".into(),
    ///         fact_type: FactType::Semantic,
    ///         source_event_id: None,
    ///         scope: None,
    ///         opts: None,
    ///     })
    ///     .await?;
    ///
    /// let found = engine
    ///     .execute_query(&MemoryQuery::new().text("deploy key"))
    ///     .await?;
    /// assert_eq!(found.results[0].fact.id, id);
    /// # Ok::<(), memory_engine::MemoryError>(())
    /// # }).unwrap();
    /// ```
    pub async fn add_fact_deferred(&self, req: &AddFactRequest) -> Result<i64> {
        let (req, found) = self.scrubbed_request(req)?;
        let id = me_ingest::add_fact_deferred(self.mem_ctx(), &self.scope_tree, &req).await?;
        self.embed_work.notify_one();
        self.tally(found, 0).await;
        Ok(id)
    }

    /// Wait until a deferred write ([`add_fact_deferred`](Self::add_fact_deferred)) has
    /// committed since the last wake: a drain with nothing due awaits this.
    ///
    /// One stored wake covers every write made while no drain was waiting.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine::{AddFactRequest, FactType, MemoryEngine};
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let engine = MemoryEngine::builder(4).build()?;
    /// let request = AddFactRequest {
    ///     content: "embed me later".into(),
    ///     fact_type: FactType::Semantic,
    ///     source_event_id: None,
    ///     scope: None,
    ///     opts: None,
    /// };
    /// engine.add_fact_deferred(&request).await?;
    /// // The write was stored before anyone waited: the wake is not lost.
    /// engine.embed_work_arrived().await;
    /// # Ok::<(), memory_engine::MemoryError>(())
    /// # }).unwrap();
    /// ```
    pub async fn embed_work_arrived(&self) {
        self.embed_work.notified().await;
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
    /// Returns `MemoryError::Conflict(ConflictError::Forgotten)` if a forget's tombstone
    /// blocks an entry's content (#1023); the whole batch rolls back.
    /// Returns errors from batch embedding, dimension validation, or DB insert.
    /// Returns `MemoryError::Internal` if the embedder returns a different
    /// number of embeddings than input entries.
    pub async fn add_facts_batch(
        &self,
        entries: &[AddFactRequest],
        embedder: Arc<dyn EmbeddingProvider>,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<Vec<i64>> {
        let mut scrubbed = Vec::with_capacity(entries.len());
        let mut found = 0;
        for entry in entries {
            let (entry, n) = self.scrubbed_request(entry)?;
            scrubbed.push(entry);
            found += n;
        }
        let ids = me_ingest::add_facts_batch(
            self.mem_ctx(),
            &self.scope_tree,
            &scrubbed,
            embedder,
            classifier,
        )
        .await?;
        self.tally(found, 0).await;
        Ok(ids)
    }

    /// Partial-success variant of [`add_facts_batch`](Self::add_facts_batch):
    /// returns a per-record result so a single invalid record no longer poisons
    /// its whole batch (#663).
    ///
    /// Each input entry maps positionally to one `Result<i64, MemoryError>`:
    /// `Ok(id)` if it was embedded and inserted, `Err(e)` if it was rejected by
    /// per-record validation (importance range, oversized content/metadata) or because a
    /// forget's tombstone blocks its content (`Conflict(Forgotten)`, #1023). The
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
        self.ensure_open()?;
        self.ensure_writable()?;
        // A record whose redaction fails, or that is oversized, fails alone, like any other
        // per-record rejection; the others go to me-ingest in their order.
        let prepared: Vec<Result<(AddFactRequest, u64)>> =
            entries.iter().map(|e| self.scrubbed_request(e)).collect();
        let valid: Vec<AddFactRequest> = prepared
            .iter()
            .filter_map(|p| p.as_ref().ok().map(|(req, _)| req.clone()))
            .collect();
        let mut stored = me_ingest::add_facts_batch_partial(
            self.mem_ctx(),
            &self.scope_tree,
            &valid,
            embedder,
            classifier,
        )
        .await?
        .into_iter();
        let mut found = 0;
        let mut results = Vec::with_capacity(entries.len());
        for p in prepared {
            results.push(match p {
                Err(e) => Err(e),
                Ok((_, n)) => {
                    let result = stored.next().ok_or_else(|| {
                        MemoryError::Internal(
                            "me-ingest returned fewer results than records".into(),
                        )
                    })?;
                    if result.is_ok() {
                        found += n;
                    }
                    result
                }
            });
        }
        self.tally(found, 0).await;
        Ok(results)
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

    fn request(content: &str) -> AddFactRequest {
        AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        }
    }

    /// Whether `embed_work_arrived` resolves at its first poll, i.e. a permit is stored.
    fn work_is_signalled(engine: &MemoryEngine) -> bool {
        use std::future::Future as _;
        let mut arrived = std::pin::pin!(engine.embed_work_arrived());
        arrived
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_ready()
    }

    #[tokio::test]
    async fn add_fact_deferred_needs_no_embedder() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let id = engine
            .add_fact_deferred(&request("the deploy key rotates weekly"))
            .await
            .unwrap();

        assert_eq!(engine.get_fact(id).await.unwrap().embedding, [0.0_f32; 0]);
        let by_text = engine
            .execute_query(&crate::MemoryQuery::new().text("deploy key"))
            .await
            .unwrap();
        let found: Vec<i64> = by_text.results.iter().map(|r| r.fact.id).collect();
        assert_eq!(found, [id]);
        let by_vector = engine
            .execute_query(&crate::MemoryQuery::new().embedding(vec![1.0; DIM]))
            .await
            .unwrap();
        assert!(by_vector.results.is_empty());
        assert!(
            engine
                .storage()
                .load_embedding_fingerprint()
                .await
                .unwrap()
                .is_none(),
            "no identity before the first vector"
        );
        assert!(work_is_signalled(&engine), "the write wakes the drain");
    }

    #[tokio::test]
    async fn add_fact_deferred_validates_like_add_fact() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let oversized = request(&"x".repeat(MAX_PAYLOAD_BYTES + 1));
        assert!(matches!(
            engine.add_fact_deferred(&oversized).await,
            Err(MemoryError::Conflict(ConflictError::PayloadTooLarge {
                kind: "fact content",
                ..
            }))
        ));
        let out_of_range = AddFactRequest {
            opts: Some(AddFactOptions {
                base_importance: Some(1.5),
                ..Default::default()
            }),
            ..request("fine words")
        };
        assert!(matches!(
            engine.add_fact_deferred(&out_of_range).await,
            Err(MemoryError::Conflict(ConflictError::PolicyParameter(_)))
        ));
        assert_eq!(engine.list_active_facts(None).await.unwrap().len(), 0);
        assert!(!work_is_signalled(&engine), "a refused write wakes nothing");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.db");
        MemoryEngine::builder(DIM)
            .path(&path)
            .build()
            .unwrap()
            .close()
            .await
            .unwrap();
        let read_only = MemoryEngine::builder(DIM)
            .path(&path)
            .read_only(true)
            .build()
            .unwrap();
        assert!(matches!(
            read_only.add_fact_deferred(&request("fine words")).await,
            Err(MemoryError::ReadOnly)
        ));
    }
}
