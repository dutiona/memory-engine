use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

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

#[allow(dead_code)] // complete CRUD API — not all methods called through engine facade yet
impl<'a> EdgeStore<'a> {
    /// Create a new `EdgeStore` borrowing the given connection.
    #[must_use]
    pub(crate) const fn new(conn: &'a Connection) -> Self {
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
    /// Idempotency guard (`t_expired IS NULL`): only an active edge is affected,
    /// so a successful `Ok(())` always means *this* call transitioned an active
    /// edge to expired. Mirrors [`FactStore::expire`](crate::store::facts::FactStore::expire).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no active edge with `id` exists (the id
    /// is unknown, or the edge was already expired) — the UPDATE affected 0 rows.
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn expire(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE edges SET t_expired = ?1 WHERE id = ?2 AND t_expired IS NULL",
            params![now.to_rfc3339(), id],
        )?;
        if changed == 0 {
            return Err(MemoryError::NotFound(format!("edge {id}")));
        }
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

    /// Iterate all edges row-by-row, calling `f` for each.
    ///
    /// Unlike [`Self::list_all`], this never allocates a `Vec` — each edge is
    /// deserialized, passed to the callback, and dropped before the next
    /// row is read.  Suitable for streaming serialization of large databases.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(Edge) -> Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id
             FROM edges ORDER BY id ASC",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let edge = row_to_edge(row)?;
            f(edge)?;
        }
        Ok(())
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
    /// Useful as a per-pair dedup guard for edge creation.
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

    /// Batch-fetch all active edges of a given relation type involving any of the given fact IDs.
    ///
    /// Returns `(source_fact_id, target_fact_id)` pairs. Used for efficient dedup
    /// before bulk edge creation (avoids N² per-pair SQL queries).
    ///
    /// # Panics
    ///
    /// Panics if `fact_ids` cannot be serialized to JSON (should never happen
    /// for `&[i64]`).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn list_active_pairs_by_facts(
        &self,
        fact_ids: &[i64],
        relation_type: &str,
    ) -> Result<std::collections::HashSet<(i64, i64)>> {
        use std::collections::HashSet;
        if fact_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let ids_json = serde_json::to_string(fact_ids).expect("serialize fact_ids");
        let mut stmt = self.conn.prepare(
            "SELECT source_fact_id, target_fact_id FROM edges
             WHERE source_fact_id IN (SELECT value FROM json_each(?1))
               AND target_fact_id IN (SELECT value FROM json_each(?1))
               AND relation_type = ?2
               AND t_expired IS NULL",
        )?;
        let rows = stmt.query_map(params![ids_json, relation_type], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut set = HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
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

    /// List edges (including expired) where BOTH endpoints are in the given
    /// fact ID set, ordered by id ascending.
    ///
    /// This is the read counterpart of [`Self::hard_delete_by_facts`] — it
    /// selects exactly the set of edges that archival will remove, pushing the
    /// "both endpoints internal" predicate into SQL so the engine never loads
    /// every edge just to discard the ones that straddle the live/archived
    /// boundary. Equivalent to the prior Rust-side filter
    /// `candidate_ids.contains(&e.source_fact_id) && candidate_ids.contains(&e.target_fact_id)`
    /// over `list_all()`.
    ///
    /// Returns an empty `Vec` for an empty `fact_ids` slice (no query issued).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure, or
    /// `MemoryError::Serialization` if the `fact_ids` slice cannot be serialized
    /// to the JSON array bound into the query (infallible in practice for
    /// `&[i64]`).
    pub fn list_internal_by_facts(&self, fact_ids: &[i64]) -> Result<Vec<Edge>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(fact_ids)?;
        // Parse the JSON id array exactly once via a CTE, then reuse it for both
        // endpoint predicates (vs. evaluating json_each(?1) twice).
        let mut stmt = self.conn.prepare(
            "WITH ids(value) AS (SELECT value FROM json_each(?1))
             SELECT id, source_fact_id, target_fact_id, relation_type, weight, t_created, t_expired, scope_id
             FROM edges
             WHERE source_fact_id IN (SELECT value FROM ids)
               AND target_fact_id IN (SELECT value FROM ids)
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![ids_json], row_to_edge)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Hard-delete edges where BOTH endpoints are in the given fact ID set.
    ///
    /// Used after successful archival to remove edges whose facts have been
    /// moved to a `.pak` file. Edges where only one endpoint is archived are
    /// left intact (they may reference facts still in the live DB).
    ///
    /// Returns the number of rows deleted.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn hard_delete_by_facts(&self, fact_ids: &[i64]) -> Result<usize> {
        if fact_ids.is_empty() {
            return Ok(0);
        }
        let placeholders: String = fact_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "DELETE FROM edges WHERE source_fact_id IN ({placeholders}) AND target_fact_id IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(fact_ids.len() * 2);
        for id in fact_ids {
            params.push(id as &dyn rusqlite::types::ToSql);
        }
        for id in fact_ids {
            params.push(id as &dyn rusqlite::types::ToSql);
        }
        let deleted = self.conn.execute(&sql, params.as_slice())?;
        Ok(deleted)
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
    fn expire_nonexistent_edge_is_not_found() {
        // #330: expiring an id that matches no row is an error, not a silent
        // no-op. `Ok(())` must mean an edge was actually expired — mirroring
        // `FactStore::expire`'s rows-affected contract.
        let conn = setup();
        let store = EdgeStore::new(&conn);
        let err = store
            .expire(9999, Utc::now())
            .expect_err("expiring a nonexistent edge id must return NotFound");
        assert!(
            matches!(err, MemoryError::NotFound(_)),
            "expected MemoryError::NotFound, got {err:?}"
        );
    }

    #[test]
    fn expire_already_expired_edge_is_not_found() {
        // The `t_expired IS NULL` guard makes re-expiring an already-expired
        // edge a no-op at the SQL level, which the rows-affected check surfaces
        // as NotFound — identical to `FactStore::expire`. This keeps `Ok(())`
        // meaning "this call transitioned an active edge to expired".
        let conn = setup();
        let store = EdgeStore::new(&conn);
        let id = store.insert(&make_edge(1, 2, "test")).unwrap();
        store.expire(id, Utc::now()).unwrap();
        let err = store
            .expire(id, Utc::now())
            .expect_err("re-expiring an already-expired edge must return NotFound");
        assert!(
            matches!(err, MemoryError::NotFound(_)),
            "expected MemoryError::NotFound, got {err:?}"
        );
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

    // --- list_internal_by_facts tests ---

    #[test]
    fn list_internal_by_facts_matches_rust_both_endpoints_filter() {
        // Equivalence guard for the SQL-pushdown refactor (#349): the new
        // `list_internal_by_facts` must return exactly the edges the prior
        // archive-side Rust filter did — `candidate_ids.contains(&e.source) &&
        // candidate_ids.contains(&e.target)` over `list_all()` — i.e. both
        // endpoints inside the candidate set.
        //
        // facts: 1, 2, 3. Edge f1→f2 is internal to {1,2}; f2→f3 straddles
        // the live/archived boundary (f3 not in set) and must be excluded.
        let conn = setup();
        let store = EdgeStore::new(&conn);

        let internal = store.insert(&make_edge(1, 2, "internal")).unwrap();
        store.insert(&make_edge(2, 3, "cross_boundary")).unwrap();

        // Reference: the prior Rust-side semantics over the full edge list.
        let set: std::collections::HashSet<i64> = [1, 2].into_iter().collect();
        let expected: Vec<i64> = store
            .list_all()
            .unwrap()
            .into_iter()
            .filter(|e| set.contains(&e.source_fact_id) && set.contains(&e.target_fact_id))
            .map(|e| e.id)
            .collect();

        let got: Vec<i64> = store
            .list_internal_by_facts(&[1, 2])
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();

        assert_eq!(
            got, expected,
            "SQL pushdown must match the prior Rust filter"
        );
        assert_eq!(got, vec![internal], "only the fully-internal edge");
    }

    #[test]
    fn list_internal_by_facts_includes_expired_edges() {
        // Archival removes edges whose BOTH facts are archived regardless of the
        // edge's own t_expired (the prior code filtered `list_all()`, which
        // returns expired edges too — unlike `list_active`). This guards against
        // a regression that would orphan an expired edge whose endpoints are gone.
        let conn = setup();
        let store = EdgeStore::new(&conn);

        let active = store.insert(&make_edge(1, 2, "active")).unwrap();
        let expired = store.insert(&make_edge(2, 1, "expired")).unwrap();
        store.expire(expired, Utc::now()).unwrap();

        let got: Vec<i64> = store
            .list_internal_by_facts(&[1, 2])
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();

        assert_eq!(
            got,
            vec![active, expired],
            "both active and expired internal edges, id-ascending"
        );
    }

    #[test]
    fn list_internal_by_facts_empty_slice_is_noop() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        store.insert(&make_edge(1, 2, "a")).unwrap();

        assert!(
            store.list_internal_by_facts(&[]).unwrap().is_empty(),
            "empty fact-id set selects no edges (no query issued)"
        );
    }

    // --- hard_delete_by_facts tests ---

    #[test]
    fn hard_delete_by_facts_only_deletes_internal_edges() {
        // facts: 1, 2, 3
        // edges: f1→f2 (both in archive set), f2→f3 (f3 not in set)
        // delete by [f1, f2] — only f1→f2 should be deleted
        let conn = setup();
        let store = EdgeStore::new(&conn);

        store.insert(&make_edge(1, 2, "internal")).unwrap();
        store.insert(&make_edge(2, 3, "cross_boundary")).unwrap();

        let deleted = store.hard_delete_by_facts(&[1, 2]).unwrap();
        assert_eq!(deleted, 1, "only the fully-internal edge should be deleted");

        let remaining = store.list_all().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].relation_type, "cross_boundary");
    }

    #[test]
    fn hard_delete_by_facts_empty_slice_is_noop() {
        let conn = setup();
        let store = EdgeStore::new(&conn);
        store.insert(&make_edge(1, 2, "a")).unwrap();

        let deleted = store.hard_delete_by_facts(&[]).unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(store.list_all().unwrap().len(), 1);
    }

    #[test]
    fn hard_delete_by_facts_removes_both_directions() {
        // Both f1→f2 and f2→f1 are internal when set is [f1, f2]
        let conn = setup();
        let store = EdgeStore::new(&conn);

        store.insert(&make_edge(1, 2, "forward")).unwrap();
        store.insert(&make_edge(2, 1, "backward")).unwrap();
        store.insert(&make_edge(1, 3, "external")).unwrap(); // f3 not in set

        let deleted = store.hard_delete_by_facts(&[1, 2]).unwrap();
        assert_eq!(deleted, 2);

        let remaining = store.list_all().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].relation_type, "external");
    }
}
