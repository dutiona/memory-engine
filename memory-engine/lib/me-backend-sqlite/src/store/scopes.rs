use me_types::error::StorageError;
use rusqlite::Connection;

use me_types::error::{ConflictError, MemoryError, Result};
use me_types::types::ScopeNode;

/// CRUD operations for the `scopes` table.
pub struct ScopeStore<'a> {
    conn: &'a Connection,
}

#[allow(dead_code)] // complete CRUD API — not all methods called through engine facade yet
impl<'a> ScopeStore<'a> {
    // TRANSIENT widening pub(crate) -> pub (Wave 2 #816, me-backend-sqlite carve,
    // sub-PR 2a): `storage/sqlite/{graph,consolidation}.rs` construct `ScopeStore`
    // from the facade; reverts to `pub(crate)` in sub-PR 2b.
    pub const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Get a scope by id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if no scope with `id` exists.
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn get(&self, id: i64) -> Result<ScopeNode> {
        self.conn
            .query_row(
                "SELECT id, parent_id, label, depth FROM scopes WHERE id = ?1",
                [id],
                |row| {
                    Ok(ScopeNode {
                        id: row.get(0)?,
                        parent_id: row.get(1)?,
                        label: row.get(2)?,
                        depth: row.get(3)?,
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    MemoryError::NotFound(format!("scope id={id}"))
                }
                other => StorageError::backend(other).into(),
            })
    }

    /// Find a scope by `parent_id` + label.
    ///
    /// Returns `Ok(None)` if no match is found.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn find_by_label(&self, parent_id: i64, label: &str) -> Result<Option<ScopeNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, label, depth FROM scopes WHERE parent_id = ?1 AND label = ?2",
        ).map_err(StorageError::backend)?;
        let mut rows = stmt
            .query_map(rusqlite::params![parent_id, label], |row| {
                Ok(ScopeNode {
                    id: row.get(0)?,
                    parent_id: row.get(1)?,
                    label: row.get(2)?,
                    depth: row.get(3)?,
                })
            })
            .map_err(StorageError::backend)?;
        match rows.next() {
            Some(Ok(node)) => Ok(Some(node)),
            Some(Err(e)) => Err(StorageError::backend(e).into()),
            None => Ok(None),
        }
    }

    /// Insert a scope under a parent. Returns the created node.
    ///
    /// **Does not validate the label.** Unlike [`Self::ensure_path`], this
    /// low-level method inserts `label` verbatim — callers are responsible for
    /// ensuring it is non-empty, contains no `/`, has no leading/trailing
    /// whitespace, and is at most [`me_types::types::MAX_SEGMENT_LEN`] bytes (the
    /// rules enforced by [`me_types::types::validate_segment`]). Storing a malformed
    /// label here can break round-tripping through
    /// `ScopeTree::resolve_path`. Prefer [`Self::ensure_path`]
    /// for a validated, idempotent alternative.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure (e.g. FK violation if
    /// `parent_id` does not exist, or a UNIQUE constraint on `(parent_id, label)`).
    pub fn insert(&self, parent_id: i64, label: &str, depth: i64) -> Result<ScopeNode> {
        self.conn
            .execute(
                "INSERT INTO scopes (parent_id, label, depth) VALUES (?1, ?2, ?3)",
                rusqlite::params![parent_id, label, depth],
            )
            .map_err(StorageError::backend)?;
        let id = self.conn.last_insert_rowid();
        Ok(ScopeNode {
            id,
            parent_id: Some(parent_id),
            label: label.to_string(),
            depth,
        })
    }

    /// Validate a scope label segment (write path).
    ///
    /// Applies the shared structural rules from [`me_types::types::validate_segment`]
    /// (non-empty, no `/`, at most 256 bytes) on the trimmed label, plus the
    /// write-path-only rule that the label must have no leading/trailing
    /// whitespace — so that stored labels always round-trip through
    /// `ScopeTree::resolve_path`.
    fn validate_label(label: &str) -> Result<()> {
        let trimmed = label.trim();
        me_types::types::validate_segment(trimmed)
            .map_err(|reason| MemoryError::Conflict(ConflictError::ScopeLabel(reason.into())))?;
        if trimmed != label {
            return Err(MemoryError::Conflict(ConflictError::ScopeLabel(
                "scope label must not have leading/trailing whitespace".into(),
            )));
        }
        Ok(())
    }

    /// Resolve a path string to a `scope_id`, creating missing nodes.
    ///
    /// Path format: `"user:michael/machine:desktop/project:memory-engine"`
    ///
    /// All top-level segments are children of root (id=1). Uses
    /// `INSERT OR IGNORE` + `SELECT` for race-safe creation.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if `path` is empty or any segment is
    /// invalid (empty, contains `/`, has leading/trailing whitespace, or exceeds
    /// [`me_types::types::MAX_SEGMENT_LEN`] bytes).
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn ensure_path(&self, path: &str) -> Result<i64> {
        if path.is_empty() {
            return Err(MemoryError::Conflict(ConflictError::ScopeLabel(
                "scope path must not be empty".into(),
            )));
        }

        let segments: Vec<&str> = path.split('/').collect();

        let mut parent_id: i64 = 1; // root

        for (depth, segment) in (1_i64..).zip(&segments) {
            Self::validate_label(segment)?;

            // INSERT OR IGNORE: if the scope already exists, this is a no-op.
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO scopes (parent_id, label, depth) VALUES (?1, ?2, ?3)",
                    rusqlite::params![parent_id, segment, depth],
                )
                .map_err(StorageError::backend)?;

            // SELECT: get the id (whether just inserted or already existed).
            let id: i64 = self
                .conn
                .query_row(
                    "SELECT id FROM scopes WHERE parent_id = ?1 AND label = ?2",
                    rusqlite::params![parent_id, segment],
                    |row| row.get(0),
                )
                .map_err(StorageError::backend)?;

            parent_id = id;
        }

        Ok(parent_id) // parent_id is now the leaf scope_id
    }

    /// List all scopes.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure.
    pub fn list_all(&self) -> Result<Vec<ScopeNode>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, parent_id, label, depth FROM scopes ORDER BY id")
            .map_err(StorageError::backend)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ScopeNode {
                    id: row.get(0)?,
                    parent_id: row.get(1)?,
                    label: row.get(2)?,
                    depth: row.get(3)?,
                })
            })
            .map_err(StorageError::backend)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| StorageError::backend(e).into())
    }
    /// Iterate all scopes row-by-row, calling `f` for each.
    ///
    /// Unlike [`Self::list_all`], this never allocates a `Vec` — each scope
    /// is read, passed to the callback, and dropped before the next row.
    /// Suitable for streaming serialization of large databases.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(ScopeNode) -> Result<()>,
    {
        let mut stmt = self
            .conn
            .prepare("SELECT id, parent_id, label, depth FROM scopes ORDER BY id")
            .map_err(StorageError::backend)?;
        let mut rows = stmt.query([]).map_err(StorageError::backend)?;
        while let Some(row) = rows.next().map_err(StorageError::backend)? {
            let scope = ScopeNode {
                id: row.get(0).map_err(StorageError::backend)?,
                parent_id: row.get(1).map_err(StorageError::backend)?,
                label: row.get(2).map_err(StorageError::backend)?,
                depth: row.get(3).map_err(StorageError::backend)?,
            };
            f(scope)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        crate::store::schema::migrate(&conn, None).unwrap();
        conn
    }

    #[test]
    fn root_scope_exists() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let root = store.get(1).unwrap();
        assert_eq!(root.label, "root");
        assert!(root.parent_id.is_none());
        assert_eq!(root.depth, 0);
    }

    #[test]
    fn insert_and_get_scope() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let node = store.insert(1, "user:michael", 1).unwrap();
        assert_eq!(node.parent_id, Some(1));
        assert_eq!(node.label, "user:michael");
        assert_eq!(node.depth, 1);

        let fetched = store.get(node.id).unwrap();
        assert_eq!(fetched, node);
    }

    #[test]
    fn find_by_label_found() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        store.insert(1, "project:demo", 1).unwrap();
        let found = store.find_by_label(1, "project:demo").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().label, "project:demo");
    }

    #[test]
    fn find_by_label_not_found() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let found = store.find_by_label(1, "nonexistent").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn ensure_path_creates_chain() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let leaf_id = store
            .ensure_path("user:michael/machine:desktop/project:memory-engine")
            .unwrap();

        let all = store.list_all().unwrap();
        // root + 3 new nodes
        assert_eq!(all.len(), 4);

        let leaf = store.get(leaf_id).unwrap();
        assert_eq!(leaf.label, "project:memory-engine");
        assert_eq!(leaf.depth, 3);

        // Verify parent chain
        let machine = store.get(leaf.parent_id.unwrap()).unwrap();
        assert_eq!(machine.label, "machine:desktop");
        assert_eq!(machine.depth, 2);

        let user = store.get(machine.parent_id.unwrap()).unwrap();
        assert_eq!(user.label, "user:michael");
        assert_eq!(user.depth, 1);
        assert_eq!(user.parent_id, Some(1)); // child of root
    }

    #[test]
    fn ensure_path_idempotent() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let id1 = store.ensure_path("user:michael/project:demo").unwrap();
        let id2 = store.ensure_path("user:michael/project:demo").unwrap();
        assert_eq!(id1, id2);

        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 3); // root + 2
    }

    #[test]
    fn ensure_path_rejects_empty_segment() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let err = store.ensure_path("user:michael//project:demo").unwrap_err();
        assert!(matches!(err, MemoryError::Conflict(_)));
    }

    #[test]
    fn ensure_path_rejects_slash_in_label() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        // A single segment containing a slash would be split by the '/' delimiter,
        // so we test a label with leading whitespace instead
        let err = store.ensure_path(" leading-space").unwrap_err();
        assert!(matches!(err, MemoryError::Conflict(_)));
    }

    #[test]
    fn ensure_path_rejects_long_label() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let long_label = "a".repeat(257);
        let err = store.ensure_path(&long_label).unwrap_err();
        assert!(matches!(err, MemoryError::Conflict(_)));
    }

    #[test]
    fn for_each_on_seeded_store_visits_only_root() {
        // `migrate` seeds the root scope, so a fresh store is never truly empty —
        // the smallest possible state is exactly the root (id=1).
        let conn = setup();
        let store = ScopeStore::new(&conn);
        let mut visited = Vec::new();
        store
            .for_each(|s| {
                visited.push(s);
                Ok(())
            })
            .unwrap();
        assert_eq!(visited.len(), 1);
        assert_eq!(visited[0].id, 1);
        assert_eq!(visited[0].label, "root");
        assert!(visited[0].parent_id.is_none());
        assert_eq!(visited[0].depth, 0);
    }

    #[test]
    fn for_each_visits_every_scope_exactly_once_in_id_order() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        // Distinct labels at distinct depths so a swap or dropped row is caught.
        let leaf = store
            .ensure_path("user:michael/machine:desktop/project:memory-engine")
            .unwrap();

        let mut visited = Vec::new();
        store
            .for_each(|s| {
                visited.push((s.id, s.label, s.depth, s.parent_id));
                Ok(())
            })
            .unwrap();

        // for_each must agree with list_all (the same SELECT ... ORDER BY id),
        // and cover root + the three created nodes.
        let expected: Vec<_> = store
            .list_all()
            .unwrap()
            .into_iter()
            .map(|s| (s.id, s.label, s.depth, s.parent_id))
            .collect();
        assert_eq!(visited, expected);

        // Pin the exact ordered shape so an out-of-order scan or missing node fails:
        // root, then the chain in insertion/id order with ascending depths.
        assert_eq!(
            visited,
            vec![
                (1, "root".to_string(), 0, None),
                (2, "user:michael".to_string(), 1, Some(1)),
                (3, "machine:desktop".to_string(), 2, Some(2)),
                (4, "project:memory-engine".to_string(), 3, Some(3)),
            ]
        );
        // The leaf id returned by ensure_path is the last visited node.
        assert_eq!(visited.last().unwrap().0, leaf);
    }

    #[test]
    fn for_each_propagates_callback_error_and_stops_iteration() {
        let conn = setup();
        let store = ScopeStore::new(&conn);
        store.ensure_path("a/b/c").unwrap(); // root + 3 => 4 scopes total

        // Fail on the second visited scope: proves the error surfaces AND that
        // the remaining scopes are not visited.
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
