//! The deferred-embedding queue (#1021): a fact stored before its vector exists holds
//! `embedding = X''` and one `embed_queue` row until the drain fills the vector.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use crate::store::facts::FactStore;
use me_types::error::{Result, StorageError};
use me_types::types::NewFact;

/// Insert `fact` without a vector and queue it, due at `now`, in one transaction.
///
/// # Errors
///
/// Returns `MemoryError::EmbeddingDimension` if `fact.embedding` is not empty, or
/// `MemoryError::Storage` on a write failure; nothing is written on any error.
pub fn insert_deferred(
    conn: &Connection,
    fact: &NewFact,
    embed_dim: usize,
    now: DateTime<Utc>,
) -> Result<i64> {
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    let id = FactStore::new(&tx, embed_dim).insert_without_vector(fact)?;
    tx.execute(
        "INSERT INTO embed_queue (fact_id, enqueued_at, next_attempt_at) VALUES (?1, ?2, ?2)",
        params![id, now.to_rfc3339()],
    )
    .map_err(StorageError::backend)?;
    tx.commit().map_err(StorageError::backend)?;
    Ok(id)
}
