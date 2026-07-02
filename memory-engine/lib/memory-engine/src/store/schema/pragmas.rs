//! Connection pragmas and foreign-key helpers.
//!
//! These set the durability/concurrency pragmas on freshly opened connections
//! and toggle/verify foreign-key enforcement around table-rebuild migrations.

use crate::error::StorageError;
use rusqlite::Connection;

use crate::error::{MigrationError, Result};

/// Pragmas safe for read-only connections — skip WAL and synchronous.
pub(super) fn set_pragmas_read_only(conn: &Connection) -> Result<()> {
    for pragma in &["PRAGMA foreign_keys = ON", "PRAGMA busy_timeout = 5000"] {
        let mut stmt = conn.prepare(pragma).map_err(StorageError::backend)?;
        let _ = stmt.query([]).map_err(StorageError::backend)?;
    }
    Ok(())
}

pub(super) fn set_pragmas(conn: &Connection) -> Result<()> {
    // All PRAGMA SET statements can return result rows in bundled SQLite.
    // Use prepare+execute to avoid ExecuteReturnedResults from execute/execute_batch.
    for pragma in &[
        "PRAGMA journal_mode = WAL",
        "PRAGMA foreign_keys = ON",
        "PRAGMA busy_timeout = 5000",
        "PRAGMA synchronous = NORMAL",
    ] {
        let mut stmt = conn.prepare(pragma).map_err(StorageError::backend)?;
        // Consume all rows (PRAGMAs return 0 or 1 rows).
        let _ = stmt.query([]).map_err(StorageError::backend)?;
    }
    Ok(())
}

pub(super) fn set_foreign_keys(conn: &Connection, enabled: bool) -> Result<()> {
    let sql = if enabled {
        "PRAGMA foreign_keys = ON"
    } else {
        "PRAGMA foreign_keys = OFF"
    };
    conn.execute_batch(sql).map_err(StorageError::backend)?;
    Ok(())
}

pub(super) fn check_foreign_keys(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(StorageError::backend)?;
    let mut rows = stmt.query([]).map_err(StorageError::backend)?;
    if rows.next().map_err(StorageError::backend)?.is_some() {
        return Err(MigrationError::Incompatible(
            "foreign key violations detected after table rebuild".to_string(),
        )
        .into());
    }
    Ok(())
}
