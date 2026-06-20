use std::collections::{HashMap, HashSet, VecDeque};

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
    /// Build the `(nodes, children)` index maps from a node iterator.
    ///
    /// Shared by [`ScopeTree::load`] (from the DB) and
    /// [`ScopeTree::from_snapshot`] (from a serialized snapshot): both insert
    /// each node into `nodes` by id and register it under its parent's
    /// `children` list.
    fn build_index<I>(node_iter: I) -> (HashMap<i64, ScopeNode>, HashMap<i64, Vec<i64>>)
    where
        I: IntoIterator<Item = ScopeNode>,
        I::IntoIter: ExactSizeIterator,
    {
        let iter = node_iter.into_iter();
        let mut nodes = HashMap::with_capacity(iter.len());
        let mut children: HashMap<i64, Vec<i64>> = HashMap::new();

        for node in iter {
            if let Some(pid) = node.parent_id {
                children.entry(pid).or_default().push(node.id);
            }
            nodes.insert(node.id, node);
        }

        (nodes, children)
    }

    /// Build tree from all scopes in the database.
    pub fn load(conn: &Connection) -> Result<Self> {
        let store = ScopeStore::new(conn);
        let all = store.list_all()?;
        let (nodes, children) = Self::build_index(all);
        Ok(Self { nodes, children })
    }

    /// Root scope id (database-assigned, always 1).
    pub(crate) const ROOT_ID: i64 = 1;

    /// Root scope id (always 1).
    #[must_use]
    pub const fn root_id() -> i64 {
        Self::ROOT_ID
    }

    /// Resolve a path string to a `scope_id` (read-only, no creation).
    ///
    /// The canonical root string `"/"` (the one [`ScopeTree::path_for_id`]
    /// renders for the root) resolves to the root scope, so
    /// `resolve_path(path_for_id(root))` round-trips rather than returning
    /// `None` (the representation is invertible). The empty string `""` is
    /// **not** a root synonym — it resolves to `None`, agreeing with the write
    /// path ([`crate::store::ScopeStore::ensure_path`] rejects `""`) and keeping
    /// an empty/defaulted scope query from silently meaning "the entire store".
    ///
    /// For any other input, segments are separated by `/`. Returns `None` if any
    /// segment does not exist in the cached tree, or if the path contains an
    /// empty *inner* segment (e.g. a leading/trailing `/` or `//`).
    ///
    /// **Whitespace:** each segment is trimmed of ASCII whitespace before lookup,
    /// so `resolve_path(" user:michael")` matches the stored label `user:michael`.
    /// This diverges from [`crate::store::ScopeStore::ensure_path`], which
    /// *rejects* a label with surrounding whitespace (`MemoryError::Conflict`).
    /// The asymmetry is intentional and safe: because `ensure_path` guarantees no
    /// stored label ever carries surrounding whitespace, the trim here can only
    /// recover the un-padded label — it is defensive, not permissive. Use
    /// `ensure_path` when you need the input itself validated rather than coerced.
    pub fn resolve_path(&self, path: &str) -> Option<i64> {
        // Root synonym: the canonical "/" rendered by `path_for_id`. Handled up
        // front so the per-segment loop never sees the two empty segments "/"
        // would otherwise split into. The empty string "" is deliberately *not*
        // a root synonym: it stays unresolvable (`None`), matching the write
        // path where `ScopeStore::ensure_path("")` errors. Resolving "" to root
        // would make an empty/defaulted scope query mean "the entire store"
        // (subtree(root) = all facts) — a fail-open hole for a scope-isolation
        // primitive. `path_for_id` never emits "", so only "/" is needed for the
        // representation to round-trip.
        if path == "/" {
            return Some(Self::ROOT_ID);
        }
        let mut current = Self::ROOT_ID; // start at root
        for segment in path.split('/') {
            let segment = segment.trim();
            // Shared structural validation (non-empty, no '/', <= 256 bytes) —
            // the single source of truth in `crate::scope::validate_segment`,
            // also used by `ScopeStore::ensure_path` on the write path. Any
            // failure is indistinguishable from "not found" here, so map to None.
            if super::validate_segment(segment).is_err() {
                return None;
            }
            let child_ids = self.children.get(&current)?;
            let found = child_ids
                .iter()
                .find(|&&id| self.nodes.get(&id).is_some_and(|n| n.label == segment));
            current = *found?;
        }
        Some(current)
    }

    /// Get ancestor `scope_ids` from leaf to root (inclusive).
    ///
    /// Cycle-safe: a malformed `parent_id` graph (e.g. a snapshot with a parent
    /// cycle) would otherwise loop forever. We stop the walk the first time an
    /// id repeats. For a valid (acyclic) tree this is byte-identical to a plain
    /// parent-walk — no id ever repeats, so the guard never fires.
    pub fn ancestors(&self, scope_id: i64) -> Vec<i64> {
        let mut result = Vec::new();
        let mut current = Some(scope_id);
        while let Some(id) = current {
            // Cycle guard: the ancestor chain is shallow (bounded by tree depth),
            // so a linear scan of `result` is cheaper than a `HashSet` and needs
            // no allocation.
            if result.contains(&id) {
                break; // cycle detected — id already visited
            }
            result.push(id);
            current = self.nodes.get(&id).and_then(|n| n.parent_id);
        }
        result
    }

    /// Get all descendant `scope_ids` (BFS, inclusive of start node).
    ///
    /// Cycle-safe: a malformed `children` graph with a cycle would otherwise
    /// enqueue ids forever. We skip any id already visited. For a valid
    /// (acyclic) tree this is byte-identical to a plain BFS — each node is
    /// reachable on exactly one path, so the guard never skips a real node.
    pub fn subtree(&self, scope_id: i64) -> Vec<i64> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(scope_id);
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id) {
                continue; // already visited — cycle protection
            }
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

    /// Resolve a [`ScopeQuery`] to a set of `scope_ids`.
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

    /// Number of nodes in the scope tree.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Maximum depth in the scope tree. Returns 0 for an empty tree.
    #[must_use]
    pub fn max_depth(&self) -> i64 {
        self.nodes.values().map(|n| n.depth).max().unwrap_or(0)
    }

    /// Reconstruct the scope path string for a given scope ID.
    ///
    /// Returns `"/"` for the root scope. Non-root example: `"user:michael/project:demo"`.
    /// Returns `None` if the ID is not in the tree.
    ///
    /// **Note:** The root path `"/"` is accepted by
    /// [`ScopeTree::resolve_path`] as a root synonym, so the representation
    /// round-trips: `resolve_path(path_for_id(root)) == Some(root_id())`.
    #[must_use]
    pub fn path_for_id(&self, scope_id: i64) -> Option<String> {
        if !self.nodes.contains_key(&scope_id) {
            return None;
        }
        if scope_id == Self::root_id() {
            return Some("/".to_string());
        }

        // Walk ancestors (excluding root) and collect labels in reverse.
        // Cycle-safe: stop if an id repeats so a malformed parent cycle cannot
        // loop forever. For a valid tree no id repeats, so behavior is identical.
        let mut segments = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(scope_id);
        while let Some(id) = current {
            if id == Self::root_id() {
                break;
            }
            if !seen.insert(id) {
                break; // cycle detected — id already visited
            }
            let node = self.nodes.get(&id)?;
            segments.push(node.label.clone());
            current = node.parent_id;
        }
        segments.reverse();
        Some(segments.join("/"))
    }

    /// Snapshot all nodes for serialization.
    pub(crate) fn to_snapshot(&self) -> crate::engine::snapshot::ScopeTreeSnapshot {
        crate::engine::snapshot::ScopeTreeSnapshot {
            nodes: self.nodes.values().cloned().collect(),
        }
    }

    /// Rebuild tree from a snapshot (same logic as `load` but from snapshot data).
    pub(crate) fn from_snapshot(snap: &crate::engine::snapshot::ScopeTreeSnapshot) -> Self {
        let (nodes, children) = Self::build_index(snap.nodes.iter().cloned());
        Self { nodes, children }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, migrate, open_memory};

    fn setup_tree() -> ScopeTree {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

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
    fn resolve_query_ancestors() {
        // `resolve_query` must dispatch `Ancestors` to `ancestors()` (not e.g.
        // `subtree`). For demo the chain is demo -> user:michael -> root. (#322)
        let tree = setup_tree();
        let demo_id = tree.resolve_path("user:michael/project:demo").unwrap();
        let ids = tree
            .resolve_query(&ScopeQuery::Ancestors("user:michael/project:demo".into()))
            .unwrap();
        // Identical to the primitive — proves the arm routed to `ancestors`.
        assert_eq!(ids, tree.ancestors(demo_id));
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], demo_id);
        assert_eq!(*ids.last().unwrap(), ScopeTree::root_id());
    }

    #[test]
    fn resolve_query_inherited() {
        // `resolve_query` must dispatch `Inherited` to `inherited()`. For
        // user:michael that is ancestors [user, root] + descendants [demo,
        // other], deduped to 4 unique ids. (#322)
        let tree = setup_tree();
        let user_id = tree.resolve_path("user:michael").unwrap();
        let ids = tree
            .resolve_query(&ScopeQuery::Inherited("user:michael".into()))
            .unwrap();
        // Identical to the primitive — proves the arm routed to `inherited`.
        assert_eq!(ids, tree.inherited(user_id));
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 4, "expected user + root + demo + other");
        assert!(ids.contains(&ScopeTree::root_id()));
    }

    #[test]
    fn resolve_query_missing_path_returns_none() {
        // Every arm short-circuits to None when the path does not resolve, so a
        // nonexistent scope yields no ids regardless of query kind.
        let tree = setup_tree();
        for q in [
            ScopeQuery::Exact("user:ghost".into()),
            ScopeQuery::Subtree("user:ghost".into()),
            ScopeQuery::Ancestors("user:ghost".into()),
            ScopeQuery::Inherited("user:ghost".into()),
        ] {
            assert!(tree.resolve_query(&q).is_none(), "{q:?} should be None");
        }
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
    fn path_for_id_root_returns_slash() {
        let tree = setup_tree();
        // Root scope renders as the canonical "/" path.
        assert_eq!(
            tree.path_for_id(ScopeTree::root_id()),
            Some("/".to_string())
        );
    }

    #[test]
    fn resolve_path_root_synonyms() {
        // The root path produced by `path_for_id` ("/") must round-trip back
        // through `resolve_path` (#360 — eliminate the non-invertible
        // representation). The empty string is deliberately NOT a root synonym:
        // it stays unresolvable, agreeing with the write path
        // (`ScopeStore::ensure_path("")` errors) and avoiding the fail-open
        // "empty scope query == the entire store" footgun.
        let tree = setup_tree();
        let root = ScopeTree::root_id();
        assert_eq!(tree.resolve_path("/"), Some(root));
        assert_eq!(tree.resolve_path(""), None);
    }

    #[test]
    fn path_for_id_root_roundtrips_through_resolve_path() {
        // The whole point of #360: `resolve_path(path_for_id(root))` resolves
        // back to root rather than silently returning `None`.
        let tree = setup_tree();
        let root = ScopeTree::root_id();
        let rendered = tree.path_for_id(root).expect("root has a path");
        assert_eq!(tree.resolve_path(&rendered), Some(root));
    }

    #[test]
    fn resolve_path_still_rejects_empty_inner_segment() {
        // A leading/trailing "/" or "//" still yields an empty *inner* segment
        // and must remain unresolvable — accepting the root synonym must not
        // make malformed multi-segment paths resolve.
        let tree = setup_tree();
        assert!(tree.resolve_path("user:michael//project:demo").is_none());
        assert!(tree.resolve_path("/user:michael").is_none());
        assert!(tree.resolve_path("user:michael/").is_none());
    }

    #[test]
    fn resolve_query_empty_string_is_no_match_not_everything() {
        // Guards the dangerous direction at the query boundary: an empty scope
        // string must NOT resolve to root. If it did, `Subtree("")`/`Inherited("")`
        // would expand to `subtree(root)` = every scope_id in the store, turning
        // an empty/defaulted scope query into an unscoped scan over all facts —
        // a fail-open hole for a scope-isolation primitive. All four variants
        // must report `None` ("scope doesn't exist → no results"), the same as
        // any other non-existent path.
        let tree = setup_tree();
        assert_eq!(tree.resolve_query(&ScopeQuery::Exact(String::new())), None);
        assert_eq!(
            tree.resolve_query(&ScopeQuery::Subtree(String::new())),
            None
        );
        assert_eq!(
            tree.resolve_query(&ScopeQuery::Ancestors(String::new())),
            None
        );
        assert_eq!(
            tree.resolve_query(&ScopeQuery::Inherited(String::new())),
            None
        );
    }

    #[test]
    fn path_for_id_unknown_returns_none() {
        let tree = setup_tree();
        // ID absent from the tree short-circuits to None (tree.rs:148-150).
        assert_eq!(tree.path_for_id(424_242), None);
    }

    #[test]
    fn path_for_id_non_root_joins_labels() {
        let tree = setup_tree();
        let demo_id = tree.resolve_path("user:michael/project:demo").unwrap();
        // Root is excluded; remaining labels are joined leaf-last.
        assert_eq!(
            tree.path_for_id(demo_id),
            Some("user:michael/project:demo".to_string())
        );
    }

    #[test]
    fn snapshot_roundtrip_preserves_tree() {
        // `from_snapshot(to_snapshot())` is the crash-recovery path
        // (engine/mod.rs builds the in-memory tree from a serialized snapshot on
        // resume). Assert it reconstructs the tree faithfully: a future
        // `ScopeNode` field left unwired in `from_snapshot` (or a dropped node)
        // would silently lose scope hierarchy on restore, and this test catches
        // it. (#323)
        let tree = setup_tree(); // root -> user:michael -> {project:demo, project:other}
        let rebuilt = ScopeTree::from_snapshot(&tree.to_snapshot());

        // Same node set.
        assert_eq!(rebuilt.node_count(), tree.node_count());

        // Path resolution still works on the rebuilt tree.
        let id = rebuilt
            .resolve_path("user:michael/project:demo")
            .expect("path must resolve in the rebuilt tree");
        assert_eq!(id, tree.resolve_path("user:michael/project:demo").unwrap());

        // Ancestor chain preserved: demo -> user:michael -> root.
        let ancestors = rebuilt.ancestors(id);
        assert_eq!(ancestors.len(), 3);
        assert_eq!(*ancestors.last().unwrap(), ScopeTree::root_id());
        assert_eq!(ancestors[0], id);

        // path_for_id roundtrip preserved for every node, including siblings.
        assert_eq!(
            rebuilt.path_for_id(id).as_deref(),
            Some("user:michael/project:demo")
        );
        let other_id = rebuilt.resolve_path("user:michael/project:other").unwrap();
        assert_eq!(
            rebuilt.path_for_id(other_id).as_deref(),
            Some("user:michael/project:other")
        );

        // children maps agree node-for-node (the structural index, not just the
        // node set): every parent resolves to the same descendant set.
        for &node_id in &[
            ScopeTree::root_id(),
            tree.resolve_path("user:michael").unwrap(),
        ] {
            let mut a = rebuilt.subtree(node_id);
            let mut b = tree.subtree(node_id);
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "subtree of {node_id} must survive the roundtrip");
        }
    }

    #[test]
    fn inherited_non_leaf_dedups_self_once() {
        let tree = setup_tree();
        // user:michael is a NON-leaf: its subtree has 3 nodes (itself + 2
        // projects). inherited() must add user:michael exactly once (via
        // ancestors) and append only the descendants, exercising the
        // `if id != scope_id` dedup branch (tree.rs:95-99).
        let user_id = tree.resolve_path("user:michael").unwrap();
        let inh = tree.inherited(user_id);
        // ancestors(user) = [user, root] (2); subtree(user) = [user, demo, other]
        // dedup user -> append demo, other => [user, root, demo, other] = 4.
        assert_eq!(inh.len(), 4);
        // user:michael appears exactly once despite being in both sets.
        assert_eq!(inh.iter().filter(|&&id| id == user_id).count(), 1);
        // All four ids are unique (no duplicate from the overlap).
        let mut sorted = inh;
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "inherited() must not emit duplicates");
    }

    #[test]
    fn ancestors_unknown_id_returns_singleton() {
        let tree = setup_tree();
        // Unknown id: the while-loop pushes the id, then nodes.get fails so
        // parent_id resolves to None and the loop ends (tree.rs:61-68).
        let anc = tree.ancestors(999_999);
        assert_eq!(anc, vec![999_999]);
    }

    #[test]
    fn subtree_unknown_id_returns_singleton() {
        let tree = setup_tree();
        // Unknown id has no children entry, so the BFS yields just the seed
        // (tree.rs:72-84).
        let sub = tree.subtree(999_999);
        assert_eq!(sub, vec![999_999]);
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

    #[test]
    fn node_count_matches_inserted_nodes() {
        let tree = setup_tree();
        // root + user:michael + project:demo + project:other = 4
        assert_eq!(tree.node_count(), 4);
    }

    #[test]
    fn node_count_empty_tree() {
        let tree = ScopeTree {
            nodes: HashMap::new(),
            children: HashMap::new(),
        };
        assert_eq!(tree.node_count(), 0);
    }

    #[test]
    fn max_depth_returns_deepest_node() {
        let tree = setup_tree();
        // root=depth 0, user:michael=depth 1, project:demo/other=depth 2
        assert_eq!(tree.max_depth(), 2);
    }

    #[test]
    fn max_depth_empty_tree() {
        let tree = ScopeTree {
            nodes: HashMap::new(),
            children: HashMap::new(),
        };
        assert_eq!(tree.max_depth(), 0);
    }

    /// Hand-build a tiny tree with a `parent_id` cycle: 10 -> 11 -> 10.
    /// A malformed/hostile snapshot or DB could yield such a graph; the
    /// traversals must terminate rather than spin forever.
    fn cyclic_tree() -> ScopeTree {
        let mut nodes = HashMap::new();
        let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
        // 10's parent is 11, 11's parent is 10 — a 2-cycle.
        nodes.insert(
            10,
            ScopeNode {
                id: 10,
                parent_id: Some(11),
                label: "a:a".into(),
                depth: 0,
            },
        );
        nodes.insert(
            11,
            ScopeNode {
                id: 11,
                parent_id: Some(10),
                label: "b:b".into(),
                depth: 1,
            },
        );
        children.entry(11).or_default().push(10);
        children.entry(10).or_default().push(11);
        ScopeTree { nodes, children }
    }

    #[test]
    fn ancestors_terminates_on_cycle() {
        let tree = cyclic_tree();
        let anc = tree.ancestors(10);
        // Must terminate and be bounded by the number of distinct nodes.
        assert!(anc.len() <= tree.nodes.len(), "ancestors must be bounded");
        // No id repeats — the cycle is broken on revisit.
        let mut sorted = anc.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), anc.len(), "ancestors must not repeat ids");
        assert!(anc.contains(&10));
    }

    #[test]
    fn subtree_terminates_on_cycle() {
        let tree = cyclic_tree();
        let sub = tree.subtree(10);
        assert!(sub.len() <= tree.nodes.len(), "subtree must be bounded");
        let mut sorted = sub.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), sub.len(), "subtree must not repeat ids");
        assert!(sub.contains(&10) && sub.contains(&11));
    }

    #[test]
    fn path_for_id_terminates_on_cycle() {
        let tree = cyclic_tree();
        // The id is present, none of the cycle nodes is root, so the label
        // walk would loop forever without cycle protection. It must return
        // *something* finite (Some or None) and not hang.
        let _ = tree.path_for_id(10);
    }

    mod proptest_scope {
        use super::*;
        use proptest::prelude::*;

        fn scope_segment() -> impl Strategy<Value = String> {
            "[a-z]{1,8}:[a-z]{1,8}"
        }

        fn scope_path(max_depth: usize) -> impl Strategy<Value = String> {
            proptest::collection::vec(scope_segment(), 1..max_depth).prop_map(|segs| segs.join("/"))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(32))]

            #[test]
            fn resolve_roundtrip(path in scope_path(4)) {
                let conn = open_memory().unwrap();
                init_schema(&conn).unwrap();
                migrate(&conn, None).unwrap();

                let store = ScopeStore::new(&conn);
                store.ensure_path(&path).unwrap();
                let tree = ScopeTree::load(&conn).unwrap();

                let resolved = tree.resolve_path(&path);
                prop_assert!(resolved.is_some(),
                    "path '{path}' was created but not resolvable");

                let id = resolved.unwrap();
                let reconstructed = tree.path_for_id(id);
                prop_assert_eq!(reconstructed.as_deref(), Some(path.as_str()),
                    "path_for_id roundtrip failed");
            }

            #[test]
            fn ancestors_always_end_at_root(path in scope_path(4)) {
                let conn = open_memory().unwrap();
                init_schema(&conn).unwrap();
                migrate(&conn, None).unwrap();

                let store = ScopeStore::new(&conn);
                store.ensure_path(&path).unwrap();
                let tree = ScopeTree::load(&conn).unwrap();

                let id = tree.resolve_path(&path).unwrap();
                let ancestors = tree.ancestors(id);

                prop_assert!(!ancestors.is_empty());
                prop_assert_eq!(*ancestors.last().unwrap(), ScopeTree::root_id(),
                    "ancestor chain should end at root");
                prop_assert_eq!(ancestors[0], id,
                    "ancestor chain should start at the node itself");
            }
        }
    }
}
