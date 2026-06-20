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
    Edge, Fact, FactScoringRow, FactType, NewEdge, NewFact, ScopeNode, SessionFact,
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
}
