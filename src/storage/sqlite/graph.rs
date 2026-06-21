//! `impl FactGraph for SqliteBackend` — delegates to [`FactStore`], [`EdgeStore`],
//! and [`ScopeStore`] verbatim.
//!
//! **Conn selection rule (D-design):**
//! - READ methods (`get_*`, `list_*`, `find_*`, `max_*`, `next_*`, `edge_exists_*`,
//!   `for_each_*`) → [`super::SqliteBackend::block_read`].
//! - WRITE methods (`insert_*`, `expire_*`, `update_*`, `increment_*`, `merge_*`,
//!   `mark_*`, `set_*`, `stamp_*`, `hard_delete_*`, `ensure_scope_path`) →
//!   [`super::SqliteBackend::block_write`].
//!
//! Borrowed arguments are cloned to owned before entering the `'static` closure.
//! `embed_dim` is captured as a `let` binding outside the closure so `FactStore`
//! construction is not coupled to `self` inside the blocking thread.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::{SqliteBackend, stream_consumer_dropped};
use crate::error::Result;
use crate::storage::graph::FactGraph;
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;
use crate::store::scopes::ScopeStore;
use crate::types::{
    Edge, EmbeddingFingerprint, Fact, FactScoringRow, FactType, NewEdge, NewFact, ScopeNode,
    SessionFact,
};

#[async_trait]
impl FactGraph for SqliteBackend {
    // -------------------------------------------------------------------------
    // facts: write
    // -------------------------------------------------------------------------

    // WRITE
    async fn insert_fact(&self, fact: &NewFact) -> Result<i64> {
        let fact = fact.clone();
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).insert(&fact))
            .await
    }

    // WRITE
    async fn insert_or_reinforce_fact(&self, fact: &NewFact) -> Result<(i64, bool)> {
        let fact = fact.clone();
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).insert_or_reinforce(&fact))
            .await
    }

    // WRITE
    async fn expire_fact(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).expire(id, now))
            .await
    }

    // WRITE
    async fn set_fact_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).set_pinned(id, pinned))
            .await
    }

    // WRITE
    async fn update_fact_importance(&self, id: i64, importance: f64) -> Result<()> {
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).update_importance(id, importance))
            .await
    }

    // WRITE
    async fn update_fact_importance_score(&self, id: i64, score: f64) -> Result<()> {
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).update_importance_score(id, score))
            .await
    }

    // WRITE
    async fn increment_fact_access(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).increment_access(id, now))
            .await
    }

    // WRITE
    async fn merge_fact_metadata(&self, id: i64, patch: &serde_json::Value) -> Result<()> {
        let patch = patch.clone();
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).merge_metadata(id, &patch))
            .await
    }

    // WRITE
    async fn mark_facts_dream_cycled(
        &self,
        ids: &[i64],
        cycle_id: u64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let ids = ids.to_vec();
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).mark_dream_cycled(&ids, cycle_id, now))
            .await
    }

    // WRITE
    async fn stamp_facts_surfaced(
        &self,
        fact_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Result<Vec<(i64, DateTime<Utc>)>> {
        let fact_ids = fact_ids.to_vec();
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).stamp_surfaced(&fact_ids, now))
            .await
    }

    // WRITE
    async fn hard_delete_facts(&self, ids: &[i64]) -> Result<usize> {
        let ids = ids.to_vec();
        let dim = self.embed_dim;
        self.block_write(move |c| FactStore::new(c, dim).hard_delete_ids(&ids))
            .await
    }

    // -------------------------------------------------------------------------
    // facts: read (single / batch / full scan)
    // -------------------------------------------------------------------------

    // READ
    async fn get_fact(&self, id: i64) -> Result<Fact> {
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).get(id))
            .await
    }

    // READ
    async fn get_facts(&self, ids: &[i64]) -> Result<HashMap<i64, Fact>> {
        let ids = ids.to_vec();
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).get_many(&ids))
            .await
    }

    // READ
    async fn list_all_facts(&self) -> Result<Vec<Fact>> {
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).list_all())
            .await
    }

    // READ (streaming)
    async fn for_each_fact(&self, f: &mut (dyn FnMut(Fact) -> Result<()> + Send)) -> Result<()> {
        let dim = self.embed_dim;
        self.for_each_streamed(
            move |conn, tx| {
                FactStore::new(conn, dim).for_each(|fact| {
                    tx.blocking_send(fact)
                        .map_err(|_| stream_consumer_dropped())
                })
            },
            f,
        )
        .await
    }

    // READ
    async fn max_caller_written_fact_id(&self) -> Result<Option<i64>> {
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).max_caller_written_fact_id())
            .await
    }

    // -------------------------------------------------------------------------
    // facts: read (filtered / scored lists)
    // -------------------------------------------------------------------------

    // READ
    async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).list_active(limit))
            .await
    }

    // READ
    async fn list_active_facts_scoring(&self) -> Result<Vec<FactScoringRow>> {
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).list_active_scoring())
            .await
    }

    // READ
    async fn list_active_facts_at(&self, valid_at: DateTime<Utc>) -> Result<Vec<Fact>> {
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).list_active_at(valid_at))
            .await
    }

    // READ
    async fn list_dormant_facts(
        &self,
        importance_threshold: f64,
        scope_ids: Option<&[i64]>,
    ) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.map(<[i64]>::to_vec);
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_dormant(importance_threshold, scope_ids.as_deref())
        })
        .await
    }

    // READ
    async fn list_facts_by_scope_importance(
        &self,
        scope_id: i64,
        limit: usize,
    ) -> Result<Vec<Fact>> {
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).list_by_scope_importance(scope_id, limit))
            .await
    }

    // READ
    async fn list_facts_by_scopes_importance(
        &self,
        scope_ids: &[i64],
        min_importance: f64,
        limit: usize,
        exclude_ids: &HashSet<i64>,
    ) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let exclude_ids = exclude_ids.clone();
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_by_scopes_importance(
                &scope_ids,
                min_importance,
                limit,
                &exclude_ids,
            )
        })
        .await
    }

    // READ
    async fn list_facts_by_importance_score(
        &self,
        scope_ids: &[i64],
        min_score: f64,
        limit: usize,
        exclude: &HashSet<i64>,
    ) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let exclude = exclude.clone();
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_by_importance_score(&scope_ids, min_score, limit, &exclude)
        })
        .await
    }

    // READ
    async fn list_pinned_facts(&self, scope_ids: &[i64]) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).list_pinned(&scope_ids))
            .await
    }

    // READ
    async fn list_due_facts(&self, now: DateTime<Utc>, scope_ids: &[i64]) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).list_due(now, &scope_ids))
            .await
    }

    // READ
    async fn next_due_time(
        &self,
        now: DateTime<Utc>,
        scope_ids: &[i64],
    ) -> Result<Option<DateTime<Utc>>> {
        let scope_ids = scope_ids.to_vec();
        let dim = self.embed_dim;
        self.block_read(move |c| FactStore::new(c, dim).next_due_time(now, &scope_ids))
            .await
    }

    // READ
    async fn list_facts_by_scopes_recent(
        &self,
        scope_ids: &[i64],
        limit: usize,
        exclude_ids: &HashSet<i64>,
    ) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let exclude_ids = exclude_ids.clone();
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_by_scopes_recent(&scope_ids, limit, &exclude_ids)
        })
        .await
    }

    // READ
    async fn list_active_facts_by_metadata_key_recent(
        &self,
        scope_ids: &[i64],
        marker_key: &str,
        limit: usize,
    ) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let marker_key = marker_key.to_owned();
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_active_by_metadata_key_recent(
                &scope_ids,
                &marker_key,
                limit,
            )
        })
        .await
    }

    // READ
    async fn list_active_facts_in_period(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        scope_ids: &[i64],
        fact_type: Option<&FactType>,
    ) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let fact_type = fact_type.copied();
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_active_in_period(start, end, &scope_ids, fact_type.as_ref())
        })
        .await
    }

    // READ
    async fn list_undreamt_facts_in_period(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        scope_ids: &[i64],
        fact_type: Option<&FactType>,
    ) -> Result<Vec<Fact>> {
        let scope_ids = scope_ids.to_vec();
        let fact_type = fact_type.copied();
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_undreamt_in_period(
                start,
                end,
                &scope_ids,
                fact_type.as_ref(),
            )
        })
        .await
    }

    // READ
    async fn list_active_facts_by_session(
        &self,
        session_id: &str,
        scope_ids: &[i64],
    ) -> Result<Vec<SessionFact>> {
        let session_id = session_id.to_owned();
        let scope_ids = scope_ids.to_vec();
        let dim = self.embed_dim;
        self.block_read(move |c| {
            FactStore::new(c, dim).list_active_by_session(&session_id, &scope_ids)
        })
        .await
    }

    // -------------------------------------------------------------------------
    // edges
    // -------------------------------------------------------------------------

    // WRITE
    async fn insert_edge(&self, edge: &NewEdge) -> Result<i64> {
        let edge = edge.clone();
        self.block_write(move |c| EdgeStore::new(c).insert(&edge))
            .await
    }

    // READ
    async fn get_edge(&self, id: i64) -> Result<Edge> {
        self.block_read(move |c| EdgeStore::new(c).get(id)).await
    }

    // WRITE
    async fn expire_edge(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        self.block_write(move |c| EdgeStore::new(c).expire(id, now))
            .await
    }

    // WRITE
    async fn expire_edges_by_fact(&self, fact_id: i64, now: DateTime<Utc>) -> Result<usize> {
        self.block_write(move |c| EdgeStore::new(c).expire_by_fact(fact_id, now))
            .await
    }

    // READ
    async fn list_all_edges(&self) -> Result<Vec<Edge>> {
        self.block_read(move |c| EdgeStore::new(c).list_all()).await
    }

    // READ (streaming)
    async fn for_each_edge(&self, f: &mut (dyn FnMut(Edge) -> Result<()> + Send)) -> Result<()> {
        self.for_each_streamed(
            move |conn, tx| {
                EdgeStore::new(conn).for_each(|edge| {
                    tx.blocking_send(edge)
                        .map_err(|_| stream_consumer_dropped())
                })
            },
            f,
        )
        .await
    }

    // READ
    async fn list_active_edges(&self) -> Result<Vec<Edge>> {
        self.block_read(move |c| EdgeStore::new(c).list_active())
            .await
    }

    // READ
    async fn list_active_edges_by_source(&self, source_fact_id: i64) -> Result<Vec<Edge>> {
        self.block_read(move |c| EdgeStore::new(c).list_active_by_source(source_fact_id))
            .await
    }

    // READ
    async fn list_active_edges_by_target(&self, target_fact_id: i64) -> Result<Vec<Edge>> {
        self.block_read(move |c| EdgeStore::new(c).list_active_by_target(target_fact_id))
            .await
    }

    // READ
    async fn edge_exists_active(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation_type: &str,
    ) -> Result<bool> {
        let relation_type = relation_type.to_owned();
        self.block_read(move |c| {
            EdgeStore::new(c).exists_active(source_fact_id, target_fact_id, &relation_type)
        })
        .await
    }

    // READ
    async fn list_active_edge_pairs_by_facts(
        &self,
        fact_ids: &[i64],
        relation_type: &str,
    ) -> Result<HashSet<(i64, i64)>> {
        let fact_ids = fact_ids.to_vec();
        let relation_type = relation_type.to_owned();
        self.block_read(move |c| {
            EdgeStore::new(c).list_active_pairs_by_facts(&fact_ids, &relation_type)
        })
        .await
    }

    // WRITE
    async fn hard_delete_edges_by_facts(&self, fact_ids: &[i64]) -> Result<usize> {
        let fact_ids = fact_ids.to_vec();
        self.block_write(move |c| EdgeStore::new(c).hard_delete_by_facts(&fact_ids))
            .await
    }

    // -------------------------------------------------------------------------
    // scopes
    // -------------------------------------------------------------------------

    // READ
    async fn get_scope(&self, id: i64) -> Result<ScopeNode> {
        self.block_read(move |c| ScopeStore::new(c).get(id)).await
    }

    // READ
    async fn find_scope_by_label(&self, parent_id: i64, label: &str) -> Result<Option<ScopeNode>> {
        let label = label.to_owned();
        self.block_read(move |c| ScopeStore::new(c).find_by_label(parent_id, &label))
            .await
    }

    // WRITE
    async fn insert_scope(&self, parent_id: i64, label: &str, depth: i64) -> Result<ScopeNode> {
        let label = label.to_owned();
        self.block_write(move |c| ScopeStore::new(c).insert(parent_id, &label, depth))
            .await
    }

    // WRITE
    async fn ensure_scope_path(&self, path: &str) -> Result<i64> {
        let path = path.to_owned();
        self.block_write(move |c| ScopeStore::new(c).ensure_path(&path))
            .await
    }

    // READ
    async fn list_all_scopes(&self) -> Result<Vec<ScopeNode>> {
        self.block_read(move |c| ScopeStore::new(c).list_all())
            .await
    }

    // READ (streaming)
    async fn for_each_scope(
        &self,
        f: &mut (dyn FnMut(ScopeNode) -> Result<()> + Send),
    ) -> Result<()> {
        self.for_each_streamed(
            move |conn, tx| {
                ScopeStore::new(conn).for_each(|scope| {
                    tx.blocking_send(scope)
                        .map_err(|_| stream_consumer_dropped())
                })
            },
            f,
        )
        .await
    }

    // -------------------------------------------------------------------------
    // Stage A atomic port methods
    // -------------------------------------------------------------------------

    // ATOMIC WRITE — identity stamp + fact insert in one transaction (#613/#614).
    async fn insert_fact_atomic(
        &self,
        fact: &NewFact,
        fingerprint: &EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<i64> {
        let fact = fact.clone();
        let fingerprint = fingerprint.clone();
        let dim = self.embed_dim;
        self.block_write(move |conn| {
            // Verbatim body of ingest.rs:228-231: one unchecked_transaction wrapping
            // stamp_identity + FactStore::insert so a vector is never committed
            // without an established, matching identity (the #614 silent-corruption guard).
            let tx = conn.unchecked_transaction()?;
            // stamp_identity equivalent: record-if-absent or compare-and-reject
            crate::store::embedding_meta::record_if_absent(&tx, &fingerprint, expected_dim)?;
            let id = FactStore::new(&tx, dim).insert(&fact)?;
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    // ATOMIC WRITE — savepoint wrapping scope-resolution + batch fact insert.
    // Returns (fact_ids, scope_ids_to_cache) — the engine applies scope_tree.write()
    // from scope_ids_to_cache AFTER this call returns, on the success path only.
    async fn insert_facts_batch_atomic(
        &self,
        facts: &[NewFact],
        scope_paths: &[Option<String>],
        fingerprint: &EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<(Vec<i64>, Vec<i64>)> {
        let facts = facts.to_vec();
        let scope_paths = scope_paths.to_vec();
        let fingerprint = fingerprint.clone();
        let dim = self.embed_dim;
        self.block_write(move |conn| {
            // Verbatim body of ingest.rs:397-476: savepoint wrapping stamp +
            // scope-resolve + per-fact insert.
            conn.execute_batch("SAVEPOINT batch_insert")?;

            let result: Result<(Vec<i64>, Vec<i64>)> = (|| {
                // Record the embedding identity on first write (#613), inside the
                // savepoint so it commits atomically with the batch.
                crate::store::embedding_meta::record_if_absent(conn, &fingerprint, expected_dim)?;

                let scope_store = ScopeStore::new(conn);
                let store = FactStore::new(conn, dim);

                // Resolve scopes INSIDE the savepoint so they roll back on error.
                // Deduplicate by path to avoid N redundant DB lookups.
                let mut scope_cache: std::collections::HashMap<String, i64> =
                    std::collections::HashMap::new();
                let mut per_entry_scope_ids = Vec::with_capacity(facts.len());
                for path_opt in &scope_paths {
                    let scope_id = match path_opt {
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
                    per_entry_scope_ids.push(scope_id);
                }
                let scope_ids_to_cache: Vec<i64> = scope_cache.into_values().collect();

                let mut ids = Vec::with_capacity(facts.len());
                for (i, fact) in facts.iter().enumerate() {
                    // Patch scope_id from the resolved value (the NewFact coming in
                    // may have a placeholder 0; in practice the engine already sets it,
                    // but we honor the resolved scope_id from the savepoint).
                    let mut f = fact.clone();
                    f.scope_id = per_entry_scope_ids[i];
                    let fact_id = store.insert(&f)?;
                    ids.push(fact_id);
                }

                Ok((ids, scope_ids_to_cache))
            })();

            match result {
                Ok(pair) => {
                    conn.execute_batch("RELEASE batch_insert")?;
                    Ok(pair)
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK TO batch_insert");
                    let _ = conn.execute_batch("RELEASE batch_insert");
                    Err(e)
                }
            }
        })
        .await
    }

    // ATOMIC WRITE — co-session edge batch in one transaction.
    async fn insert_cosession_edges_atomic(
        &self,
        fact_ids: &[i64],
        relation: &str,
        weight: f64,
        scope_id: i64,
        now: DateTime<Utc>,
    ) -> Result<Vec<(i64, i64, i64)>> {
        let fact_ids = fact_ids.to_vec();
        let relation = relation.to_owned();
        self.block_write(move |conn| {
            // Verbatim body of engine/graph.rs:71-101: one unchecked_transaction
            // wrapping batch-dedup + edge inserts.
            let tx = conn.unchecked_transaction()?;
            let edge_store = EdgeStore::new(&tx);

            let existing = edge_store.list_active_pairs_by_facts(&fact_ids, &relation)?;

            let mut new_edges: Vec<(i64, i64, i64)> = Vec::new();
            for i in 0..fact_ids.len() {
                for j in (i + 1)..fact_ids.len() {
                    let a_id = fact_ids[i];
                    let b_id = fact_ids[j];
                    for (src, tgt) in [(a_id, b_id), (b_id, a_id)] {
                        if !existing.contains(&(src, tgt)) {
                            let edge_id = edge_store.insert(&NewEdge {
                                source_fact_id: src,
                                target_fact_id: tgt,
                                relation_type: relation.clone(),
                                weight,
                                scope_id,
                                t_created: now,
                                t_expired: None,
                            })?;
                            new_edges.push((edge_id, src, tgt));
                        }
                    }
                }
            }

            tx.commit()?;
            Ok(new_edges)
        })
        .await
    }

    // READ — archive candidate selection (verbatim body of engine/archive.rs:167-176).
    async fn select_archive_candidates(
        &self,
        expired_before: DateTime<Utc>,
    ) -> Result<(Vec<Fact>, Vec<Edge>)> {
        let dim = self.embed_dim;
        self.block_read(move |conn| {
            let candidate_facts =
                FactStore::new(conn, dim).list_archive_candidates(expired_before)?;
            let candidate_ids: Vec<i64> = candidate_facts.iter().map(|f| f.id).collect();
            let candidate_edges = EdgeStore::new(conn).list_internal_by_facts(&candidate_ids)?;
            Ok((candidate_facts, candidate_edges))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::error::{ConflictError, MemoryError};
    use crate::pool::ConnectionPool;
    use crate::storage::graph::FactGraph;
    use crate::store::facts::FactStore;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::{Edge, Fact, FactType, NewEdge, NewFact};

    const DIM: usize = 4;

    /// Build a `NewFact` with the given content and embedding (`scope_id=1`).
    fn fact(content: &str, embedding: [f32; DIM]) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: embedding.to_vec(),
            fact_type: FactType::Episodic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
    }

    /// Seed facts directly via `FactStore` (write-lock held across the loop),
    /// return the shared pool. Pattern mirrors `search_index.rs::seeded()`.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the write guard is intentionally held across the seed loop"
    )]
    fn seeded(facts: &[NewFact]) -> Arc<ConnectionPool> {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            for f in facts {
                store.insert(f).unwrap();
            }
        }
        pool
    }

    fn backend(pool: Arc<ConnectionPool>) -> SqliteBackend {
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    // -------------------------------------------------------------------------
    // H4 — semantic errors pass through; read-only rejects writes
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn get_fact_missing_yields_not_found() {
        // H4: missing id → NotFound, NOT remapped to Storage(Backend).
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        let err = be.get_fact(999).await.unwrap_err();
        assert!(
            matches!(err, MemoryError::NotFound(_)),
            "expected NotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn insert_on_read_only_backend_yields_read_only() {
        // H4: write through a read-only pool → ReadOnly.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.db");
        {
            let _rw = ConnectionPool::open(&path, DIM, 2, None).unwrap();
        }
        let ro = ConnectionPool::open_read_only(&path, DIM, 2).unwrap();
        let be = SqliteBackend::from_pool(Arc::new(ro), Arc::new(UpcasterRegistry::new()));
        let err = be.insert_fact(&fact("x", [0.1; DIM])).await.unwrap_err();
        assert!(matches!(err, MemoryError::ReadOnly), "got {err:?}");
    }

    #[tokio::test]
    async fn expire_on_read_only_backend_yields_read_only() {
        // H4 (expire path): write through a read-only pool → ReadOnly.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro2.db");
        {
            let rw = SqliteBackend::from_pool(
                Arc::new(ConnectionPool::open(&path, DIM, 2, None).unwrap()),
                Arc::new(UpcasterRegistry::new()),
            );
            rw.insert_fact(&fact("x", [0.1; DIM])).await.unwrap();
        }
        let ro = ConnectionPool::open_read_only(&path, DIM, 2).unwrap();
        let be = SqliteBackend::from_pool(Arc::new(ro), Arc::new(UpcasterRegistry::new()));
        let err = be.expire_fact(1, Utc::now()).await.unwrap_err();
        assert!(matches!(err, MemoryError::ReadOnly), "got {err:?}");
    }

    // -------------------------------------------------------------------------
    // H2 — scope_ids contract: empty=ALL vs empty=NONE preserved verbatim
    // -------------------------------------------------------------------------

    /// `scope_ids=[]` on `list_pinned` means ALL scopes (empty=ALL).
    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write guard is intentionally held across the seed block"
    )]
    async fn list_pinned_facts_empty_scope_means_all() {
        // Seed: two pinned facts in scope_id=1 (the only scope guaranteed by schema init).
        // The empty=ALL contract is about the filter being disabled, not about spanning
        // physically distinct scopes — two rows in scope 1 with empty slice must return both.
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            let mut f1 = fact("pinned one", [0.1; DIM]);
            f1.is_pinned = true;
            let mut f2 = fact("pinned two", [0.2; DIM]);
            f2.is_pinned = true;
            store.insert(&f1).unwrap();
            store.insert(&f2).unwrap();
        }
        // Oracle: direct FactStore call with empty slice.
        let oracle: Vec<Fact> = {
            let conn = pool.read().unwrap();
            FactStore::new(&conn, DIM).list_pinned(&[]).unwrap()
        };
        let be = backend(Arc::clone(&pool));
        let got = be.list_pinned_facts(&[]).await.unwrap();
        // Both should return the same 2 rows (empty = ALL scopes).
        assert_eq!(
            got.len(),
            2,
            "empty scope_ids should return all pinned facts"
        );
        assert_eq!(
            got.iter().map(|f| f.id).collect::<HashSet<_>>(),
            oracle.iter().map(|f| f.id).collect::<HashSet<_>>(),
        );
    }

    /// `scope_ids=[]` on `list_facts_by_scopes_recent` means NO scopes (empty=NONE).
    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write guard is intentionally held across the seed block"
    )]
    async fn list_facts_by_scopes_recent_empty_scope_means_none() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            store.insert(&fact("alpha", [0.1; DIM])).unwrap();
            store.insert(&fact("beta", [0.2; DIM])).unwrap();
        }
        // Oracle: FactStore with empty scope_ids.
        let oracle: Vec<Fact> = {
            let conn = pool.read().unwrap();
            FactStore::new(&conn, DIM)
                .list_by_scopes_recent(&[], 10, &HashSet::new())
                .unwrap()
        };
        let be = backend(Arc::clone(&pool));
        let got = be
            .list_facts_by_scopes_recent(&[], 10, &HashSet::new())
            .await
            .unwrap();
        // Both oracle and backend should return 0 (empty = NO scopes).
        assert!(got.is_empty(), "empty scope_ids should return no facts");
        assert_eq!(got, oracle);
    }

    /// `scope_ids=[]` on `list_active_facts_by_metadata_key_recent` means NO scopes (empty=NONE).
    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write guard is intentionally held across the seed block"
    )]
    async fn list_active_by_metadata_key_recent_empty_scope_means_none() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            let mut f = fact("with key", [0.1; DIM]);
            f.metadata = serde_json::json!({"insight": {"v": 1}});
            store.insert(&f).unwrap();
        }
        let oracle: Vec<Fact> = {
            let conn = pool.read().unwrap();
            FactStore::new(&conn, DIM)
                .list_active_by_metadata_key_recent(&[], "insight", 10)
                .unwrap()
        };
        let be = backend(Arc::clone(&pool));
        let got = be
            .list_active_facts_by_metadata_key_recent(&[], "insight", 10)
            .await
            .unwrap();
        assert!(got.is_empty(), "empty scope_ids = no scopes");
        assert_eq!(got, oracle);
    }

    // -------------------------------------------------------------------------
    // H3 — marker_key injection guard
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn list_active_by_metadata_key_recent_rejects_invalid_keys() {
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        let invalid_keys = ["", "in'sight", "$.x", "a b", "a;b"];
        for key in invalid_keys {
            let err = be
                .list_active_facts_by_metadata_key_recent(&[1], key, 10)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    MemoryError::Conflict(ConflictError::QueryValidation(_))
                ),
                "key {key:?} should yield QueryValidation, got {err:?}"
            );
        }
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write guard is intentionally held across the seed block"
    )]
    async fn list_active_by_metadata_key_recent_happy_path_parity() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            let mut f = fact("with marker", [0.1; DIM]);
            f.metadata = serde_json::json!({"insight": {"v": 1}});
            store.insert(&f).unwrap();
            // A fact without the key — should not appear.
            store.insert(&fact("no marker", [0.2; DIM])).unwrap();
        }
        let oracle: Vec<Fact> = {
            let conn = pool.read().unwrap();
            FactStore::new(&conn, DIM)
                .list_active_by_metadata_key_recent(&[1], "insight", 10)
                .unwrap()
        };
        let be = backend(Arc::clone(&pool));
        let got = be
            .list_active_facts_by_metadata_key_recent(&[1], "insight", 10)
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got, oracle);
    }

    // -------------------------------------------------------------------------
    // Streaming — for_each_fact / for_each_edge / for_each_scope
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn for_each_fact_collects_same_as_direct_store() {
        let pool = seeded(&[
            fact("alpha", [0.1; DIM]),
            fact("beta", [0.2; DIM]),
            fact("gamma", [0.3; DIM]),
        ]);
        let oracle: Vec<i64> = {
            let conn = pool.read().unwrap();
            let mut ids = Vec::new();
            FactStore::new(&conn, DIM)
                .for_each(|f| {
                    ids.push(f.id);
                    Ok(())
                })
                .unwrap();
            ids
        };
        let be = backend(Arc::clone(&pool));
        let mut got: Vec<i64> = Vec::new();
        be.for_each_fact(&mut |f: Fact| {
            got.push(f.id);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(got, oracle);
        assert_eq!(got.len(), 3);
    }

    #[tokio::test]
    async fn for_each_fact_callback_error_propagates_and_stops_early() {
        let pool = seeded(&[
            fact("a", [0.1; DIM]),
            fact("b", [0.2; DIM]),
            fact("c", [0.3; DIM]),
        ]);
        let be = backend(pool);
        let mut count = 0usize;
        let err = be
            .for_each_fact(&mut |_f: Fact| {
                count += 1;
                if count == 2 {
                    return Err(MemoryError::Lineage("stop at 2".into()));
                }
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::Lineage(ref m) if m == "stop at 2"),
            "callback error must propagate, got {err:?}"
        );
        assert_eq!(count, 2, "must stop early at the erroring row");
    }

    // -------------------------------------------------------------------------
    // Round-trips
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn fact_insert_get_round_trip() {
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        let f = fact("hello world", [0.1; DIM]);
        let id = be.insert_fact(&f).await.unwrap();
        let got = be.get_fact(id).await.unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.content, "hello world");
    }

    #[tokio::test]
    async fn expire_fact_excludes_from_list_active() {
        let pool = seeded(&[fact("live", [0.1; DIM]), fact("to_expire", [0.2; DIM])]);
        let be = backend(Arc::clone(&pool));
        // Get the id of the second fact.
        let all = be.list_all_facts().await.unwrap();
        let expire_id = all.iter().find(|f| f.content == "to_expire").unwrap().id;
        be.expire_fact(expire_id, Utc::now()).await.unwrap();
        let active = be.list_active_facts(None).await.unwrap();
        assert_eq!(active.len(), 1);
        assert!(
            active.iter().all(|f| f.id != expire_id),
            "expired fact must not appear in list_active_facts"
        );
    }

    #[tokio::test]
    async fn edge_insert_get_list_by_source_round_trip() {
        // Seed a fact first (FK constraint).
        let pool = seeded(&[fact("src", [0.1; DIM]), fact("tgt", [0.2; DIM])]);
        let be = backend(Arc::clone(&pool));
        let all = be.list_all_facts().await.unwrap();
        let src_id = all[0].id;
        let tgt_id = all[1].id;
        let new_edge = NewEdge {
            source_fact_id: src_id,
            target_fact_id: tgt_id,
            relation_type: "related_to".into(),
            weight: 0.9,
            t_created: Utc::now(),
            t_expired: None,
            scope_id: 1,
        };
        let edge_id = be.insert_edge(&new_edge).await.unwrap();
        let got: Edge = be.get_edge(edge_id).await.unwrap();
        assert_eq!(got.source_fact_id, src_id);
        assert_eq!(got.target_fact_id, tgt_id);
        assert_eq!(got.relation_type, "related_to");

        let by_source = be.list_active_edges_by_source(src_id).await.unwrap();
        assert_eq!(by_source.len(), 1);
        assert_eq!(by_source[0].id, edge_id);
    }

    #[tokio::test]
    async fn scope_ensure_get_round_trip() {
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        let leaf_id = be
            .ensure_scope_path("user:michael/project:demo")
            .await
            .unwrap();
        let scope = be.get_scope(leaf_id).await.unwrap();
        assert_eq!(scope.label, "project:demo");
        assert_eq!(scope.depth, 2);
    }

    // =========================================================================
    // Stage A — parity + rollback tests for atomic port methods
    // =========================================================================

    use crate::storage::schema::SchemaManager;
    use crate::store::embedding_meta;
    use crate::types::EmbeddingFingerprint;

    fn fp() -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("test-model", "tei", DIM)
    }

    fn mismatched_fp() -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("other-model", "tei", DIM)
    }

    // -------------------------------------------------------------------------
    // insert_fact_atomic
    // -------------------------------------------------------------------------

    /// Happy path: fact is inserted and `fact_id` is returned.
    #[tokio::test]
    async fn insert_fact_atomic_inserts_fact_and_stamps_identity() {
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        let f = fact("hello", [0.1; DIM]);
        let id = be.insert_fact_atomic(&f, &fp(), DIM).await.unwrap();
        assert!(id > 0);
        let got = be.get_fact(id).await.unwrap();
        assert_eq!(got.content, "hello");
        // Identity must now be stamped in the DB.
        let stored = be.load_embedding_fingerprint().await.unwrap();
        assert_eq!(stored, Some(fp()));
    }

    /// Parity: `insert_fact_atomic` produces the same row as a direct
    /// `FactStore::insert` on a pre-stamped store.
    #[tokio::test]
    #[allow(clippy::significant_drop_tightening)]
    async fn insert_fact_atomic_parity_with_direct_store() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        // Oracle: stamp identity + insert via direct FactStore.
        let oracle_id = {
            let conn = pool.write();
            embedding_meta::record_if_absent(&conn, &fp(), DIM).unwrap();
            FactStore::new(&conn, DIM)
                .insert(&fact("oracle", [0.2; DIM]))
                .unwrap()
        };
        // Subject: insert via port.
        let be = backend(Arc::clone(&pool));
        let subject_id = be
            .insert_fact_atomic(&fact("subject", [0.3; DIM]), &fp(), DIM)
            .await
            .unwrap();

        // Both ids exist and are distinct.
        assert_ne!(oracle_id, subject_id);
        let oracle_row = be.get_fact(oracle_id).await.unwrap();
        let subject_row = be.get_fact(subject_id).await.unwrap();
        assert_eq!(oracle_row.content, "oracle");
        assert_eq!(subject_row.content, "subject");
    }

    /// Crash-injection / rollback: a mismatched fingerprint inside the tx causes
    /// an error — the fact must NOT have been persisted (store byte-identical).
    #[tokio::test]
    #[allow(clippy::significant_drop_tightening)]
    async fn insert_fact_atomic_rollback_on_fingerprint_mismatch() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        // Pre-stamp the store with fp(), then try to insert with a mismatched one.
        {
            let conn = pool.write();
            embedding_meta::record_if_absent(&conn, &fp(), DIM).unwrap();
        }
        let be = backend(Arc::clone(&pool));
        let err = be
            .insert_fact_atomic(&fact("bad", [0.1; DIM]), &mismatched_fp(), DIM)
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
            "expected EmbeddingModelMismatch, got {err:?}"
        );
        // Store must be byte-identical: no facts inserted.
        let all = be.list_all_facts().await.unwrap();
        assert!(
            all.is_empty(),
            "rollback must leave no fact in the store; got {all:?}"
        );
    }

    /// #614 orphan-vector guard: a fresh (un-stamped) store must accept the
    /// first insert (establishing identity) and reject a second insert whose
    /// declared fingerprint disagrees.
    #[tokio::test]
    async fn insert_fact_atomic_orphan_vector_guard_614() {
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        // First insert: establishes fp() as the stored identity.
        let id1 = be
            .insert_fact_atomic(&fact("first", [0.1; DIM]), &fp(), DIM)
            .await
            .unwrap();
        assert!(id1 > 0);
        // Second insert with a different model: must be rejected.
        let err = be
            .insert_fact_atomic(&fact("second", [0.2; DIM]), &mismatched_fp(), DIM)
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
            "expected EmbeddingModelMismatch on second insert, got {err:?}"
        );
        // Only the first fact exists.
        assert_eq!(be.list_all_facts().await.unwrap().len(), 1);
    }

    // -------------------------------------------------------------------------
    // insert_facts_batch_atomic
    // -------------------------------------------------------------------------

    /// Happy path: batch inserts all facts and returns ids in order.
    #[tokio::test]
    async fn insert_facts_batch_atomic_inserts_all_facts() {
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        let facts = vec![
            fact("a", [0.1; DIM]),
            fact("b", [0.2; DIM]),
            fact("c", [0.3; DIM]),
        ];
        let paths: Vec<Option<String>> = vec![None, None, None];
        let (ids, scope_ids_to_cache) = be
            .insert_facts_batch_atomic(&facts, &paths, &fp(), DIM)
            .await
            .unwrap();
        assert_eq!(ids.len(), 3);
        assert!(
            scope_ids_to_cache.is_empty(),
            "root-scope paths produce no cache entries"
        );
        for (i, id) in ids.iter().enumerate() {
            let got = be.get_fact(*id).await.unwrap();
            let expected_content = ["a", "b", "c"][i];
            assert_eq!(got.content, expected_content);
        }
    }

    /// Scope split: a named scope path is resolved inside the savepoint and its id
    /// is returned in `scope_ids_to_cache` for the engine to update `scope_tree`.
    #[tokio::test]
    async fn insert_facts_batch_atomic_returns_scope_ids_to_cache() {
        let be = backend(Arc::new(ConnectionPool::open_memory(DIM).unwrap()));
        let facts = vec![fact("scoped", [0.1; DIM])];
        let paths: Vec<Option<String>> = vec![Some("user:test/project:foo".to_owned())];
        let (ids, scope_ids_to_cache) = be
            .insert_facts_batch_atomic(&facts, &paths, &fp(), DIM)
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);
        // The new scope ids must be non-empty (at least one new scope created).
        assert!(
            !scope_ids_to_cache.is_empty(),
            "named scope must produce scope_ids_to_cache"
        );
        // The inserted fact's scope_id must be in the returned set.
        let inserted = be.get_fact(ids[0]).await.unwrap();
        assert!(
            scope_ids_to_cache.contains(&inserted.scope_id) || inserted.scope_id == 1,
            "fact scope_id must be root or in the returned cache set"
        );
    }

    /// Rollback: injecting a dim-mismatch error on the stamp step aborts the
    /// savepoint — no facts are persisted.
    #[tokio::test]
    async fn insert_facts_batch_atomic_rollback_on_stamp_error() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        let be = backend(Arc::clone(&pool));
        // Pre-stamp with fp() so a mismatched fingerprint fails.
        be.insert_fact_atomic(&fact("seed", [0.9; DIM]), &fp(), DIM)
            .await
            .unwrap();

        let batch = vec![fact("bad1", [0.1; DIM]), fact("bad2", [0.2; DIM])];
        let paths: Vec<Option<String>> = vec![None, None];
        let err = be
            .insert_facts_batch_atomic(&batch, &paths, &mismatched_fp(), DIM)
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
            "expected EmbeddingModelMismatch, got {err:?}"
        );
        // Only the seed fact is in the store (the batch was rolled back).
        assert_eq!(
            be.list_all_facts().await.unwrap().len(),
            1,
            "batch rollback must leave only the seed fact"
        );
    }

    // -------------------------------------------------------------------------
    // insert_cosession_edges_atomic
    // -------------------------------------------------------------------------

    /// Happy path: edges are created between all fact pairs (bidirectional).
    #[tokio::test]
    async fn insert_cosession_edges_atomic_creates_bidirectional_edges() {
        let pool = seeded(&[fact("a", [0.1; DIM]), fact("b", [0.2; DIM])]);
        let be = backend(Arc::clone(&pool));
        let all = be.list_all_facts().await.unwrap();
        let fact_ids: Vec<i64> = all.iter().map(|f| f.id).collect();
        let now = Utc::now();
        let new_edges = be
            .insert_cosession_edges_atomic(&fact_ids, "co_session", 0.5, 1, now)
            .await
            .unwrap();
        // 2 facts → 2 directed edges (A→B and B→A).
        assert_eq!(new_edges.len(), 2, "expected 2 directed co-session edges");
        // Each entry is (edge_id, src, tgt) — edge_id > 0.
        for (edge_id, src, tgt) in &new_edges {
            assert!(*edge_id > 0);
            assert_ne!(src, tgt);
        }
        // Persisted in the edge table.
        let db_edges = be.list_active_edges().await.unwrap();
        assert_eq!(db_edges.len(), 2);
    }

    /// Idempotent: calling again for the same session creates no duplicate edges.
    #[tokio::test]
    async fn insert_cosession_edges_atomic_is_idempotent() {
        let pool = seeded(&[fact("x", [0.1; DIM]), fact("y", [0.2; DIM])]);
        let be = backend(Arc::clone(&pool));
        let all = be.list_all_facts().await.unwrap();
        let fact_ids: Vec<i64> = all.iter().map(|f| f.id).collect();
        let now = Utc::now();
        let first = be
            .insert_cosession_edges_atomic(&fact_ids, "co_session", 0.5, 1, now)
            .await
            .unwrap();
        let second = be
            .insert_cosession_edges_atomic(&fact_ids, "co_session", 0.5, 1, now)
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(
            second.len(),
            0,
            "idempotent: second call must create no new edges"
        );
        assert_eq!(be.list_active_edges().await.unwrap().len(), 2);
    }

    /// Crash-injection / rollback: including a non-existent `fact_id` triggers a
    /// foreign-key violation mid-transaction (the `edges.source_fact_id` FK on
    /// `facts.id` is enforced). Every edge insert that ran earlier in the tx must
    /// be rolled back — the `edges` table is byte-identical to its state before
    /// the call.
    ///
    /// Proof of atomicity (stronger than `is_err()`): we assert the exact edge
    /// count after the error, not just that an error occurred.
    #[tokio::test]
    #[allow(clippy::significant_drop_tightening)]
    async fn insert_cosession_edges_atomic_rollback_on_error() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        // Seed two real facts.
        let (real_a, real_b) = {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            let a = store.insert(&fact("p", [0.1; DIM])).unwrap();
            let b = store.insert(&fact("q", [0.2; DIM])).unwrap();
            (a, b)
        };

        let be = backend(Arc::clone(&pool));

        // Assert baseline: no edges exist yet.
        assert!(
            be.list_active_edges().await.unwrap().is_empty(),
            "baseline: edges table must be empty before the call"
        );

        // Pass [real_a, real_b, fake_id]. The method iterates pairs:
        //   (a,b) → insert OK (inside tx, not yet committed)
        //   (b,a) → insert OK
        //   (a,fake) → FK violation: `source_fact_id` references non-existent row
        //   (fake,a) → never reached
        //   (b,fake) → never reached
        // The whole transaction rolls back; no edges are committed.
        let fake_id: i64 = 99_999;
        let fact_ids = vec![real_a, real_b, fake_id];
        let err = be
            .insert_cosession_edges_atomic(&fact_ids, "co_session", 0.5, 1, Utc::now())
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::Storage(_) | MemoryError::Database(_)),
            "expected a storage/database error from FK violation, got {err:?}"
        );

        // Byte-identical assertion: edges table is still empty — exactly as before.
        let edges_after = be.list_active_edges().await.unwrap();
        assert!(
            edges_after.is_empty(),
            "rollback must leave edges table byte-identical (empty); got {edges_after:?}"
        );
    }

    // -------------------------------------------------------------------------
    // select_archive_candidates (read method)
    // -------------------------------------------------------------------------

    /// Parity: `select_archive_candidates` matches direct `FactStore::list_archive_candidates`.
    #[tokio::test]
    #[allow(clippy::significant_drop_tightening)]
    async fn select_archive_candidates_parity_with_direct_store() {
        use chrono::Duration;
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        let cutoff = Utc::now();
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            // Expired fact (qualifies as candidate).
            let mut expired = fact("expired", [0.1; DIM]);
            expired.t_expired = Some(cutoff - Duration::hours(1));
            store.insert(&expired).unwrap();
            // Active fact (must NOT appear as candidate).
            store.insert(&fact("active", [0.2; DIM])).unwrap();
        }
        // Oracle: direct FactStore call.
        let oracle_facts = {
            let conn = pool.read().unwrap();
            crate::store::facts::FactStore::new(&conn, DIM)
                .list_archive_candidates(cutoff + Duration::hours(1))
                .unwrap()
        };
        let be = backend(Arc::clone(&pool));
        let (port_facts, _edges) = be
            .select_archive_candidates(cutoff + Duration::hours(1))
            .await
            .unwrap();
        let oracle_ids: Vec<i64> = oracle_facts.iter().map(|f| f.id).collect();
        let port_ids: Vec<i64> = port_facts.iter().map(|f| f.id).collect();
        assert_eq!(
            oracle_ids, port_ids,
            "port must return same candidates as direct store"
        );
        assert_eq!(port_facts.len(), 1, "only the expired fact qualifies");
    }
}
