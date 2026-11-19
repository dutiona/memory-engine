//! The store-wide redaction totals (#1024), in `config` keys named here once.

use rusqlite::{Connection, Transaction, params};

use crate::store::schema::get_config;
use me_types::error::{Result, StorageError};
use me_types::types::RedactionCounts;

/// Secrets redacted by the store's writes.
pub const REDACT_COUNT: &str = "redact_count";
/// Tool calls stored as sensitive-path stubs.
pub const STUB_COUNT: &str = "stub_count";

/// Add to both totals in one transaction; a zero leaves its total as it is.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on a write failure; nothing is written then.
pub fn add(conn: &Connection, redacted: u64, stubbed: u64) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    for (key, n) in [(REDACT_COUNT, redacted), (STUB_COUNT, stubbed)] {
        if n == 0 {
            continue;
        }
        let n = i64::try_from(n).map_err(StorageError::backend)?;
        tx.execute(
            "INSERT INTO config (key, value) VALUES (?1, CAST(?2 AS TEXT))
             ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + ?2 AS TEXT)",
            params![key, n],
        )
        .map_err(StorageError::backend)?;
    }
    tx.commit().map_err(StorageError::backend)?;
    Ok(())
}

/// Both totals; an absent one reads as 0.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on a read failure or a total that is not a count.
pub fn read_counts(tx: &Transaction) -> Result<RedactionCounts> {
    let count = |key| -> Result<u64> {
        get_config(tx, key)?.map_or(Ok(0), |v| {
            v.parse::<u64>()
                .map_err(StorageError::backend)
                .map_err(Into::into)
        })
    };
    Ok(RedactionCounts {
        redacted: count(REDACT_COUNT)?,
        stubbed: count(STUB_COUNT)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_add_up_and_start_at_zero() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        let read = |conn: &Connection| read_counts(&conn.unchecked_transaction().unwrap()).unwrap();
        assert_eq!(read(&conn), RedactionCounts::default());
        add(&conn, 2, 0).unwrap();
        add(&conn, 3, 1).unwrap();
        assert_eq!(
            read(&conn),
            RedactionCounts {
                redacted: 5,
                stubbed: 1
            }
        );
    }
}
