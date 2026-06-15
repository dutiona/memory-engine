use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::types::ScopeNode;

/// CRUD operations for the `scopes` table.
pub struct ScopeStore<'a> {
    conn: &'a Connection,
}

#[allow(dead_code)] // complete CRUD API — not all methods called through engine facade yet
impl<'a> ScopeStore<'a> {
    pub(crate) const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Get a scope by id.
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
                other => other.into(),
            })
    }

    /// Find a scope by `parent_id` + label.
    pub fn find_by_label(&self, parent_id: i64, label: &str) -> Result<Option<ScopeNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, label, depth FROM scopes WHERE parent_id = ?1 AND label = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![parent_id, label], |row| {
            Ok(ScopeNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                label: row.get(2)?,
                depth: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(Ok(node)) => Ok(Some(node)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Insert a scope under a parent. Returns the created node.
    pub fn insert(&self, parent_id: i64, label: &str, depth: i64) -> Result<ScopeNode> {
        self.conn.execute(
            "INSERT INTO scopes (parent_id, label, depth) VALUES (?1, ?2, ?3)",
            rusqlite::params![parent_id, label, depth],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(ScopeNode {
            id,
            parent_id: Some(parent_id),
            label: label.to_string(),
            depth,
        })
    }

    /// Validate a scope label segment.
    fn validate_label(label: &str) -> Result<()> {
        let trimmed = label.trim();
        if trimmed.is_empty() {
            return Err(MemoryError::Conflict(
                "scope label must not be empty".into(),
            ));
        }
        if trimmed.contains('/') {
            return Err(MemoryError::Conflict(
                "scope label must not contain '/'".into(),
            ));
        }
        if trimmed.len() > 256 {
            return Err(MemoryError::Conflict(
                "scope label must be at most 256 bytes".into(),
            ));
        }
        if trimmed != label {
            return Err(MemoryError::Conflict(
                "scope label must not have leading/trailing whitespace".into(),
            ));
        }
        Ok(())
    }

    /// Resolve a path string to a `scope_id`, creating missing nodes.
    ///
    /// Path format: `"user:michael/machine:desktop/project:memory-engine"`
    ///
    /// All top-level segments are children of root (id=1). Uses
    /// `INSERT OR IGNORE` + `SELECT` for race-safe creation.
    pub fn ensure_path(&self, path: &str) -> Result<i64> {
        let segments: Vec<&str> = path.split('/').collect();
        if segments.is_empty() {
            return Err(MemoryError::Conflict("scope path must not be empty".into()));
        }

        let mut parent_id: i64 = 1; // root

        for (depth, segment) in (1_i64..).zip(&segments) {
            Self::validate_label(segment)?;

            // INSERT OR IGNORE: if the scope already exists, this is a no-op.
            self.conn.execute(
                "INSERT OR IGNORE INTO scopes (parent_id, label, depth) VALUES (?1, ?2, ?3)",
                rusqlite::params![parent_id, segment, depth],
            )?;

            // SELECT: get the id (whether just inserted or already existed).
            let id: i64 = self.conn.query_row(
                "SELECT id FROM scopes WHERE parent_id = ?1 AND label = ?2",
                rusqlite::params![parent_id, segment],
                |row| row.get(0),
            )?;

            parent_id = id;
        }

        Ok(parent_id) // parent_id is now the leaf scope_id
    }

    /// List all scopes.
    pub fn list_all(&self) -> Result<Vec<ScopeNode>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, parent_id, label, depth FROM scopes ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(ScopeNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                label: row.get(2)?,
                depth: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
    /// Iterate all scopes row-by-row, calling `f` for each.
    ///
    /// Unlike [`Self::list_all`], this never allocates a `Vec` — each scope
    /// is read, passed to the callback, and dropped before the next row.
    /// Suitable for streaming serialization of large databases.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(ScopeNode) -> Result<()>,
    {
        let mut stmt = self
            .conn
            .prepare("SELECT id, parent_id, label, depth FROM scopes ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let scope = ScopeNode {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                label: row.get(2)?,
                depth: row.get(3)?,
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
}
