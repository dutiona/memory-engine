use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use crate::error::{MemoryError, Result};
use crate::store::{parse_optional_timestamp, parse_timestamp};
use crate::types::{Edge, NewEdge};

/// Store for graph edges with bi-temporal support.
pub struct EdgeStore<'a> {
    conn: &'a Connection,
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    let t_created_str: String = row.get("t_created")?;
    let t_expired_str: Option<String> = row.get("t_expired")?;

    Ok(Edge {
        id: row.get("id")?,
        source_fact_id: row.get("source_fact_id")?,
        target_fact_id: row.get("target_fact_id")?,
        relation_type: row.get("relation_type")?,
        weight: row.get("weight")?,
        t_created: parse_timestamp(&t_created_str)?,
        t_expired: parse_optional_timestamp(t_expired_str.as_deref())?,
        scope_id: row.get("scope_id")?,
    })
}

impl<'a> EdgeStore<'a> {
    /// Create a new `EdgeStore` borrowing the given connection.
    #[must_use]
    pub const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a new edge. Returns the assigned row id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure (e.g. FK violation).
    pub fn insert(&self, edge: &NewEdge) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO edges (source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                edge.source_fact_id,
                edge.target_fact_id,
                edge.relation_type,
                edge.weight,
                edge.t_created.to_rfc3339(),
                edge.t_expired.map(|dt| dt.to_rfc3339()),
                edge.scope_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get an edge by id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the edge doesn't exist.
    pub fn get(&self, id: i64) -> Result<Edge> {
        self.conn
            .query_row(
                "SELECT id, source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id
                 FROM edges WHERE id = ?1",
                params![id],
                row_to_edge,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    MemoryError::NotFound(format!("edge {id}"))
                }
                other => MemoryError::Database(other),
            })
    }

    /// Expire an edge (soft-delete).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn expire(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET t_expired = ?1 WHERE id = ?2",
            params![now.to_rfc3339(), id],
        )?;
        Ok(())
    }

    /// Expire all active edges involving a given fact (as source or target).
    ///
    /// Used by conflict resolution for edge cascade on fact expiry.
    /// Returns the number of edges expired.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn expire_by_fact(&self, fact_id: i64, now: DateTime<Utc>) -> Result<usize> {
        let count = self.conn.execute(
            "UPDATE edges SET t_expired = ?1
             WHERE (source_fact_id = ?2 OR target_fact_id = ?2)
               AND t_expired IS NULL",
            params![now.to_rfc3339(), fact_id],
        )?;
        Ok(count)
    }

    /// List ALL edges (including expired). Used for state dumps.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_all(&self) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id
             FROM edges ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], row_to_edge)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// List all active edges (`t_expired IS NULL`).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_active(&self) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id
             FROM edges WHERE t_expired IS NULL",
        )?;
        let edges = stmt
            .query_map([], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(edges)
    }

    /// List active edges by source fact id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_active_by_source(&self, source_fact_id: i64) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id
             FROM edges WHERE source_fact_id = ?1 AND t_expired IS NULL",
        )?;
        let edges = stmt
            .query_map(params![source_fact_id], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(edges)
    }

    /// Check if an active edge exists between two facts with a given relation type.
    ///
    /// Used as a dedup guard for idempotent edge creation (e.g. `co_session` edges).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn exists_active(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation_type: &str,
    ) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM edges
                 WHERE source_fact_id = ?1
                   AND target_fact_id = ?2
                   AND relation_type = ?3
                   AND t_expired IS NULL",
                params![source_fact_id, target_fact_id, relation_type],
                |row| row.get(0),
            )
            .map_err(MemoryError::Database)?;
        Ok(count > 0)
    }

    /// List active edges by target fact id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_active_by_target(&self, target_fact_id: i64) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id
             FROM edges WHERE target_fact_id = ?1 AND t_expired IS NULL",
        )?;
        let edges = stmt
            .query_map(params![target_fact_id], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        // Insert dummy facts for FK constraints (minimal valid rows)
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata)
             VALUES ('fact1', 'h1', X'00000000', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata)
             VALUES ('fact2', 'h2', X'00000000', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata)
             VALUES ('fact3', 'h3', X'00000000', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}')",
            [],
        ).unwrap();
        conn
    }

    fn make_edge(source: i64, target: i64, rel: &str) -> NewEdge {
        NewEdge {
            source_fact_id: source,
            target_fact_id: target,
            relation_type: rel.to_string(),
            weight: 1.0,
            t_created: Utc::now(),
            t_expired: None,
            scope_id: 1,
        }
    }

    #[test]
    fn insert_and_get_edge() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        let edge = make_edge(1, 2, "related_to");
        let id = store.insert(&edge).unwrap();
        let got = store.get(id).unwrap();
        assert_eq!(got.source_fact_id, 1);
        assert_eq!(got.target_fact_id, 2);
        assert_eq!(got.relation_type, "related_to");
        assert!((got.weight - 1.0).abs() < f64::EPSILON);
        assert!(got.t_expired.is_none());
    }

    #[test]
    fn expire_edge() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        let id = store.insert(&make_edge(1, 2, "test")).unwrap();
        store.expire(id, Utc::now()).unwrap();
        let got = store.get(id).unwrap();
        assert!(got.t_expired.is_some());
    }

    #[test]
    fn list_active_excludes_expired() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        let id1 = store.insert(&make_edge(1, 2, "a")).unwrap();
        store.insert(&make_edge(1, 3, "b")).unwrap();
        store.expire(id1, Utc::now()).unwrap();

        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].relation_type, "b");
    }

    #[test]
    fn list_active_by_source() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        store.insert(&make_edge(1, 2, "a")).unwrap();
        store.insert(&make_edge(1, 3, "b")).unwrap();
        store.insert(&make_edge(2, 3, "c")).unwrap();

        let from_1 = store.list_active_by_source(1).unwrap();
        assert_eq!(from_1.len(), 2);

        let from_2 = store.list_active_by_source(2).unwrap();
        assert_eq!(from_2.len(), 1);
    }

    #[test]
    fn list_active_by_target() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        store.insert(&make_edge(1, 3, "a")).unwrap();
        store.insert(&make_edge(2, 3, "b")).unwrap();

        let to_3 = store.list_active_by_target(3).unwrap();
        assert_eq!(to_3.len(), 2);
    }

    #[test]
    fn exists_active_returns_correct_status() {
        let conn = setup();
        let store = EdgeStore::new(&conn);

        // No edges yet
        assert!(!store.exists_active(1, 2, "co_session").unwrap());

        // Insert a co_session edge
        store.insert(&make_edge(1, 2, "co_session")).unwrap();
        assert!(store.exists_active(1, 2, "co_session").unwrap());

        // Wrong direction
        assert!(!store.exists_active(2, 1, "co_session").unwrap());

        // Wrong relation type
        assert!(!store.exists_active(1, 2, "supplements").unwrap());

        // Expire the edge
        let edges = store.list_active_by_source(1).unwrap();
        store.expire(edges[0].id, Utc::now()).unwrap();
        assert!(!store.exists_active(1, 2, "co_session").unwrap());
    }

    #[test]
    fn expire_by_fact_cascades() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        store.insert(&make_edge(1, 2, "a")).unwrap();
        store.insert(&make_edge(1, 3, "b")).unwrap();
        store.insert(&make_edge(2, 1, "c")).unwrap(); // target = 1
        store.insert(&make_edge(2, 3, "d")).unwrap(); // unrelated

        let expired_count = store.expire_by_fact(1, Utc::now()).unwrap();
        assert_eq!(expired_count, 3); // edges a, b, c all involve fact 1

        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].relation_type, "d");
    }
}
