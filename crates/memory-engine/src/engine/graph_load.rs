//! Backend→projection loaders (the engine's graph/scope-sync glue).
//!
//! The conn-based DB loaders for the in-memory `MemoryGraph` / `ScopeTree`
//! projections. They live in the facade (not `me-index`) because they name the
//! concrete `SQLite` stores; the projections themselves are backend-free and expose
//! only the pure cores (`MemoryGraph::from_active_edges`, `ScopeTree::from_nodes`).
//! Relocated here from inherent methods in Wave 2 #816 / S2 to keep the
//! graph/scope ↔ store carve acyclic.
use rusqlite::Connection;

use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::scope::ScopeTree;
use crate::store::ScopeStore;
use crate::store::edges::EdgeStore;

/// Rebuild the in-memory graph from all active edges in `SQLite` (recovery path on
/// `MemoryEngine::open` when there is no snapshot). Was `MemoryGraph::load_from_db`.
///
/// `pub` (not `pub(crate)`): the enclosing `mod graph_load;` is already private to
/// `crate::engine`, so `pub(crate)` here would be a no-op stricter than the module
/// already enforces (`clippy::redundant_pub_crate`).
pub fn load_graph_from_db(conn: &Connection) -> Result<MemoryGraph> {
    let store = EdgeStore::new(conn);
    let active_edges = store.list_active()?;
    Ok(MemoryGraph::from_active_edges(&active_edges))
}

/// Build the scope tree from all scopes in `SQLite`. Was `ScopeTree::load`.
pub fn load_scope_tree(conn: &Connection) -> Result<ScopeTree> {
    let store = ScopeStore::new(conn);
    Ok(ScopeTree::from_nodes(store.list_all()?))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::store::schema::{init_schema, migrate, open_memory};
    use crate::types::NewEdge;

    fn setup_db() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        // Insert dummy facts for FK constraints
        for (content, hash) in &[("f1", "h1"), ("f2", "h2"), ("f3", "h3")] {
            conn.execute(
                "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata)
                 VALUES (?1, ?2, X'00000000', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}')",
                rusqlite::params![content, hash],
            ).unwrap();
        }
        conn
    }

    #[test]
    fn load_from_db_skips_expired() {
        let conn = setup_db();
        let store = EdgeStore::new(&conn);

        let now = Utc::now();
        let e1 = NewEdge {
            source_fact_id: 1,
            target_fact_id: 2,
            relation_type: "active".into(),
            weight: 1.0,
            scope_id: 1,
            t_created: now,
            t_expired: None,
        };
        let e2 = NewEdge {
            source_fact_id: 2,
            target_fact_id: 3,
            relation_type: "expired".into(),
            weight: 1.0,
            scope_id: 1,
            t_created: now,
            t_expired: None,
        };

        let id1 = store.insert(&e1).unwrap();
        let id2 = store.insert(&e2).unwrap();
        store.expire(id2, now).unwrap();

        let graph = super::load_graph_from_db(&conn).unwrap();
        assert_eq!(graph.edge_count(), 1);
        assert!(graph.has_node(1));
        assert!(graph.has_node(2));
        assert!(!graph.has_node(3)); // only appeared in the expired edge

        // The active edge should be loadable
        let neighbors = graph.neighbors(1);
        assert_eq!(neighbors, vec![2]);

        // Verify we loaded the right edge_id
        let _ = id1; // used for insert, graph has it as EdgeData.edge_id
    }

    #[test]
    fn load_from_db_empty_edges_table() {
        // #499 (graph/testing-load-from-db-edge-cases): an edges table with no
        // active rows yields an empty graph — no nodes, no edges, no panic.
        let conn = setup_db();
        let graph = super::load_graph_from_db(&conn).unwrap();
        assert_eq!(graph.edge_count(), 0);
        assert_eq!(graph.node_count(), 0);
        assert!(!graph.has_node(1));
    }

    #[test]
    fn load_from_db_preserves_edge_weight() {
        // #499: weight preservation through the DB load path. Insert an edge with a
        // distinctive non-unit weight and assert it round-trips into the loaded
        // graph's `EdgeData.weight` (observed via the snapshot projection).
        let conn = setup_db();
        let store = EdgeStore::new(&conn);
        let now = Utc::now();
        store
            .insert(&NewEdge {
                source_fact_id: 1,
                target_fact_id: 2,
                relation_type: "weighted".into(),
                weight: 0.625,
                scope_id: 1,
                t_created: now,
                t_expired: None,
            })
            .unwrap();

        let graph = super::load_graph_from_db(&conn).unwrap();
        assert_eq!(graph.edge_count(), 1);

        let snap = graph.to_snapshot();
        assert_eq!(snap.edges.len(), 1);
        let e = &snap.edges[0];
        assert_eq!(e.source, 1);
        assert_eq!(e.target, 2);
        assert_eq!(e.relation_type, "weighted");
        assert!((e.weight - 0.625).abs() < f64::EPSILON);
    }

    #[test]
    fn load_from_db_parallel_edges() {
        // #499: two active edges with the same (source, target) are both loaded —
        // petgraph is a multigraph, so a parallel edge must not collapse. Each
        // edge keeps its own edge_id/relation_type.
        let conn = setup_db();
        let store = EdgeStore::new(&conn);
        let now = Utc::now();
        store
            .insert(&NewEdge {
                source_fact_id: 1,
                target_fact_id: 2,
                relation_type: "first".into(),
                weight: 1.0,
                scope_id: 1,
                t_created: now,
                t_expired: None,
            })
            .unwrap();
        store
            .insert(&NewEdge {
                source_fact_id: 1,
                target_fact_id: 2,
                relation_type: "second".into(),
                weight: 1.0,
                scope_id: 1,
                t_created: now,
                t_expired: None,
            })
            .unwrap();

        let graph = super::load_graph_from_db(&conn).unwrap();
        // Both parallel edges survive (multigraph), but only one node pair.
        assert_eq!(graph.edge_count(), 2);
        assert_eq!(graph.node_count(), 2);
        // 1 → 2 appears twice in the outgoing neighbor list.
        assert_eq!(graph.neighbors(1), vec![2, 2]);
        assert_eq!(graph.degree(1), 2);
        assert_eq!(graph.degree(2), 2);

        // Both relation types are present, edge_ids distinct.
        let snap = graph.to_snapshot();
        let mut rels: Vec<_> = snap.edges.iter().map(|e| e.relation_type.clone()).collect();
        rels.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(rels[0], "first");
        assert_eq!(rels[1], "second");
        let ids: std::collections::HashSet<i64> = snap.edges.iter().map(|e| e.edge_id).collect();
        assert_eq!(ids.len(), 2);
    }

    mod proptest_scope {
        use proptest::prelude::*;

        use super::*;
        use crate::scope::ScopeTree;

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
                let tree = super::super::load_scope_tree(&conn).unwrap();

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
                let tree = super::super::load_scope_tree(&conn).unwrap();

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
