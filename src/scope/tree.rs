use std::collections::{HashMap, VecDeque};

use rusqlite::Connection;

use crate::error::Result;
use crate::store::ScopeStore;
use crate::types::{ScopeNode, ScopeQuery};

/// In-memory cache of the scope hierarchy.
///
/// Built from the database via [`ScopeTree::load`]. Supports path resolution,
/// ancestor/descendant traversal, and query resolution — all without DB access.
pub struct ScopeTree {
    nodes: HashMap<i64, ScopeNode>,
    children: HashMap<i64, Vec<i64>>,
}

impl ScopeTree {
    /// Build tree from all scopes in the database.
    pub fn load(conn: &Connection) -> Result<Self> {
        let store = ScopeStore::new(conn);
        let all = store.list_all()?;

        let mut nodes = HashMap::with_capacity(all.len());
        let mut children: HashMap<i64, Vec<i64>> = HashMap::new();

        for node in all {
            if let Some(pid) = node.parent_id {
                children.entry(pid).or_default().push(node.id);
            }
            nodes.insert(node.id, node);
        }

        Ok(Self { nodes, children })
    }

    /// Root scope id (always 1).
    pub fn root_id(&self) -> i64 {
        1
    }

    /// Resolve a path string to a scope_id (read-only, no creation).
    /// Returns `None` if any segment is missing.
    pub fn resolve_path(&self, path: &str) -> Option<i64> {
        let mut current = 1i64; // start at root
        for segment in path.split('/') {
            let segment = segment.trim();
            if segment.is_empty() {
                return None;
            }
            let child_ids = self.children.get(&current)?;
            let found = child_ids
                .iter()
                .find(|&&id| self.nodes.get(&id).map_or(false, |n| n.label == segment));
            current = *found?;
        }
        Some(current)
    }

    /// Get ancestor scope_ids from leaf to root (inclusive).
    pub fn ancestors(&self, scope_id: i64) -> Vec<i64> {
        let mut result = Vec::new();
        let mut current = Some(scope_id);
        while let Some(id) = current {
            result.push(id);
            current = self.nodes.get(&id).and_then(|n| n.parent_id);
        }
        result
    }

    /// Get all descendant scope_ids (BFS, inclusive of start node).
    pub fn subtree(&self, scope_id: i64) -> Vec<i64> {
        let mut result = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(scope_id);
        while let Some(id) = queue.pop_front() {
            result.push(id);
            if let Some(child_ids) = self.children.get(&id) {
                for &cid in child_ids {
                    queue.push_back(cid);
                }
            }
        }
        result
    }

    /// Get ancestors + subtree of leaf (inherited context).
    ///
    /// Returns ancestors of `scope_id` PLUS the subtree rooted at `scope_id`.
    /// Deduplicates `scope_id` itself (appears in both ancestors and subtree).
    pub fn inherited(&self, scope_id: i64) -> Vec<i64> {
        let mut result = self.ancestors(scope_id);
        // subtree includes scope_id itself, but ancestors already has it
        let sub = self.subtree(scope_id);
        for id in sub {
            if id != scope_id {
                result.push(id);
            }
        }
        result
    }

    /// Resolve a [`ScopeQuery`] to a set of scope_ids.
    pub fn resolve_query(&self, query: &ScopeQuery) -> Option<Vec<i64>> {
        match query {
            ScopeQuery::Exact(path) => self.resolve_path(path).map(|id| vec![id]),
            ScopeQuery::Subtree(path) => self.resolve_path(path).map(|id| self.subtree(id)),
            ScopeQuery::Ancestors(path) => self.resolve_path(path).map(|id| self.ancestors(id)),
            ScopeQuery::Inherited(path) => self.resolve_path(path).map(|id| self.inherited(id)),
        }
    }

    /// Add a node to the in-memory tree (after DB insert).
    ///
    /// **Idempotent by id:** If a node with the same `id` already exists in the
    /// cache, this is a no-op.
    pub fn insert(&mut self, node: ScopeNode) {
        if self.nodes.contains_key(&node.id) {
            return; // idempotent
        }
        if let Some(pid) = node.parent_id {
            self.children.entry(pid).or_default().push(node.id);
        }
        self.nodes.insert(node.id, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, migrate, open_memory};

    fn setup_tree() -> ScopeTree {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        // Create: root -> user:michael -> project:demo
        //                              -> project:other
        let store = ScopeStore::new(&conn);
        store.ensure_path("user:michael/project:demo").unwrap();
        store.ensure_path("user:michael/project:other").unwrap();

        ScopeTree::load(&conn).unwrap()
    }

    #[test]
    fn load_has_root() {
        let tree = setup_tree();
        let root = tree.nodes.get(&1).unwrap();
        assert_eq!(root.label, "root");
    }

    #[test]
    fn resolve_path_found() {
        let tree = setup_tree();
        let id = tree.resolve_path("user:michael/project:demo");
        assert!(id.is_some());
    }

    #[test]
    fn resolve_path_not_found() {
        let tree = setup_tree();
        assert!(tree.resolve_path("nonexistent").is_none());
        assert!(tree.resolve_path("user:michael/nonexistent").is_none());
    }

    #[test]
    fn ancestors_returns_leaf_to_root() {
        let tree = setup_tree();
        let demo_id = tree.resolve_path("user:michael/project:demo").unwrap();
        let anc = tree.ancestors(demo_id);
        // demo -> user:michael -> root
        assert_eq!(anc.len(), 3);
        assert_eq!(anc[0], demo_id);
        assert_eq!(*anc.last().unwrap(), 1); // root
    }

    #[test]
    fn subtree_returns_all_descendants() {
        let tree = setup_tree();
        let user_id = tree.resolve_path("user:michael").unwrap();
        let sub = tree.subtree(user_id);
        // user:michael, project:demo, project:other
        assert_eq!(sub.len(), 3);
        assert_eq!(sub[0], user_id);
    }

    #[test]
    fn inherited_combines_ancestors_and_subtree() {
        let tree = setup_tree();
        let demo_id = tree.resolve_path("user:michael/project:demo").unwrap();
        let inh = tree.inherited(demo_id);
        // ancestors: demo, user:michael, root (3)
        // subtree of demo: just demo (already counted)
        // total: 3 unique
        assert_eq!(inh.len(), 3);
    }

    #[test]
    fn resolve_query_exact() {
        let tree = setup_tree();
        let ids = tree
            .resolve_query(&ScopeQuery::Exact("user:michael/project:demo".into()))
            .unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn resolve_query_subtree() {
        let tree = setup_tree();
        let ids = tree
            .resolve_query(&ScopeQuery::Subtree("user:michael".into()))
            .unwrap();
        assert_eq!(ids.len(), 3); // user:michael + 2 projects
    }

    #[test]
    fn insert_idempotent() {
        let mut tree = setup_tree();
        let node_count_before = tree.nodes.len();
        let demo_id = tree.resolve_path("user:michael/project:demo").unwrap();
        let demo_node = tree.nodes.get(&demo_id).unwrap().clone();

        tree.insert(demo_node);
        assert_eq!(tree.nodes.len(), node_count_before); // no growth
    }

    #[test]
    fn insert_new_node() {
        let mut tree = setup_tree();
        let node_count_before = tree.nodes.len();
        let new_node = ScopeNode {
            id: 999,
            parent_id: Some(1),
            label: "new:scope".into(),
            depth: 1,
        };
        tree.insert(new_node);
        assert_eq!(tree.nodes.len(), node_count_before + 1);
        assert!(tree.children.get(&1).unwrap().contains(&999));
    }
}
