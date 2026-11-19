//! The redaction totals `status` reports (#1024), read beside a live server.

use std::path::Path;

use crate::error::{Result, StorageError};
use crate::types::RedactionCounts;

use super::embed_queue::open_status_connection;

/// Read the store-wide redaction totals through one read-only connection, like
/// [`read_embed_queue_status`](crate::read_embed_queue_status): no engine, no store lock, no
/// migration. Both totals read 0 on a store from before #1024.
///
/// # Errors
///
/// Returns `MemoryError::Storage` if the file cannot be opened read-only or a total is not a
/// count, and `MemoryError::Migration` if its schema version is not this binary's.
///
/// # Examples
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("memory.db");
/// let mut engine = memory_engine::MemoryEngine::builder(4).path(&db).build()?;
/// engine.close().await?;
///
/// let counts = memory_engine::read_redaction_counts(&db)?;
/// assert_eq!((counts.redacted, counts.stubbed), (0, 0));
/// # Ok::<(), memory_engine::MemoryError>(())
/// # }).unwrap();
/// ```
pub fn read_redaction_counts(db: &Path) -> Result<RedactionCounts> {
    let conn = open_status_connection(db)?;
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    crate::store::schema::validate_schema_version(&tx)?;
    crate::store::redaction::read_counts(&tx)
}
