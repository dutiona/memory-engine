//! `impl Retraction for SqliteBackend` — the forget retraction (#1023) over
//! [`tombstones`](crate::store::tombstones), each write in one transaction on the write
//! connection, the vector index notified after the commit.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::SqliteBackend;
use crate::store::erase;
use crate::store::events::EventStore;
use crate::store::scopes::ScopeStore;
use crate::store::tombstones;
use me_storage::ScopedWrite;
use me_storage::retraction::Retraction;
use me_types::error::{Result, StorageError};
use me_types::types::{
    EraseReport, EraseTarget, ForgetOutcome, NewEvent, Tombstone, UnforgetOutcome,
};

#[async_trait]
impl Retraction for SqliteBackend {
    // WRITE
    async fn forget_fact(
        &self,
        id: i64,
        homes: &[i64],
        reason: &str,
        now: DateTime<Utc>,
        record_at: &str,
        record: &NewEvent,
    ) -> Result<(ForgetOutcome, ScopedWrite)> {
        let homes = homes.to_vec();
        let reason = reason.to_owned();
        let record_at = record_at.to_owned();
        let mut record = record.clone();
        let registry = Arc::clone(&self.upcaster_registry);
        let (outcome, written, left_active) = self
            .block_write(move |c| {
                let tx = c.unchecked_transaction().map_err(StorageError::backend)?;
                let scopes = ScopeStore::new(&tx);
                record.scope_id = scopes.ensure_path(&record_at)?;
                let event_id = EventStore::new(&tx, &registry).insert(&record)?;
                let (outcome, left_active) =
                    tombstones::forget(&tx, id, &homes, &reason, now, event_id)?;
                let chain = scopes.chain(record.scope_id)?;
                tx.commit().map_err(StorageError::backend)?;
                Ok((
                    outcome,
                    ScopedWrite {
                        id: event_id,
                        chain,
                    },
                    left_active,
                ))
            })
            .await?;
        #[cfg(feature = "ann")]
        for fact in &left_active {
            self.hnsw_notify_expire(*fact);
        }
        #[cfg(not(feature = "ann"))]
        let _ = left_active;
        Ok((outcome, written))
    }

    // WRITE
    async fn unforget_fact(&self, id: i64, now: DateTime<Utc>) -> Result<UnforgetOutcome> {
        let registry = Arc::clone(&self.upcaster_registry);
        let dim = self.embed_dim;
        let (outcome, embeddings) = self
            .block_write(move |c| {
                let tx = c.unchecked_transaction().map_err(StorageError::backend)?;
                let Some(restored) = tombstones::unforget(&tx, &registry, id, now)? else {
                    let exists: bool = tx
                        .query_row(
                            "SELECT EXISTS (SELECT 1 FROM facts WHERE id = ?1)",
                            params![id],
                            |r| r.get(0),
                        )
                        .map_err(StorageError::backend)?;
                    let outcome = if exists {
                        UnforgetOutcome::NotForgotten
                    } else {
                        UnforgetOutcome::NotFound
                    };
                    return Ok((outcome, Vec::new()));
                };
                let mut embeddings = Vec::new();
                for &(fact, active) in &restored {
                    if active {
                        let blob: Option<Vec<u8>> = tx
                            .query_row(
                                "SELECT embedding FROM facts WHERE id = ?1",
                                params![fact],
                                |r| r.get(0),
                            )
                            .optional()
                            .map_err(StorageError::backend)?;
                        if let Some(blob) = blob {
                            embeddings
                                .push((fact, crate::store::deserialize_embedding(&blob, dim)?));
                        }
                    }
                }
                tx.commit().map_err(StorageError::backend)?;
                Ok((UnforgetOutcome::Restored { facts: restored }, embeddings))
            })
            .await?;
        #[cfg(feature = "ann")]
        for (fact, embedding) in &embeddings {
            self.hnsw_notify_insert(*fact, embedding)?;
        }
        #[cfg(not(feature = "ann"))]
        let _ = embeddings;
        Ok(outcome)
    }

    // READ
    async fn tombstone(&self, fact_id: i64) -> Result<Option<Tombstone>> {
        self.block_read(move |c| tombstones::get(c, fact_id)).await
    }

    // WRITE
    async fn erase(&self, target: &EraseTarget, now: DateTime<Utc>) -> Result<EraseReport> {
        let target = target.clone();
        let registry = Arc::clone(&self.upcaster_registry);
        let dim = self.embed_dim;
        let mut report = self
            .block_write(move |c| {
                let tx = c.unchecked_transaction().map_err(StorageError::backend)?;
                let report = erase::erase(&tx, &registry, &target, dim, now)?;
                tx.commit().map_err(StorageError::backend)?;
                Ok(report)
            })
            .await?;
        #[cfg(feature = "ann")]
        for fact in &report.erased_facts {
            self.hnsw_notify_expire(*fact);
        }
        // The purge: a deleted row's earlier versions (a forget, any later update) linger in
        // free pages, and the pre-erase page images in WAL frames. It runs after the commit,
        // and even when nothing was deleted, so the same erasure retries a failed purge.
        let purge = self
            .block_write(|c| {
                c.execute_batch("VACUUM").map_err(StorageError::backend)?;
                let busy: i64 = c
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
                    .map_err(StorageError::backend)?;
                Ok(busy)
            })
            .await;
        match purge {
            Ok(0) => report.purged = true,
            Ok(_) => {
                report.purge_error =
                    Some("the WAL checkpoint is busy: a reader still holds the log".to_owned());
            }
            Err(e) => report.purge_error = Some(e.to_string()),
        }
        Ok(report)
    }

    // READ
    async fn is_forgotten(&self, content: &str) -> Result<bool> {
        let content = content.to_owned();
        self.block_read(move |c| tombstones::blocks(c, &content))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use me_storage::retraction::Retraction;
    use me_types::types::{EraseTarget, FactType, NewFact};

    use super::SqliteBackend;
    use crate::pool::ConnectionPool;
    use crate::store::facts::FactStore;
    use crate::store::upcaster::UpcasterRegistry;

    const DIM: usize = 4;

    fn fact(content: &str) -> NewFact {
        NewFact::builder(content, vec![0.1; DIM], FactType::Semantic)
            .scope_id(1)
            .build()
    }

    fn backend(pool: &Arc<ConnectionPool>) -> SqliteBackend {
        SqliteBackend::from_pool(Arc::clone(pool), Arc::new(UpcasterRegistry::new()))
    }

    /// The fan-in the battery can only seed through the port, checked with SQL: after an
    /// erasure no foreign key dangles.
    #[tokio::test]
    #[allow(clippy::significant_drop_tightening)]
    async fn erase_leaves_no_dangling_reference() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            let a = store.insert(&fact("a")).unwrap();
            let b = store.insert(&fact("b")).unwrap();
            conn.execute_batch(&format!(
                "INSERT INTO edges (source_fact_id, target_fact_id, relation_type, t_created)
                     VALUES ({a}, {b}, 'r', 't'), ({b}, {a}, 'r', 't');
                 INSERT INTO activities (session_id, tool_name, args_hash, first_seen, last_seen,
                                         promoted_fact_id)
                     VALUES ('s', 'Edit', 'h', 't', 't', {a});
                 INSERT INTO fact_tombstones (fact_id, content_hash, forgotten_at)
                     VALUES ({a}, 'h', 't');"
            ))
            .unwrap();
        }
        backend(&pool)
            .erase(&EraseTarget::Fact(1), Utc::now())
            .await
            .unwrap();
        let conn = pool.write();
        let dangling: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(dangling, 0);
        let tombstones: i64 = conn
            .query_row("SELECT COUNT(*) FROM fact_tombstones", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tombstones, 1, "the block stays");
    }

    /// An event whose payload is not JSON is never the machine's, and does not abort.
    #[tokio::test]
    async fn erase_skips_an_event_with_an_invalid_json_payload() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        pool.write()
            .execute_batch(
                "INSERT INTO events (timestamp, event_type, payload, source)
                     VALUES ('t', 'memory_op', 'not json', 'x'),
                            ('t', 'memory_op', '{\"machine\":\"laptop\"}', 'x');",
            )
            .unwrap();
        let report = backend(&pool)
            .erase(&EraseTarget::Machine("laptop".into()), Utc::now())
            .await
            .unwrap();
        assert_eq!(report.deleted["events"], 1);
        let left: String = pool
            .write()
            .query_row("SELECT payload FROM events WHERE source = 'x'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(left, "not json");
    }

    /// A reader holding the log keeps the WAL checkpoint busy: the rows are gone, the purge
    /// is reported as not done, and the same erasure retries it once the reader leaves.
    #[tokio::test]
    async fn erase_reports_not_purged_when_the_checkpoint_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let pool = Arc::new(ConnectionPool::open(&path, DIM, 1, None).unwrap());
        let id = FactStore::new(&pool.write(), DIM)
            .insert(&fact("purge me"))
            .unwrap();
        let reader = rusqlite::Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        let be = backend(&pool);
        let report = be.erase(&EraseTarget::Fact(id), Utc::now()).await.unwrap();
        assert_eq!(report.erased_facts, vec![id]);
        assert!(!report.purged);
        assert!(report.purge_error.is_some());
        reader.execute_batch("COMMIT").unwrap();
        drop(reader);
        let retry = be.erase(&EraseTarget::Fact(id), Utc::now()).await.unwrap();
        assert_eq!(retry.erased_facts, Vec::<i64>::new());
        assert!(retry.purged, "{:?}", retry.purge_error);
    }
}
