use me_types::error::StorageError;
use rusqlite::{Connection, params};

use crate::store::{deserialize_embedding, parse_timestamp, serialize_embedding};
use me_types::error::{MemoryError, Result};
use me_types::types::{ConsolidationLevel, NewSummary, Summary};

#[must_use]
pub const fn level_to_str(level: &ConsolidationLevel) -> &'static str {
    match level {
        ConsolidationLevel::Local => "local",
        ConsolidationLevel::Cluster => "cluster",
        ConsolidationLevel::Global => "global",
    }
}

/// Parse a stored `level` string back into a [`ConsolidationLevel`].
///
/// This is the single fallible parser for the persisted level encoding produced
/// by [`level_to_str`]; it is the inverse of that function. Both the summary
/// row mapper and `compute_statistics` route
/// through it so an unrecognised level (a corrupted row, or a new variant that
/// missed an encoding update) surfaces as an error instead of being silently
/// dropped from the `by_level` histogram (#337).
///
/// # Errors
///
/// Returns [`rusqlite::Error::FromSqlConversionFailure`] if `s` is not one of
/// the encodings produced by [`level_to_str`].
pub fn str_to_level(s: &str) -> rusqlite::Result<ConsolidationLevel> {
    match s {
        "local" => Ok(ConsolidationLevel::Local),
        "cluster" => Ok(ConsolidationLevel::Cluster),
        "global" => Ok(ConsolidationLevel::Global),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::from(format!("unknown consolidation level: {other}")),
        )),
    }
}

/// Store for consolidation summaries.
pub struct SummaryStore<'a> {
    conn: &'a Connection,
    embed_dim: usize,
}

#[allow(dead_code)] // complete CRUD API — not all methods called through engine facade yet
impl<'a> SummaryStore<'a> {
    /// Create a new `SummaryStore` borrowing the given connection.
    // Stays `pub` (Wave 2 #816, me-backend-sqlite carve): `storage/sqlite/` joined
    // this crate in sub-PR 2b, but the facade's own `#[cfg(test)]` consolidation
    // tests (`consolidation/{cluster,global,mod}.rs`) still construct `SummaryStore`
    // directly across the crate boundary.
    #[must_use]
    pub const fn new(conn: &'a Connection, embed_dim: usize) -> Self {
        Self { conn, embed_dim }
    }

    /// Insert a new summary. Returns the assigned row id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if the embedding length
    /// doesn't match `embed_dim`.
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn insert(&self, summary: &NewSummary) -> Result<i64> {
        if summary.embedding.len() != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: summary.embedding.len(),
            });
        }

        let blob = serialize_embedding(&summary.embedding);
        let source_ids_json = serde_json::to_string(&summary.source_fact_ids)?;

        self.conn.execute(
            "INSERT INTO summaries (content, embedding, level, source_fact_ids, created_at, scope_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                summary.content,
                blob,
                level_to_str(&summary.level),
                source_ids_json,
                summary.created_at.to_rfc3339(),
                summary.scope_id,
            ],
        ).map_err(StorageError::backend)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get a summary by id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the summary doesn't exist.
    pub fn get(&self, id: i64) -> Result<Summary> {
        self.conn
            .query_row(
                "SELECT id, content, embedding, level, source_fact_ids, created_at, scope_id
                 FROM summaries WHERE id = ?1",
                params![id],
                |row| self.row_to_summary(row),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    MemoryError::NotFound(format!("summary {id}"))
                }
                other => StorageError::backend(other).into(),
            })
    }

    /// List summaries by consolidation level.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn list_by_level(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, content, embedding, level, source_fact_ids, created_at, scope_id
             FROM summaries WHERE level = ?1",
            )
            .map_err(StorageError::backend)?;
        let summaries = stmt
            .query_map(params![level_to_str(level)], |row| self.row_to_summary(row))
            .map_err(StorageError::backend)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::backend)?;
        Ok(summaries)
    }

    /// List ALL summaries. Used for state dumps.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn list_all(&self) -> Result<Vec<Summary>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, content, embedding, level, source_fact_ids, created_at, scope_id
             FROM summaries ORDER BY id ASC",
            )
            .map_err(StorageError::backend)?;
        let summaries = stmt
            .query_map([], |row| self.row_to_summary(row))
            .map_err(StorageError::backend)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::backend)?;
        Ok(summaries)
    }

    /// Iterate all summaries row-by-row, calling `f` for each.
    ///
    /// Unlike [`Self::list_all`], this never allocates a `Vec` — each summary
    /// is deserialized, passed to the callback, and dropped before the next
    /// row is read.  Suitable for streaming serialization of large databases.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(Summary) -> Result<()>,
    {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, content, embedding, level, source_fact_ids, created_at, scope_id
             FROM summaries ORDER BY id ASC",
            )
            .map_err(StorageError::backend)?;
        let mut rows = stmt.query([]).map_err(StorageError::backend)?;
        while let Some(row) = rows.next().map_err(StorageError::backend)? {
            let summary = self.row_to_summary(row).map_err(StorageError::backend)?;
            f(summary)?;
        }
        Ok(())
    }

    /// Delete all summaries at the given level. Returns count deleted.
    ///
    /// Used for idempotent consolidation rebuild.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn delete_by_level(&self, level: &ConsolidationLevel) -> Result<usize> {
        let count = self
            .conn
            .execute(
                "DELETE FROM summaries WHERE level = ?1",
                params![level_to_str(level)],
            )
            .map_err(StorageError::backend)?;
        Ok(count)
    }

    /// Map a row to a [`Summary`], deserializing embedding and `source_fact_ids`.
    ///
    /// # Errors
    ///
    /// Returns `rusqlite::Error` on deserialization failure.
    fn row_to_summary(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
        let blob: Vec<u8> = row.get("embedding")?;
        let embedding = deserialize_embedding(&blob, self.embed_dim).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Blob,
                Box::from(e.to_string()),
            )
        })?;

        let level_str: String = row.get("level")?;
        let level = str_to_level(&level_str)?;

        let source_ids_str: String = row.get("source_fact_ids")?;
        let source_fact_ids: Vec<i64> = serde_json::from_str(&source_ids_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;

        let created_at_str: String = row.get("created_at")?;
        let created_at = parse_timestamp(&created_at_str)?;

        Ok(Summary {
            id: row.get("id")?,
            content: row.get("content")?,
            embedding,
            level,
            source_fact_ids,
            created_at,
            scope_id: row.get("scope_id")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::store::schema::{init_schema, open_memory};

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_summary(level: ConsolidationLevel, source_ids: Vec<i64>) -> NewSummary {
        NewSummary {
            content: format!("summary of facts {source_ids:?}"),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            level,
            source_fact_ids: source_ids,
            created_at: Utc::now(),
            scope_id: 1,
        }
    }

    #[test]
    fn insert_and_get_summary() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        let summary = make_summary(ConsolidationLevel::Local, vec![1, 2]);
        let id = store.insert(&summary).unwrap();
        let got = store.get(id).unwrap();
        assert_eq!(got.content, summary.content);
        assert_eq!(got.level, ConsolidationLevel::Local);
        assert_eq!(got.source_fact_ids, vec![1, 2]);
        assert_eq!(got.embedding, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn list_by_level() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        store
            .insert(&make_summary(ConsolidationLevel::Local, vec![1]))
            .unwrap();
        store
            .insert(&make_summary(ConsolidationLevel::Local, vec![2]))
            .unwrap();
        store
            .insert(&make_summary(ConsolidationLevel::Cluster, vec![1, 2]))
            .unwrap();
        store
            .insert(&make_summary(ConsolidationLevel::Global, vec![1, 2, 3]))
            .unwrap();

        assert_eq!(
            store
                .list_by_level(&ConsolidationLevel::Local)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            store
                .list_by_level(&ConsolidationLevel::Cluster)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list_by_level(&ConsolidationLevel::Global)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn source_fact_ids_json_roundtrip() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        let ids = vec![10, 20, 30, 40, 50];
        let id = store
            .insert(&make_summary(ConsolidationLevel::Local, ids.clone()))
            .unwrap();
        let got = store.get(id).unwrap();
        assert_eq!(got.source_fact_ids, ids);
    }

    #[test]
    fn wrong_embedding_dim_rejected() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        let mut summary = make_summary(ConsolidationLevel::Local, vec![1]);
        summary.embedding = vec![0.1, 0.2]; // 2 instead of 4
        let err = store.insert(&summary).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::EmbeddingDimension {
                expected: 4,
                actual: 2
            }
        ));
    }

    #[test]
    fn delete_by_level_removes_correct_entries() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        store
            .insert(&make_summary(ConsolidationLevel::Cluster, vec![1]))
            .unwrap();
        store
            .insert(&make_summary(ConsolidationLevel::Cluster, vec![2]))
            .unwrap();
        store
            .insert(&make_summary(ConsolidationLevel::Global, vec![1, 2]))
            .unwrap();

        let deleted = store.delete_by_level(&ConsolidationLevel::Cluster).unwrap();
        assert_eq!(deleted, 2);
        assert!(
            store
                .list_by_level(&ConsolidationLevel::Cluster)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .list_by_level(&ConsolidationLevel::Global)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn get_not_found() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        let err = store.get(999).unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));
    }

    #[test]
    fn list_all_empty_store_returns_empty() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        assert!(store.list_all().unwrap().is_empty());
    }

    #[test]
    fn list_all_returns_every_summary_in_id_order() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        // Insert across levels with asymmetric source ids so each content string
        // is distinct — a tuple-swap or dropped row would change the sequence.
        let id1 = store
            .insert(&make_summary(ConsolidationLevel::Local, vec![1]))
            .unwrap();
        let id2 = store
            .insert(&make_summary(ConsolidationLevel::Cluster, vec![2, 3]))
            .unwrap();
        let id3 = store
            .insert(&make_summary(ConsolidationLevel::Global, vec![4, 5, 6]))
            .unwrap();

        let all = store.list_all().unwrap();
        // All three rows, regardless of level (list_all is not level-filtered).
        assert_eq!(all.len(), 3);
        // ORDER BY id ASC — assert the exact ordered id sequence so a reorder
        // (e.g. DESC) would fail.
        assert_eq!(
            all.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![id1, id2, id3]
        );
        // Content/level travel with the right row — asymmetric so a swap is caught.
        assert_eq!(all[0].level, ConsolidationLevel::Local);
        assert_eq!(all[0].source_fact_ids, vec![1]);
        assert_eq!(all[1].level, ConsolidationLevel::Cluster);
        assert_eq!(all[1].source_fact_ids, vec![2, 3]);
        assert_eq!(all[2].level, ConsolidationLevel::Global);
        assert_eq!(all[2].source_fact_ids, vec![4, 5, 6]);
    }

    #[test]
    fn for_each_visits_every_summary_exactly_once_in_order() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        let id1 = store
            .insert(&make_summary(ConsolidationLevel::Local, vec![1]))
            .unwrap();
        let id2 = store
            .insert(&make_summary(ConsolidationLevel::Cluster, vec![2, 3]))
            .unwrap();
        let id3 = store
            .insert(&make_summary(ConsolidationLevel::Global, vec![4, 5, 6]))
            .unwrap();

        let mut visited = Vec::new();
        store
            .for_each(|s| {
                visited.push((s.id, s.level, s.source_fact_ids));
                Ok(())
            })
            .unwrap();

        // Exactly-once and ORDER BY id ASC: the full ordered tuple sequence.
        assert_eq!(
            visited,
            vec![
                (id1, ConsolidationLevel::Local, vec![1]),
                (id2, ConsolidationLevel::Cluster, vec![2, 3]),
                (id3, ConsolidationLevel::Global, vec![4, 5, 6]),
            ]
        );
    }

    #[test]
    fn for_each_on_empty_store_never_calls_callback() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        let mut calls = 0_usize;
        store
            .for_each(|_| {
                calls += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, 0);
    }

    #[test]
    fn for_each_propagates_callback_error_and_stops_iteration() {
        let conn = setup();
        let store = SummaryStore::new(&conn, 4);
        store
            .insert(&make_summary(ConsolidationLevel::Local, vec![1]))
            .unwrap();
        store
            .insert(&make_summary(ConsolidationLevel::Cluster, vec![2]))
            .unwrap();
        store
            .insert(&make_summary(ConsolidationLevel::Global, vec![3]))
            .unwrap();

        // Fail on the second visited row: proves the error surfaces AND that
        // iteration halts (the third row is never seen).
        let mut seen = 0_usize;
        let err = store
            .for_each(|_| {
                seen += 1;
                if seen == 2 {
                    Err(MemoryError::NotFound("boom".to_string()))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(msg) if msg == "boom"));
        assert_eq!(seen, 2, "iteration must stop at the failing callback");
    }
}
