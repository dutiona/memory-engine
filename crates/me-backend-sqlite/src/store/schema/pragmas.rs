//! Connection pragmas and foreign-key helpers.
//!
//! These set the durability/concurrency pragmas on freshly opened connections
//! and toggle/verify foreign-key enforcement around table-rebuild migrations.

use me_types::error::StorageError;
use rusqlite::Connection;

use me_types::error::{MigrationError, Result};

/// Pragmas safe for read-only connections — skip WAL and synchronous (a read-only connection
/// never commits).
pub(super) fn set_pragmas_read_only(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
        .map_err(StorageError::backend)?;
    Ok(())
}

pub(super) fn set_pragmas(conn: &Connection) -> Result<()> {
    set_pragmas_read_only(conn)?;
    // `execute_batch` steps each statement: `journal_mode` takes effect only when stepped,
    // and a bare `query` never steps (#1040).
    //
    // `FULL`, not `NORMAL`: in WAL mode it syncs the log on every commit, so a write survives an
    // OS crash or a power loss, not only a process crash. The lifecycle route answers 204 only
    // after its commit, and the 204 deletes the client's copy of the event, so the 204 must mean
    // durable.
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")
        .map_err(StorageError::backend)?;
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
