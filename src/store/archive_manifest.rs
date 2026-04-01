use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::archive::types::ArchiveManifestEntry;
use crate::error::Result;

/// Store for archive manifest entries — tracks `.pak` files in the database.
pub struct ArchiveManifestStore<'a> {
    conn: &'a Connection,
}

impl<'a> ArchiveManifestStore<'a> {
    /// Create a new `ArchiveManifestStore` borrowing the given connection.
    #[must_use]
    pub const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a manifest entry for a newly created `.pak` file.
    ///
    /// Returns the auto-assigned row id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure (e.g. duplicate `pak_path`).
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        pak_path: &str,
        created_at: DateTime<Utc>,
        fact_count: i64,
        edge_count: i64,
        fact_id_min: i64,
        fact_id_max: i64,
        t_created_min: DateTime<Utc>,
        t_created_max: DateTime<Utc>,
        size_bytes: i64,
        blake3_hash: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO archive_manifest (pak_path, created_at, fact_count, edge_count,
              fact_id_min, fact_id_max, t_created_min, t_created_max, size_bytes, blake3_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                pak_path,
                created_at.to_rfc3339(),
                fact_count,
                edge_count,
                fact_id_min,
                fact_id_max,
                t_created_min.to_rfc3339(),
                t_created_max.to_rfc3339(),
                size_bytes,
                blake3_hash,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// List all manifest entries, ordered by `created_at` ascending.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list(&self) -> Result<Vec<ArchiveManifestEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pak_path, created_at, fact_count, edge_count,
                    fact_id_min, fact_id_max, t_created_min, t_created_max,
                    size_bytes, blake3_hash
             FROM archive_manifest ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
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
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Delete a manifest entry by id.
    ///
    /// Returns `true` if a row was deleted, `false` if the id was not found.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    // Forward-looking: archive management CLI will use this.
    #[allow(dead_code)]
    pub fn delete(&self, id: i64) -> Result<bool> {
        let deleted = self.conn.execute(
            "DELETE FROM archive_manifest WHERE id = ?1",
            rusqlite::params![id],
        )?;
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

    type EntryArgs = (
        String,
        DateTime<Utc>,
        i64,
        i64,
        i64,
        i64,
        DateTime<Utc>,
        DateTime<Utc>,
        i64,
        String,
    );

    fn make_entry(pak_path: &str) -> EntryArgs {
        let now = Utc::now();
        (
            pak_path.to_string(),
            now,
            10,
            5,
            1,
            10,
            now,
            now,
            1024,
            "deadbeefdeadbeefdeadbeefdeadbeef".to_string(),
        )
    }

    #[test]
    fn insert_and_list_manifest() {
        let conn = setup();
        let store = ArchiveManifestStore::new(&conn);

        assert!(store.list().unwrap().is_empty());

        let (
            pak_path,
            created_at,
            fact_count,
            edge_count,
            fact_id_min,
            fact_id_max,
            t_created_min,
            t_created_max,
            size_bytes,
            blake3_hash,
        ) = make_entry("archives/2026-01.pak");

        let id = store
            .insert(
                &pak_path,
                created_at,
                fact_count,
                edge_count,
                fact_id_min,
                fact_id_max,
                t_created_min,
                t_created_max,
                size_bytes,
                &blake3_hash,
            )
            .unwrap();
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
        let (
            pak_path,
            created_at,
            fact_count,
            edge_count,
            fact_id_min,
            fact_id_max,
            t_created_min,
            t_created_max,
            size_bytes,
            blake3_hash,
        ) = make_entry("archives/dup.pak");

        store
            .insert(
                &pak_path,
                created_at,
                fact_count,
                edge_count,
                fact_id_min,
                fact_id_max,
                t_created_min,
                t_created_max,
                size_bytes,
                &blake3_hash,
            )
            .unwrap();

        // Second insert with same path must fail (UNIQUE INDEX)
        let result = store.insert(
            &pak_path,
            created_at,
            fact_count,
            edge_count,
            fact_id_min,
            fact_id_max,
            t_created_min,
            t_created_max,
            size_bytes,
            &blake3_hash,
        );
        assert!(result.is_err(), "duplicate pak_path should be rejected");
    }

    #[test]
    fn delete_manifest_entry() {
        let conn = setup();
        let store = ArchiveManifestStore::new(&conn);

        let (
            pak_path,
            created_at,
            fact_count,
            edge_count,
            fact_id_min,
            fact_id_max,
            t_created_min,
            t_created_max,
            size_bytes,
            blake3_hash,
        ) = make_entry("archives/to_delete.pak");

        let id = store
            .insert(
                &pak_path,
                created_at,
                fact_count,
                edge_count,
                fact_id_min,
                fact_id_max,
                t_created_min,
                t_created_max,
                size_bytes,
                &blake3_hash,
            )
            .unwrap();

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
