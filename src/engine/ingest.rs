use chrono::Utc;

use crate::error::{MemoryError, Result};
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::scopes::ScopeStore;
use crate::traits::{EmbeddingProvider, PersistenceClassifier};
use crate::types::{AddFactOptions, BatchFactEntry, Fact, FactType, NewEvent, NewFact};

#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy;

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Ingest ---

    /// Append an event to the event log. Returns the assigned event id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on insert failure.
    pub fn ingest(&self, event: &NewEvent) -> Result<i64> {
        let conn = self.write_conn()?;
        EventStore::new(&conn, &self.upcaster_registry).insert(event)
    }

    /// Add a fact: compute embedding via `embedder`, compute blake3 content hash,
    /// and insert into the fact store. Returns the assigned fact id.
    ///
    /// Embedding is computed **before** acquiring the write lock, so slow
    /// embedding calls (network API) don't block readers.
    ///
    /// # Errors
    ///
    /// Returns errors from embedding computation, dimension validation, or DB insert.
    #[allow(clippy::too_many_arguments)]
    pub fn add_fact(
        &self,
        content: &str,
        fact_type: FactType,
        source_event_id: Option<i64>,
        embedder: &dyn EmbeddingProvider,
        scope: Option<&str>,
        opts: Option<&AddFactOptions>,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<i64> {
        // Embed OUTSIDE the write lock (potentially slow)
        let embedding = embedder.embed(content)?;
        let now = Utc::now();
        let opts = opts.cloned().unwrap_or_default();
        let base_importance = opts.importance.unwrap_or(0.5);
        let effective_created = opts.t_created.unwrap_or(now);
        let effective_last_accessed = opts.last_accessed.unwrap_or(now);

        // Classify OUTSIDE the write lock (potentially slow — LLM, I/O, etc.)
        // Uses scope_id=0 placeholder; classifiers should rely on content/type/importance/metadata.
        let is_pinned = match opts.pinned {
            Some(p) => p,
            None => classifier.is_some_and(|c| {
                let temp = Fact {
                    id: 0,
                    content: content.into(),
                    content_hash: String::new(),
                    embedding: embedding.clone(),
                    fact_type: fact_type.clone(),
                    t_created: effective_created,
                    t_expired: None,
                    t_valid: opts.t_valid,
                    t_invalid: opts.t_invalid,
                    source_event_id,
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
            }),
        };

        // Resolve scope + insert fact in a single write lock, then release
        #[cfg(feature = "ann")]
        let emb_copy = embedding.clone();

        let fact_id = {
            let conn = self.write_conn()?;
            let scope_id = match scope {
                Some(path) => self.ensure_scope_with_conn(&conn, path)?,
                None => 1, // root scope
            };

            let new_fact = NewFact {
                content: content.into(),
                content_hash: String::new(), // FactStore::insert computes this via blake3
                embedding,
                fact_type,
                t_created: effective_created,
                t_expired: None,
                t_valid: opts.t_valid,
                t_invalid: opts.t_invalid,
                source_event_id,
                scope_id,
                importance: opts.importance.unwrap_or(0.5),
                access_count: 0,
                last_accessed: effective_last_accessed,
                metadata: opts.metadata.unwrap_or_else(|| serde_json::json!({})),
                is_pinned,
            };

            FactStore::new(&conn, self.embed_dim).insert(&new_fact)?
        }; // DB lock released = committed

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            hnsw.notify_insert(fact_id, &emb_copy);
        }

        Ok(fact_id)
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
    /// - 1 SQLite transaction (savepoint) instead of N
    /// - Scope resolution inside savepoint for atomicity; `scope_tree`
    ///   cache deferred to after `RELEASE` (two-phase commit).
    ///
    /// # Errors
    ///
    /// Returns errors from batch embedding, dimension validation, or DB insert.
    pub fn add_facts_batch(
        &self,
        entries: &[BatchFactEntry],
        embedder: &dyn EmbeddingProvider,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<Vec<i64>> {
        if entries.is_empty() {
            return Ok(Vec::new());
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

        let prepared: Vec<_> = entries
            .iter()
            .zip(embeddings)
            .map(|(entry, embedding)| {
                let opts = entry.opts.clone().unwrap_or_default();
                let base_importance = opts.importance.unwrap_or(0.5);
                let effective_created = opts.t_created.unwrap_or(now);
                let effective_last_accessed = opts.last_accessed.unwrap_or(now);

                let is_pinned = match opts.pinned {
                    Some(p) => p,
                    None => classifier.is_some_and(|c| {
                        let temp = Fact {
                            id: 0,
                            content: entry.content.clone(),
                            content_hash: String::new(),
                            embedding: embedding.clone(),
                            fact_type: entry.fact_type.clone(),
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
                    }),
                };

                (
                    entry,
                    embedding,
                    opts,
                    is_pinned,
                    effective_created,
                    effective_last_accessed,
                )
            })
            .collect();

        // --- Phase 3: DB operations INSIDE the write lock ---
        #[cfg(feature = "ann")]
        let mut hnsw_pairs: Vec<(i64, Vec<f32>)> = Vec::with_capacity(entries.len());

        let fact_ids = {
            let conn = self.write_conn()?;
            conn.execute_batch("SAVEPOINT batch_insert")?;

            let result = (|| -> Result<(Vec<i64>, Vec<i64>)> {
                let scope_store = ScopeStore::new(&conn);
                let store = FactStore::new(&conn, self.embed_dim);

                // Resolve scopes INSIDE the savepoint so they roll back on
                // error. Deduplicate paths to avoid redundant DB lookups.
                let mut scope_cache: std::collections::HashMap<String, i64> =
                    std::collections::HashMap::new();
                let mut scope_ids = Vec::with_capacity(prepared.len());

                for (entry, ..) in &prepared {
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
                        fact_type: entry.fact_type.clone(),
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

                // Collect unique scope_ids for deferred cache update
                let unique_scope_ids: Vec<i64> = scope_cache.into_values().collect();

                Ok((ids, unique_scope_ids))
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
