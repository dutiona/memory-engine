//! `impl Retraction for SqliteBackend` — the forget retraction (#1023) over
//! [`tombstones`](crate::store::tombstones), each write in one transaction on the write
//! connection, the vector index notified after the commit.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::SqliteBackend;
use crate::store::events::EventStore;
use crate::store::scopes::ScopeStore;
use crate::store::tombstones;
use me_storage::ScopedWrite;
use me_storage::retraction::Retraction;
use me_types::error::{Result, StorageError};
use me_types::types::{ForgetOutcome, NewEvent, Tombstone, UnforgetOutcome};

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

    // READ
    async fn is_forgotten(&self, content: &str) -> Result<bool> {
        let content = content.to_owned();
        self.block_read(move |c| tombstones::blocks(c, &content))
            .await
    }
}
