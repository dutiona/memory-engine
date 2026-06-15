//! Store operations for session checkpoints.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{MemoryError, Result};
use crate::types::SessionCheckpoint;

use super::parse_timestamp;

/// Store facade for the `session_checkpoints` table.
pub struct CheckpointStore<'a> {
    conn: &'a Connection,
}

impl<'a> CheckpointStore<'a> {
    pub(crate) const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Upsert a session checkpoint (last-write-wins per `session_id`).
    pub fn upsert(&self, checkpoint: &SessionCheckpoint) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO session_checkpoints
                 (session_id, scope_path, summary, last_activity_id, checkpoint_at, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(session_id) DO UPDATE SET
                     scope_path = excluded.scope_path,
                     summary = excluded.summary,
                     last_activity_id = excluded.last_activity_id,
                     checkpoint_at = excluded.checkpoint_at,
                     metadata = excluded.metadata",
                params![
                    checkpoint.session_id,
                    checkpoint.scope_path,
                    checkpoint.summary,
                    checkpoint.last_activity_id,
                    checkpoint.checkpoint_at.to_rfc3339(),
                    checkpoint.metadata.to_string(),
                ],
            )
            .map_err(MemoryError::Database)?;
        Ok(())
    }

    /// Get a checkpoint by `session_id`.
    ///
    /// Currently exercised only by unit tests; gated to keep the lib target
    /// `dead_code`-clean until wired into the engine/MCP (see #95/#96).
    #[cfg(test)]
    pub fn get(&self, session_id: &str) -> Result<Option<SessionCheckpoint>> {
        self.conn
            .query_row(
                "SELECT session_id, scope_path, summary, last_activity_id, checkpoint_at, metadata
                 FROM session_checkpoints
                 WHERE session_id = ?1",
                params![session_id],
                row_to_checkpoint,
            )
            .optional()
            .map_err(MemoryError::Database)
    }

    /// Get the most recent checkpoint for a scope path.
    pub fn get_by_scope(&self, scope_path: &str) -> Result<Option<SessionCheckpoint>> {
        self.conn
            .query_row(
                "SELECT session_id, scope_path, summary, last_activity_id, checkpoint_at, metadata
                 FROM session_checkpoints
                 WHERE scope_path = ?1
                 ORDER BY checkpoint_at DESC
                 LIMIT 1",
                params![scope_path],
                row_to_checkpoint,
            )
            .optional()
            .map_err(MemoryError::Database)
    }

    /// List recent checkpoints, most recent first.
    ///
    /// Currently exercised only by unit tests; gated to keep the lib target
    /// `dead_code`-clean until wired into the engine/MCP (see #95/#96).
    #[cfg(test)]
    pub fn list_recent(&self, limit: usize) -> Result<Vec<SessionCheckpoint>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, scope_path, summary, last_activity_id, checkpoint_at, metadata
                 FROM session_checkpoints
                 ORDER BY checkpoint_at DESC
                 LIMIT ?1",
            )
            .map_err(MemoryError::Database)?;
        let rows = stmt
            .query_map(
                params![i64::try_from(limit).unwrap_or(i64::MAX)],
                row_to_checkpoint,
            )
            .map_err(MemoryError::Database)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(MemoryError::Database)
    }
}

fn row_to_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionCheckpoint> {
    let checkpoint_at_str: String = row.get(4)?;
    let metadata_str: String = row.get(5)?;
    let metadata: serde_json::Value = serde_json::from_str(&metadata_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    Ok(SessionCheckpoint {
        session_id: row.get(0)?,
        scope_path: row.get(1)?,
        summary: row.get(2)?,
        last_activity_id: row.get(3)?,
        checkpoint_at: parse_timestamp(&checkpoint_at_str)?,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::init_schema;
    use chrono::Utc;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn upsert_insert_and_get() {
        let conn = setup();
        let store = CheckpointStore::new(&conn);
        let cp = SessionCheckpoint {
            session_id: "sess-1".into(),
            scope_path: Some("project:memory-engine".into()),
            summary: Some("worked on activity stream".into()),
            last_activity_id: None,
            checkpoint_at: Utc::now(),
            metadata: serde_json::json!({"tool_count": 42}),
        };
        store.upsert(&cp).unwrap();

        let fetched = store.get("sess-1").unwrap().unwrap();
        assert_eq!(fetched.session_id, "sess-1");
        assert_eq!(fetched.scope_path, Some("project:memory-engine".into()));
        assert_eq!(fetched.summary, Some("worked on activity stream".into()));
    }

    #[test]
    fn upsert_overwrites() {
        let conn = setup();
        let store = CheckpointStore::new(&conn);
        let cp1 = SessionCheckpoint {
            session_id: "sess-1".into(),
            scope_path: Some("project:foo".into()),
            summary: Some("first".into()),
            last_activity_id: None,
            checkpoint_at: Utc::now(),
            metadata: serde_json::json!({}),
        };
        store.upsert(&cp1).unwrap();

        let cp2 = SessionCheckpoint {
            summary: Some("second".into()),
            ..cp1
        };
        store.upsert(&cp2).unwrap();

        let fetched = store.get("sess-1").unwrap().unwrap();
        assert_eq!(fetched.summary, Some("second".into()));
    }

    #[test]
    fn get_by_scope() {
        let conn = setup();
        let store = CheckpointStore::new(&conn);
        let cp = SessionCheckpoint {
            session_id: "sess-1".into(),
            scope_path: Some("project:memory-engine".into()),
            summary: None,
            last_activity_id: None,
            checkpoint_at: Utc::now(),
            metadata: serde_json::json!({}),
        };
        store.upsert(&cp).unwrap();

        let found = store
            .get_by_scope("project:memory-engine")
            .unwrap()
            .unwrap();
        assert_eq!(found.session_id, "sess-1");

        let not_found = store.get_by_scope("project:other").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let conn = setup();
        let store = CheckpointStore::new(&conn);
        assert!(store.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_recent() {
        let conn = setup();
        let store = CheckpointStore::new(&conn);
        for i in 0..5 {
            let cp = SessionCheckpoint {
                session_id: format!("sess-{i}"),
                scope_path: Some("project:test".into()),
                summary: None,
                last_activity_id: None,
                checkpoint_at: Utc::now(),
                metadata: serde_json::json!({}),
            };
            store.upsert(&cp).unwrap();
        }
        let list = store.list_recent(3).unwrap();
        assert_eq!(list.len(), 3);
    }
}
