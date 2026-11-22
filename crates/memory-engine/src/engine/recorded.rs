//! What a store records of its embedding, read before anything opens it (#1049, #1065).

use crate::error::{MemoryError, Result};

/// What the store at `db` records of its embedding, read at any schema version without
/// validating or migrating it (#1049, #1065): its identity, a bare dimension, or nothing.
///
/// Opens one read-only connection and reads in one transaction; it takes no store lock and
/// never writes, so it runs beside a live server. A WAL store with no sidecars may gain an
/// empty `-wal` and `-shm`, as any reader's open does.
///
/// # Errors
///
/// `MemoryError::NotFound` unless `db` is an existing file; `MemoryError::Migration` for a
/// store newer than this build or a corrupt recorded value; `MemoryError::Storage` when the
/// store cannot be read.
///
/// # Examples
///
/// ```
/// use memory_engine::{RecordedEmbedding, read_recorded_embedding};
///
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("memory.db");
/// drop(memory_engine::MemoryEngine::builder(4).path(&db).build()?);
/// assert_eq!(read_recorded_embedding(&db)?, RecordedEmbedding::Nothing);
/// # Ok::<(), memory_engine::MemoryError>(())
/// ```
pub fn read_recorded_embedding(db: &std::path::Path) -> Result<crate::types::RecordedEmbedding> {
    if !db.is_file() {
        return Err(MemoryError::NotFound(format!(
            "database not found (or is a directory): {}",
            db.display()
        )));
    }
    let conn = super::embed_queue::open_status_connection(db)?;
    let tx = conn
        .unchecked_transaction()
        .map_err(crate::error::StorageError::backend)?;
    crate::store::embedding_meta::load_recorded(&tx)
}
