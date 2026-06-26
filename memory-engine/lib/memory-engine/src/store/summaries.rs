use rusqlite::{Connection, params};

use crate::error::{MemoryError, Result};
use crate::store::{deserialize_embedding, parse_timestamp, serialize_embedding};
use crate::types::{ConsolidationLevel, NewSummary, Summary};

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
/// row mapper and [`crate::inspect::statistics::compute_statistics`] route
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
    #[must_use]
    pub(crate) const fn new(conn: &'a Connection, embed_dim: usize) -> Self {
        Self { conn, embed_dim }
    }

    /// Insert a new summary. Returns the assigned row id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if the embedding length
    /// doesn't match `embed_dim`.
    /// Returns `MemoryError::Database` on SQL failure.
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
        )?;
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
                other => MemoryError::Database(other),
            })
    }

    /// List summaries by consolidation level.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_by_level(&self, level: &ConsolidationLevel) -> Result<Vec<Summary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, embedding, level, source_fact_ids, created_at, scope_id
             FROM summaries WHERE level = ?1",
        )?;
        let summaries = stmt
            .query_map(params![level_to_str(level)], |row| self.row_to_summary(row))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(summaries)
    }

    /// List ALL summaries. Used for state dumps.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_all(&self) -> Result<Vec<Summary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, embedding, level, source_fact_ids, created_at, scope_id
             FROM summaries ORDER BY id ASC",
        )?;
        let summaries = stmt
            .query_map([], |row| self.row_to_summary(row))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
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
    /// Returns `MemoryError::Database` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(Summary) -> Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, embedding, level, source_fact_ids, created_at, scope_id
             FROM summaries ORDER BY id ASC",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let summary = self.row_to_summary(row)?;
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
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn delete_by_level(&self, level: &ConsolidationLevel) -> Result<usize> {
        let count = self.conn.execute(
            "DELETE FROM summaries WHERE level = ?1",
            params![level_to_str(level)],
        )?;
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
}
