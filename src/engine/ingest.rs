use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::{ConflictError, MemoryError, Result};
use crate::limits::{check_json_size, check_str_size};
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::scopes::ScopeStore;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{AddFactOptions, AddFactRequest, Fact, NewEvent, NewFact};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::MemoryEngine;

/// Prepared batch entry: a borrowed request plus the insert fields computed
/// outside the write lock by [`MemoryEngine::prepare_batch_entries`]
/// (aliased to keep `add_facts_batch` free of `clippy::type_complexity`).
type PreparedBatchEntry<'a> = (
    &'a AddFactRequest,
    Vec<f32>,
    AddFactOptions,
    bool,
    DateTime<Utc>,
    DateTime<Utc>,
);

impl MemoryEngine {
    // --- Public API: Ingest ---

    /// Append an event to the event log. Returns the assigned event id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Database` on insert failure.
    pub fn ingest(&self, event: &NewEvent) -> Result<i64> {
        check_json_size(&event.payload, "event payload")?;
        let conn = self.write_conn()?;
        EventStore::new(&conn, &self.upcaster_registry).insert(event)
    }

    /// Validate a caller-supplied `importance` override.
    ///
    /// `AddFactOptions::importance` is documented as living in `[0, 1]`. We
    /// reject out-of-range values (and non-finite ones such as `NaN`/`±inf`)
    /// loudly rather than clamping silently, mirroring the typed
    /// `Conflict(PolicyParameter)` errors raised elsewhere for out-of-range
    /// policy parameters. `None` is always valid (the engine default is used).
    pub(crate) fn validate_importance(importance: Option<f64>) -> Result<()> {
        if let Some(v) = importance {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                    format!("importance must be in [0, 1], got {v}"),
                )));
            }
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
    pub fn add_fact(
        &self,
        req: &AddFactRequest,
        embedder: &dyn EmbeddingProvider,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<i64> {
        Self::validate_add_fact_request(req)?;
        // Embed OUTSIDE the write lock (potentially slow)
        let embedding = embedder.embed(&req.content)?;
        // Record the provider's identity on the first embedding write (#613) — which,
        // under #614, also rejects a fingerprint that disagrees with the stored one.
        self.insert_fact_with_embedding(req, embedding, classifier, |conn| {
            self.record_embedding_identity(conn, embedder)
        })
    }

    /// Add a fact with a caller-supplied **pre-computed** embedding.
    ///
    /// Unlike [`add_fact`](Self::add_fact), this performs no embedding (the vector is
    /// given) and does **not** record a model identity: a pre-computed vector carries no
    /// real provider fingerprint (declaring its model is #615). Instead it requires the
    /// store to **already** have a recorded identity
    /// ([`require_present`](crate::store::embedding_meta::require_present)) and inserts
    /// the dim-checked vector into that established space. On a fresh, never-embedded
    /// store this errors — a pre-computed write cannot establish identity.
    ///
    /// This mirrors the `promote` and cycle `AddFact`/`Synthesize` pre-computed paths
    /// (#613) and is the path the MCP `memory_add_fact` precomputed branch uses, so #614
    /// enforcement does not compare the caller's vector against a sentinel fingerprint.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Internal` if the store has no recorded embedding identity,
    /// `MemoryError::EmbeddingDimension` if the vector length is wrong, or any write
    /// error.
    pub fn add_fact_precomputed(
        &self,
        req: &AddFactRequest,
        embedding: Vec<f32>,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<i64> {
        Self::validate_add_fact_request(req)?;
        self.insert_fact_with_embedding(req, embedding, classifier, |conn| {
            crate::store::embedding_meta::require_present(conn)
        })
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
    /// either recording the provider identity (`add_fact`) or asserting one already
    /// exists (`add_fact_precomputed`) — so a vector is never committed without an
    /// established identity (the #614 silent-corruption landmine).
    // The `conn` write lock is held until the block's end: `tx` borrows it and must
    // commit before it drops, so clippy's nursery suggestion to drop `conn` right after
    // `unchecked_transaction()` misses that transitive borrow and would not compile.
    #[allow(clippy::significant_drop_tightening)]
    fn insert_fact_with_embedding(
        &self,
        req: &AddFactRequest,
        embedding: Vec<f32>,
        classifier: Option<&dyn PersistenceClassifier>,
        stamp_identity: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<i64> {
        let now = Utc::now();
        let opts = req.opts.clone().unwrap_or_default();

        let base_importance = opts.importance.unwrap_or(0.5);
        let effective_created = opts.t_created.unwrap_or(now);
        let effective_last_accessed = opts.last_accessed.unwrap_or(now);

        // Classify OUTSIDE the write lock (potentially slow — LLM, I/O, etc.)
        // Uses scope_id=0 placeholder; classifiers should rely on content/type/importance/metadata.
        let is_pinned = opts.pinned.unwrap_or_else(|| {
            classifier.is_some_and(|c| {
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
                c.should_pin(&temp)
            })
        });

        // Resolve scope + insert fact in a single write lock, then release
        #[cfg(feature = "ann")]
        let emb_copy = embedding.clone();

        let fact_id = {
            let conn = self.write_conn()?;
            let scope_id = match &req.scope {
                Some(path) => self.ensure_scope_with_conn(&conn, path)?,
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

            // Atomic first-write: stamp/verify the embedding identity (#613/#614, ADR
            // 0015 §2) and insert the fact in one transaction, so a vector is never
            // committed without an established identity (the #614 silent-corruption
            // landmine). The insert was previously a bare autocommit; a second autocommit
            // statement for the identity would let a crash between them orphan the vector.
            // Scope creation above stays autocommit (unchanged): an orphan scope on
            // rollback is a pre-existing, benign outcome, and the scope row + its
            // scope_tree cache entry commit together, independent of the fact — so no
            // cache desync is introduced.
            let tx = conn.unchecked_transaction()?;
            stamp_identity(&tx)?;
            let id = FactStore::new(&tx, self.embed_dim).insert(&new_fact)?;
            tx.commit()?;
            id
        }; // DB lock released = committed

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            hnsw.notify_insert(fact_id, &emb_copy);
        }

        Ok(fact_id)
    }

    /// Classify and prepare each entry (importance, timestamps, auto-pin
    /// decision) outside the write lock — the Phase-2 helper for
    /// [`add_facts_batch`](Self::add_facts_batch).
    fn prepare_batch_entries<'a>(
        entries: &'a [AddFactRequest],
        embeddings: Vec<Vec<f32>>,
        classifier: Option<&dyn PersistenceClassifier>,
        now: DateTime<Utc>,
    ) -> Vec<PreparedBatchEntry<'a>> {
        entries
            .iter()
            .zip(embeddings)
            .map(|(entry, embedding)| {
                let opts = entry.opts.clone().unwrap_or_default();
                let base_importance = opts.importance.unwrap_or(0.5);
                let effective_created = opts.t_created.unwrap_or(now);
                let effective_last_accessed = opts.last_accessed.unwrap_or(now);

                let is_pinned = opts.pinned.unwrap_or_else(|| {
                    classifier.is_some_and(|c| {
                        let temp = Fact {
                            id: 0,
                            content: entry.content.clone(),
                            content_hash: String::new(),
                            embedding: embedding.clone(),
                            fact_type: entry.fact_type,
                            t_created: effective_created,
                            t_expired: None,
                            t_valid: opts.t_valid,
                            t_invalid: opts.t_invalid,
                            source_event_id: entry.source_event_id,
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
                        c.should_pin(&temp)
                    })
                });

                (
                    entry,
                    embedding,
                    opts,
                    is_pinned,
                    effective_created,
                    effective_last_accessed,
                )
            })
            .collect()
    }

    /// Resolve (and dedupe) the scope id for each prepared entry inside the
    /// savepoint. Returns the per-entry scope ids and the unique set to cache.
    fn resolve_batch_scopes(
        scope_store: &ScopeStore,
        prepared: &[PreparedBatchEntry<'_>],
    ) -> Result<(Vec<i64>, Vec<i64>)> {
        let mut scope_cache: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        let mut scope_ids = Vec::with_capacity(prepared.len());
        for (entry, ..) in prepared {
            let scope_id = match &entry.scope {
                Some(path) => {
                    if let Some(&cached) = scope_cache.get(path) {
                        cached
                    } else {
                        let id = scope_store.ensure_path(path)?;
                        scope_cache.insert(path.clone(), id);
                        id
                    }
                }
                None => 1, // root scope
            };
            scope_ids.push(scope_id);
        }
        let unique_scope_ids = scope_cache.into_values().collect();
        Ok((scope_ids, unique_scope_ids))
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
    // `conn` (write lock) spans the whole savepoint transaction via FactStore/
    // ScopeStore wrappers that borrow it; clippy misses the transitive borrow.
    #[allow(clippy::significant_drop_tightening)]
    pub fn add_facts_batch(
        &self,
        entries: &[AddFactRequest],
        embedder: &dyn EmbeddingProvider,
        classifier: Option<&dyn PersistenceClassifier>,
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

        // --- Phase 1: Batch embed OUTSIDE the write lock ---
        let texts: Vec<&str> = entries.iter().map(|e| e.content.as_str()).collect();
        let embeddings = embedder.embed_batch(&texts)?;

        if embeddings.len() != entries.len() {
            return Err(MemoryError::Internal(format!(
                "embed_batch returned {} embeddings for {} entries",
                embeddings.len(),
                entries.len()
            )));
        }

        // --- Phase 2: Classify + prepare OUTSIDE the write lock ---
        let now = Utc::now();
        let prepared = Self::prepare_batch_entries(entries, embeddings, classifier, now);

        // --- Phase 3: DB operations INSIDE the write lock ---
        #[cfg(feature = "ann")]
        let mut hnsw_pairs: Vec<(i64, Vec<f32>)> = Vec::with_capacity(entries.len());

        let fact_ids = {
            let conn = self.write_conn()?;
            conn.execute_batch("SAVEPOINT batch_insert")?;

            let result = (|| -> Result<(Vec<i64>, Vec<i64>)> {
                let scope_store = ScopeStore::new(&conn);
                let store = FactStore::new(&conn, self.embed_dim);

                // Record the embedding identity on first write (#613), inside the
                // savepoint so it commits atomically with the batch (write-once;
                // a no-op on every subsequent batch).
                self.record_embedding_identity(&conn, embedder)?;

                // Resolve scopes INSIDE the savepoint so they roll back on error.
                let (scope_ids, scope_ids_to_cache) =
                    Self::resolve_batch_scopes(&scope_store, &prepared)?;

                let mut ids = Vec::with_capacity(prepared.len());

                for (
                    i,
                    (entry, embedding, opts, is_pinned, effective_created, effective_last_accessed),
                ) in prepared.iter().enumerate()
                {
                    let new_fact = NewFact {
                        content: entry.content.clone(),
                        content_hash: String::new(), // FactStore::insert computes via blake3
                        embedding: embedding.clone(),
                        fact_type: entry.fact_type,
                        t_created: *effective_created,
                        t_expired: None,
                        t_valid: opts.t_valid,
                        t_invalid: opts.t_invalid,
                        source_event_id: entry.source_event_id,
                        scope_id: scope_ids[i],
                        importance: opts.importance.unwrap_or(0.5),
                        access_count: 0,
                        last_accessed: *effective_last_accessed,
                        metadata: opts
                            .metadata
                            .clone()
                            .unwrap_or_else(|| serde_json::json!({})),
                        is_pinned: *is_pinned,
                    };

                    let fact_id = store.insert(&new_fact)?;

                    #[cfg(feature = "ann")]
                    hnsw_pairs.push((fact_id, embedding.clone()));

                    ids.push(fact_id);
                }

                Ok((ids, scope_ids_to_cache))
            })();

            match result {
                Ok((ids, scope_ids_to_cache)) => {
                    conn.execute_batch("RELEASE batch_insert")?;

                    // Deferred scope_tree cache update — only after successful
                    // commit. Prevents cache desync on rollback.
                    let scope_store = ScopeStore::new(&conn);
                    let mut tree = self.scope_tree.write();
                    for sid in scope_ids_to_cache {
                        if let Ok(node) = scope_store.get(sid) {
                            tree.insert(node);
                        }
                    }
                    drop(tree);

                    ids
                }
                Err(e) => {
                    // ROLLBACK TO restores savepoint but keeps it open —
                    // RELEASE closes it, leaving the connection clean.
                    let _ = conn.execute_batch("ROLLBACK TO batch_insert");
                    let _ = conn.execute_batch("RELEASE batch_insert");
                    return Err(e);
                }
            }
        }; // write lock released

        // --- Phase 4: HNSW notification AFTER lock (success only) ---
        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for (fact_id, emb) in &hnsw_pairs {
                hnsw.notify_insert(*fact_id, emb);
            }
        }

        Ok(fact_ids)
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
    #[test]
    fn ingest_rejects_oversized_payload() {
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
        let err = engine.ingest(&event).unwrap_err();
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
    #[test]
    fn add_fact_rejects_oversized_metadata() {
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
        let err = engine.add_fact(&req, &FakeEmbed, None).unwrap_err();
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
        assert!(engine.add_fact(&ok_req, &FakeEmbed, None).is_ok());
    }

    /// An oversized fact `content` body is rejected by `add_fact` — the content
    /// String is the larger unbounded vector, guarded alongside metadata.
    #[test]
    fn add_fact_rejects_oversized_content() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let req = AddFactRequest {
            content: "x".repeat(MAX_PAYLOAD_BYTES + 1),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        let err = engine.add_fact(&req, &FakeEmbed, None).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Conflict(ConflictError::PayloadTooLarge {
                kind: "fact content",
                ..
            })
        ));
    }
}
