use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::error::{ConflictError, MemoryError, Result};
use crate::limits::{check_json_size, check_str_size};
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{AddFactRequest, EmbeddingFingerprint, Fact, NewEvent, NewFact};

use super::{MemoryEngine, spawn_join_err};

impl MemoryEngine {
    // --- Public API: Ingest ---

    /// Append an event to the event log. Returns the assigned event id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Database` on insert failure.
    pub async fn ingest(&self, event: &NewEvent) -> Result<i64> {
        check_json_size(&event.payload, "event payload")?;
        self.storage.insert_event(event).await
    }

    /// Validate a caller-supplied `importance` override.
    ///
    /// `AddFactOptions::importance` is documented as living in `[0, 1]`. We
    /// reject out-of-range values (and non-finite ones such as `NaN`/`±inf`)
    /// loudly rather than clamping silently, mirroring the typed
    /// `Conflict(PolicyParameter)` errors raised elsewhere for out-of-range
    /// policy parameters. `None` is always valid (the engine default is used).
    pub(crate) fn validate_importance(importance: Option<f64>) -> Result<()> {
        if let Some(v) = importance
            && (!v.is_finite() || !(0.0..=1.0).contains(&v))
        {
            return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!("importance must be in [0, 1], got {v}"),
            )));
        }
        Ok(())
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
    /// `opts.importance` is set and outside `[0, 1]` (or non-finite); the
    /// request is rejected before any embedding, event, or fact is written.
    /// Returns errors from embedding computation, dimension validation, or DB insert.
    pub async fn add_fact(
        &self,
        req: &AddFactRequest,
        embedder: Arc<dyn EmbeddingProvider>,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
    ) -> Result<i64> {
        Self::validate_add_fact_request(req)?;
        // Embed off the async executor (the provider call may be a blocking HTTP
        // round-trip; running it inline would park the runtime thread, and a
        // `reqwest::blocking` provider would panic with a nested-runtime error).
        let content = req.content.clone();
        let provider = Arc::clone(&embedder);
        let embedding = tokio::task::spawn_blocking(move || provider.embed(&content))
            .await
            .map_err(spawn_join_err)??;
        // Record the provider's identity on the first embedding write (#613) — which,
        // under #614, also rejects a fingerprint that disagrees with the stored one.
        // The atomic insert records-if-absent inside its transaction.
        let fingerprint = embedder.fingerprint();
        self.insert_fact_with_embedding(req, embedding, classifier, fingerprint)
            .await
    }

    /// Add a fact with a caller-supplied **pre-computed** embedding.
    ///
    /// Unlike [`add_fact`](Self::add_fact), this performs no embedding (the vector is
    /// given) — the caller supplies the **declared** identity of the model that produced
    /// it (#615, §Design.3). That declared fingerprint is treated exactly like a live
    /// provider's: [`record_if_absent`](crate::store::embedding_meta::record_if_absent)
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
        Self::validate_add_fact_request(req)?;
        // The caller's declared fingerprint is treated exactly like a live
        // provider's: the atomic insert records-if-absent (and #614-rejects a
        // mismatch) inside its transaction.
        self.insert_fact_with_embedding(req, embedding, classifier, declared.clone())
            .await
    }

    /// Validate an [`AddFactRequest`] before any expensive work (embedding, metadata
    /// clone, write). Rejects an out-of-range importance and oversized content/metadata
    /// (issue #572 / L10) so a hostile or malformed request fails fast and cheap.
    fn validate_add_fact_request(req: &AddFactRequest) -> Result<()> {
        Self::validate_importance(req.opts.as_ref().and_then(|o| o.importance))?;
        check_str_size(&req.content, "fact content")?;
        if let Some(metadata) = req.opts.as_ref().and_then(|o| o.metadata.as_ref()) {
            check_json_size(metadata, "fact metadata")?;
        }
        Ok(())
    }

    /// Shared body of [`add_fact`](Self::add_fact) and
    /// [`add_fact_precomputed`](Self::add_fact_precomputed): classify, resolve scope, and
    /// insert the fact with its (already-obtained) `embedding` in a single write
    /// transaction. `stamp_identity` runs inside that transaction *before* the insert —
    /// recording-or-comparing the identity (from the live provider in `add_fact`, or the
    /// caller's declared fingerprint in `add_fact_precomputed`) — so a vector is never
    /// committed without an established, matching identity (the #614 silent-corruption
    /// landmine).
    async fn insert_fact_with_embedding(
        &self,
        req: &AddFactRequest,
        embedding: Vec<f32>,
        classifier: Option<Arc<dyn PersistenceClassifier>>,
        fingerprint: EmbeddingFingerprint,
    ) -> Result<i64> {
        let now = Utc::now();
        let opts = req.opts.clone().unwrap_or_default();

        let base_importance = opts.importance.unwrap_or(0.5);
        let effective_created = opts.t_created.unwrap_or(now);
        let effective_last_accessed = opts.last_accessed.unwrap_or(now);

        // Classify off the async executor (a classifier may be a blocking LLM/HTTP
        // call). scope_id=0 placeholder; classifiers rely on content/type/importance/metadata.
        let is_pinned = match opts.pinned {
            Some(p) => p,
            None => match classifier {
                None => false,
                Some(c) => {
                    let temp = Fact {
                        id: 0,
                        content: req.content.clone(),
                        content_hash: String::new(),
                        embedding: embedding.clone(),
                        fact_type: req.fact_type,
                        t_created: effective_created,
                        t_expired: None,
                        t_valid: opts.t_valid,
                        t_invalid: opts.t_invalid,
                        source_event_id: req.source_event_id,
                        importance: base_importance,
                        access_count: 0,
                        last_accessed: effective_last_accessed,
                        metadata: opts
                            .metadata
                            .clone()
                            .unwrap_or_else(|| serde_json::json!({})),
                        scope_id: 0,
                        is_pinned: false,
                        importance_score: base_importance,
                        surfaced_at: None,
                    };
                    tokio::task::spawn_blocking(move || c.should_pin(&temp))
                        .await
                        .map_err(spawn_join_err)?
                }
            },
        };

        // Resolve scope via the port (autocommit-separate by design, as before),
        // caching the full chain into the in-memory tree.
        let scope_id = match &req.scope {
            Some(path) => {
                let id = self.storage.ensure_scope_path(path).await?;
                self.cache_scope_chain(id).await?;
                id
            }
            None => 1, // root scope
        };

        let new_fact = NewFact {
            content: req.content.clone(),
            content_hash: String::new(), // FactStore::insert computes this via blake3
            embedding,
            fact_type: req.fact_type,
            t_created: effective_created,
            t_expired: None,
            t_valid: opts.t_valid,
            t_invalid: opts.t_invalid,
            source_event_id: req.source_event_id,
            scope_id,
            importance: opts.importance.unwrap_or(0.5),
            access_count: 0,
            last_accessed: effective_last_accessed,
            metadata: opts.metadata.unwrap_or_else(|| serde_json::json!({})),
            is_pinned,
        };

        // Atomic first-write below the seam: record-or-verify the embedding identity
        // (#613/#614) and insert the fact in one transaction; the backend fires the
        // HNSW notify post-commit internally (Stage B). `Ok ⟹ committed`.
        self.storage
            .insert_fact_atomic(&new_fact, &fingerprint, self.embed_dim)
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
    /// entry's `opts.importance` is set and outside `[0, 1]` (or non-finite);
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
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        // Validate every entry up front: the batch is all-or-nothing, so a
        // single invalid importance or oversized content/metadata rejects the
        // whole call before any entry is embedded or persisted.
        for entry in entries {
            Self::validate_importance(entry.opts.as_ref().and_then(|o| o.importance))?;
            check_str_size(&entry.content, "fact content")?;
            if let Some(metadata) = entry.opts.as_ref().and_then(|o| o.metadata.as_ref()) {
                check_json_size(metadata, "fact metadata")?;
            }
        }

        // --- Phase 1: Batch embed off the executor (one blocking call) ---
        let texts: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
        let provider = Arc::clone(&embedder);
        let embeddings = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            provider.embed_batch(&refs)
        })
        .await
        .map_err(spawn_join_err)??;

        if embeddings.len() != entries.len() {
            return Err(MemoryError::Internal(format!(
                "embed_batch returned {} embeddings for {} entries",
                embeddings.len(),
                entries.len()
            )));
        }

        // --- Phase 2: Classify + prepare (auto-pin off the executor) ---
        let now = Utc::now();
        let pins = self
            .compute_batch_pins(entries, &embeddings, classifier, now)
            .await?;

        // --- Phase 3: build NewFacts (+ scope paths) for the atomic batch insert ---
        // `scope_id` here is a placeholder; `insert_facts_batch_atomic` resolves
        // each `scope_paths[i]` inside its savepoint and patches the real id.
        let mut facts = Vec::with_capacity(entries.len());
        let mut scope_paths = Vec::with_capacity(entries.len());
        for ((entry, embedding), &is_pinned) in entries.iter().zip(embeddings).zip(&pins) {
            let opts = entry.opts.clone().unwrap_or_default();
            facts.push(NewFact {
                content: entry.content.clone(),
                content_hash: String::new(), // FactStore::insert computes via blake3
                embedding,
                fact_type: entry.fact_type,
                t_created: opts.t_created.unwrap_or(now),
                t_expired: None,
                t_valid: opts.t_valid,
                t_invalid: opts.t_invalid,
                source_event_id: entry.source_event_id,
                scope_id: 1, // placeholder — patched from scope_paths below the seam
                importance: opts.importance.unwrap_or(0.5),
                access_count: 0,
                last_accessed: opts.last_accessed.unwrap_or(now),
                metadata: opts.metadata.unwrap_or_else(|| serde_json::json!({})),
                is_pinned,
            });
            scope_paths.push(entry.scope.clone());
        }

        // --- Phase 4: atomic batch insert below the seam (one savepoint; the
        // backend fires HNSW notify post-commit internally, Stage B). Returns the
        // new ids + the unique scope ids to mirror into the in-memory tree. ---
        let fingerprint = embedder.fingerprint();
        let (ids, scope_ids_to_cache) = self
            .storage
            .insert_facts_batch_atomic(&facts, &scope_paths, &fingerprint, self.embed_dim)
            .await?;

        // --- Phase 5: deferred scope_tree cache (post-commit, leaf nodes only —
        // matching the prior batch behavior). Fetch nodes via the port FIRST,
        // then take the write lock so no guard is held across `.await`. ---
        if !scope_ids_to_cache.is_empty() {
            let mut nodes = Vec::with_capacity(scope_ids_to_cache.len());
            for sid in scope_ids_to_cache {
                if let Ok(node) = self.storage.get_scope(sid).await {
                    nodes.push(node);
                }
            }
            let mut tree = self.scope_tree.write();
            for node in nodes {
                tree.insert(node);
            }
        }

        Ok(ids)
    }

    /// Compute the per-entry auto-pin decision for a batch, offloading each
    /// classifier call (a possibly-blocking LLM/HTTP round-trip) to the blocking
    /// pool so it never parks the async executor. Mirrors the prior
    /// `prepare_batch_entries` pin logic exactly.
    async fn compute_batch_pins(
        &self,
        entries: &[AddFactRequest],
        embeddings: &[Vec<f32>],
        classifier: Option<Arc<dyn PersistenceClassifier>>,
        now: DateTime<Utc>,
    ) -> Result<Vec<bool>> {
        let mut pins = Vec::with_capacity(entries.len());
        for (entry, embedding) in entries.iter().zip(embeddings) {
            let opts = entry.opts.clone().unwrap_or_default();
            let is_pinned = match opts.pinned {
                Some(p) => p,
                None => match &classifier {
                    None => false,
                    Some(c) => {
                        let base_importance = opts.importance.unwrap_or(0.5);
                        let temp = Fact {
                            id: 0,
                            content: entry.content.clone(),
                            content_hash: String::new(),
                            embedding: embedding.clone(),
                            fact_type: entry.fact_type,
                            t_created: opts.t_created.unwrap_or(now),
                            t_expired: None,
                            t_valid: opts.t_valid,
                            t_invalid: opts.t_invalid,
                            source_event_id: entry.source_event_id,
                            importance: base_importance,
                            access_count: 0,
                            last_accessed: opts.last_accessed.unwrap_or(now),
                            metadata: opts
                                .metadata
                                .clone()
                                .unwrap_or_else(|| serde_json::json!({})),
                            scope_id: 0,
                            is_pinned: false,
                            importance_score: base_importance,
                            surfaced_at: None,
                        };
                        let c = Arc::clone(c);
                        tokio::task::spawn_blocking(move || c.should_pin(&temp))
                            .await
                            .map_err(spawn_join_err)?
                    }
                },
            };
            pins.push(is_pinned);
        }
        Ok(pins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ConflictError;
    use crate::limits::MAX_PAYLOAD_BYTES;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactOptions, AddFactRequest, EventType, FactType};

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

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
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
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
                    std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
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
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
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
}
