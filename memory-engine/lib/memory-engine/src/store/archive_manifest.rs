use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::{Result, StorageError};
use me_types::types::archive::ArchiveManifestEntry;

/// Store for archive manifest entries — tracks `.pak` files in the database.
pub struct ArchiveManifestStore<'a> {
    conn: &'a Connection,
}

/// Parameter object for [`ArchiveManifestStore::insert`].
///
/// Bundles the ten columns of a new `archive_manifest` row so call sites pass a
/// single named-field value instead of a long positional argument list. This
/// removes the argument-order footgun and the `clippy::too_many_arguments`
/// suppression on `insert`.
///
/// The two string fields borrow (`&'a str`) — the manifest insert does not retain
/// them past the call, so no allocation is forced on the caller.
#[derive(Debug, Clone, Copy)]
pub struct NewArchiveManifest<'a> {
    /// Relative path of the `.pak` file (unique key).
    pub pak_path: &'a str,
    /// System time the archive was created.
    pub created_at: DateTime<Utc>,
    /// Number of facts archived into the `.pak`.
    pub fact_count: i64,
    /// Number of edges archived into the `.pak`.
    pub edge_count: i64,
    /// Smallest archived fact id.
    pub fact_id_min: i64,
    /// Largest archived fact id.
    pub fact_id_max: i64,
    /// Earliest `t_created` across the archived facts.
    pub t_created_min: DateTime<Utc>,
    /// Latest `t_created` across the archived facts.
    pub t_created_max: DateTime<Utc>,
    /// Size of the `.pak` file in bytes.
    pub size_bytes: i64,
    /// BLAKE3 hash of the `.pak` file contents.
    pub blake3_hash: &'a str,
}

impl<'a> ArchiveManifestStore<'a> {
    /// Create a new `ArchiveManifestStore` borrowing the given connection.
    #[must_use]
    pub(crate) const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a manifest entry for a newly created `.pak` file.
    ///
    /// Returns the auto-assigned row id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure (e.g. duplicate `pak_path`).
    pub fn insert(&self, m: &NewArchiveManifest<'_>) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO archive_manifest (pak_path, created_at, fact_count, edge_count,
              fact_id_min, fact_id_max, t_created_min, t_created_max, size_bytes, blake3_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    m.pak_path,
                    m.created_at.to_rfc3339(),
                    m.fact_count,
                    m.edge_count,
                    m.fact_id_min,
                    m.fact_id_max,
                    m.t_created_min.to_rfc3339(),
                    m.t_created_max.to_rfc3339(),
                    m.size_bytes,
                    m.blake3_hash,
                ],
            )
            .map_err(StorageError::backend)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// List all manifest entries, ordered by `created_at` ascending.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn list(&self) -> Result<Vec<ArchiveManifestEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, pak_path, created_at, fact_count, edge_count,
                    fact_id_min, fact_id_max, t_created_min, t_created_max,
                    size_bytes, blake3_hash
             FROM archive_manifest ORDER BY created_at",
            )
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map([], |row| {
                let created_at_str: String = row.get(2)?;
                let t_created_min_str: String = row.get(7)?;
                let t_created_max_str: String = row.get(8)?;

                let parse_dt = |s: String, col_idx: usize| -> rusqlite::Result<DateTime<Utc>> {
                    s.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            col_idx,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })
                };

                Ok(ArchiveManifestEntry {
                    id: row.get(0)?,
                    pak_path: row.get(1)?,
                    created_at: parse_dt(created_at_str, 2)?,
                    fact_count: row.get(3)?,
                    edge_count: row.get(4)?,
                    fact_id_min: row.get(5)?,
                    fact_id_max: row.get(6)?,
                    t_created_min: parse_dt(t_created_min_str, 7)?,
                    t_created_max: parse_dt(t_created_max_str, 8)?,
                    size_bytes: row.get(9)?,
                    blake3_hash: row.get(10)?,
                })
            })
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
    }

    /// Delete a manifest entry by id.
    ///
    /// Returns `true` if a row was deleted, `false` if the id was not found.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    // Forward-looking: archive management CLI will use this.
    #[allow(dead_code)]
    pub fn delete(&self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute(
                "DELETE FROM archive_manifest WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(StorageError::backend)?;
        Ok(deleted > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_entry(pak_path: &str) -> NewArchiveManifest<'_> {
        let now = Utc::now();
        NewArchiveManifest {
            pak_path,
            created_at: now,
            fact_count: 10,
            edge_count: 5,
            fact_id_min: 1,
            fact_id_max: 10,
            t_created_min: now,
            t_created_max: now,
            size_bytes: 1024,
            blake3_hash: "deadbeefdeadbeefdeadbeefdeadbeef",
        }
    }

    #[test]
    fn insert_and_list_manifest() {
        let conn = setup();
        let store = ArchiveManifestStore::new(&conn);

        assert!(store.list().unwrap().is_empty());

        let entry = make_entry("archives/2026-01.pak");

        let id = store.insert(&entry).unwrap();
        assert!(id > 0);

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, id);
        assert_eq!(entry.pak_path, "archives/2026-01.pak");
        assert_eq!(entry.fact_count, 10);
        assert_eq!(entry.edge_count, 5);
        assert_eq!(entry.fact_id_min, 1);
        assert_eq!(entry.fact_id_max, 10);
        assert_eq!(entry.size_bytes, 1024);
        assert_eq!(entry.blake3_hash, "deadbeefdeadbeefdeadbeefdeadbeef");
    }

    #[test]
    fn insert_duplicate_pak_path_fails() {
        let conn = setup();
        let store = ArchiveManifestStore::new(&conn);
        let entry = make_entry("archives/dup.pak");

        store.insert(&entry).unwrap();

        // Second insert with same path must fail (UNIQUE INDEX)
        let result = store.insert(&entry);
        assert!(result.is_err(), "duplicate pak_path should be rejected");
    }

    #[test]
    fn delete_manifest_entry() {
        let conn = setup();
        let store = ArchiveManifestStore::new(&conn);

        let entry = make_entry("archives/to_delete.pak");

        let id = store.insert(&entry).unwrap();

        assert_eq!(store.list().unwrap().len(), 1);

        let deleted = store.delete(id).unwrap();
        assert!(deleted, "delete should return true for existing id");
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let conn = setup();
        let store = ArchiveManifestStore::new(&conn);

        let deleted = store.delete(9999).unwrap();
        assert!(!deleted, "delete of nonexistent id should return false");
    }
}
