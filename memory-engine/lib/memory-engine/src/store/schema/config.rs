//! The `config` key-value table: read, list, and upsert helpers.
//!
//! `config` stores schema metadata (`schema_version`, `storage_epoch`) and
//! consumer-visible tooling keys. These helpers are the sole access path; see
//! [`migrate`](super::migrate) and [`init_schema`](super::init_schema) for the
//! lifecycle keys they seed.

use crate::error::StorageError;
use rusqlite::Connection;

use crate::error::Result;

/// Read a config value by key.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure.
pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT value FROM config WHERE key = ?1")
        .map_err(StorageError::backend)?;
    let mut rows = stmt
        .query_map([key], |row| row.get(0))
        .map_err(StorageError::backend)?;
    match rows.next() {
        Some(Ok(val)) => Ok(Some(val)),
        Some(Err(e)) => Err(StorageError::backend(e).into()),
        None => Ok(None),
    }
}

/// List all config key-value pairs.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure.
pub fn list_config(conn: &Connection) -> Result<std::collections::BTreeMap<String, String>> {
    use std::collections::BTreeMap;
    let mut stmt = conn
        .prepare("SELECT key, value FROM config")
        .map_err(StorageError::backend)?;
    let rows = stmt
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })
        .map_err(StorageError::backend)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (key, value) = row.map_err(StorageError::backend)?;
        map.insert(key, value);
    }
    Ok(map)
}

/// Write a config value (upsert).
///
/// # Errors
///
/// Returns `MemoryError::Storage` on write failure.
pub fn set_config(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )
    .map_err(StorageError::backend)?;
    Ok(())
}
