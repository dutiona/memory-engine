use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use rusqlite::Connection;

use crate::error::Result;
use crate::store::edges::EdgeStore;

/// Edge weight stored in the petgraph `DiGraph`.
#[derive(Debug, Clone)]
pub struct EdgeData {
    /// `SQLite` row id of the edge.
    pub edge_id: i64,
    /// Relationship label (e.g. "contradicts", "supplements").
    pub relation_type: String,
    /// Numeric weight for the edge.
    pub weight: f64,
}

/// In-memory graph backed by `petgraph`, mirroring the active edges in `SQLite`.
///
/// Node weights are fact ids (`i64`). Edge weights are [`EdgeData`].
/// The graph only contains active (non-expired) edges.
pub struct MemoryGraph {
    graph: DiGraph<i64, EdgeData>,
    node_map: HashMap<i64, NodeIndex>,
}

impl MemoryGraph {
    /// Create an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_map: HashMap::new(),
        }
    }

    /// Ensure a node exists for the given fact id, returning its index.
    pub(crate) fn ensure_node(&mut self, fact_id: i64) -> NodeIndex {
        *self
            .node_map
            .entry(fact_id)
            .or_insert_with(|| self.graph.add_node(fact_id))
    }

    /// Check whether a node exists for the given fact id.
    #[must_use]
    pub fn has_node(&self, fact_id: i64) -> bool {
        self.node_map.contains_key(&fact_id)
    }

    /// Add a directed edge between two fact ids.
    ///
    /// Ensures both nodes exist before adding the edge.
    pub fn add_edge(&mut self, source: i64, target: i64, data: EdgeData) {
        let s = self.ensure_node(source);
        let t = self.ensure_node(target);
        self.graph.add_edge(s, t, data);
    }

    /// Remove the edge with the given `SQLite` edge id.
    ///
    /// Edge ids are unique (one `SQLite` row → one petgraph edge), so this
    /// scans O(E) and removes at most one edge. Short-circuits after the
    /// first match.
    ///
    /// Used after expiring an edge in `SQLite` to keep the graph in sync.
    pub fn remove_edge_by_id(&mut self, edge_id: i64) {
        if let Some(ei) = self
            .graph
            .edge_indices()
            .find(|&ei| self.graph[ei].edge_id == edge_id)
        {
            self.graph.remove_edge(ei);
        }
    }

    /// Remove a node and all its edges from the graph.
    ///
    /// No-op if the fact id is not in the graph.
    /// Used by archival after hard-deleting facts from `SQLite`.
    ///
    /// petgraph's [`DiGraph::remove_node`] uses swap-remove: the former last
    /// node is relocated into the freed slot, which invalidates that node's
    /// cached [`NodeIndex`]. This method handles that re-indexing — after the
    /// removal it rewrites `node_map` for the displaced node so every surviving
    /// node still resolves to its correct index. Callers may therefore remove
    /// nodes in a loop (e.g. archival) without corrupting the map.
    pub fn remove_node(&mut self, fact_id: i64) {
        let Some(idx) = self.node_map.remove(&fact_id) else {
            return;
        };
        self.graph.remove_node(idx);
        // Swap-remove relocated the former last node into `idx`. Its weight is
        // the displaced fact id; rewrite its `node_map` entry to point at `idx`.
        // `node_weight` returns `None` when the removed node *was* the last slot
        // (no displacement), making the guard a no-op in that case.
        if let Some(&displaced_fact_id) = self.graph.node_weight(idx) {
            self.node_map.insert(displaced_fact_id, idx);
        }
    }

    /// Remove all edges involving a given fact id (as source or target).
    ///
    /// Uses directed edge iterators for O(degree) instead of O(E).
    /// Used by conflict resolution after expiring edges in `SQLite`.
    pub fn remove_edges_by_fact(&mut self, fact_id: i64) {
        let Some(&idx) = self.node_map.get(&fact_id) else {
            return;
        };
        // Collect outgoing and incoming edge indices for this node — O(degree)
        let mut to_remove: Vec<_> = self
            .graph
            .edges_directed(idx, Direction::Outgoing)
            .map(|e| e.id())
            .collect();
        to_remove.extend(
            self.graph
                .edges_directed(idx, Direction::Incoming)
                .map(|e| e.id()),
        );
        // petgraph's `remove_edge` swap-removes: it relocates the current last
        // edge into the freed slot, invalidating any still-pending cached
        // `EdgeIndex` that pointed at that last slot. Removing highest-index
        // first guarantees each edge we remove is always the current last edge,
        // so swap-remove never relocates anything still in `to_remove`.
        //
        // The descending sort also groups duplicate indices adjacently so
        // `dedup` can collapse them: a self-loop (source == target) is yielded
        // by BOTH the Outgoing and Incoming iterators, pushing the same
        // `EdgeIndex` twice. Without dedup the second `remove_edge` would delete
        // an innocent relocated edge.
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        to_remove.dedup();
        for ei in to_remove {
            self.graph.remove_edge(ei);
        }
    }

    /// Outgoing neighbors of a fact.
    #[must_use]
    pub fn neighbors(&self, fact_id: i64) -> Vec<i64> {
        self.node_map.get(&fact_id).map_or_else(Vec::new, |&idx| {
            self.graph
                .neighbors_directed(idx, Direction::Outgoing)
                .map(|ni| self.graph[ni])
                .collect()
        })
    }

    /// Total degree (in + out) for importance scoring.
    #[must_use]
    pub fn degree(&self, fact_id: i64) -> usize {
        self.node_map.get(&fact_id).map_or(0, |&idx| {
            self.graph
                .neighbors_directed(idx, Direction::Outgoing)
                .count()
                + self
                    .graph
                    .neighbors_directed(idx, Direction::Incoming)
                    .count()
        })
    }

    /// All fact ids in the connected component containing `fact_id`.
    ///
    /// Treats the directed graph as undirected for connectivity.
    #[must_use]
    pub fn connected_component(&self, fact_id: i64) -> Vec<i64> {
        let Some(&start) = self.node_map.get(&fact_id) else {
            return vec![];
        };

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(start);
        visited.insert(start);

        while let Some(node) = queue.pop_front() {
            // Outgoing
            for neighbor in self.graph.neighbors_directed(node, Direction::Outgoing) {
                if visited.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
            // Incoming
            for neighbor in self.graph.neighbors_directed(node, Direction::Incoming) {
                if visited.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
        }

        visited.iter().map(|&ni| self.graph[ni]).collect()
    }

    /// Number of nodes in the graph.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges in the graph.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Rebuild the graph from all active (non-expired) edges in `SQLite`.
    ///
    /// This is the recovery path if the in-memory graph ever drifts from
    /// the database. Called on `MemoryEngine::open`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn load_from_db(conn: &Connection) -> Result<Self> {
        let store = EdgeStore::new(conn);
        let active_edges = store.list_active()?;
        Ok(Self::from_active_edges(&active_edges))
    }

    /// Build the graph from an already-loaded set of active edges.
    ///
    /// The connection-free core of [`load_from_db`](Self::load_from_db): the async
    /// engine rebuilds its in-memory graph from the `list_active_edges` port read
    /// (e.g. after consolidation or a dream cycle expires facts) without reaching a
    /// raw `&Connection`. Isolated nodes are not represented (edges only), matching
    /// `load_from_db` semantics exactly.
    #[must_use]
    pub fn from_active_edges(active_edges: &[crate::types::Edge]) -> Self {
        let mut graph = Self::new();
        for edge in active_edges {
            graph.add_edges_from_iter(
                edge.source_fact_id,
                edge.target_fact_id,
                edge.id,
                &edge.relation_type,
                edge.weight,
            );
        }
        graph
    }

    /// Extract all edges for snapshotting.
    ///
    /// No isolated nodes — matches `load_from_db` semantics (edges only).
    pub(crate) fn to_snapshot(&self) -> crate::engine::snapshot::GraphSnapshot {
        use crate::engine::snapshot::{GraphEdgeSnapshot, GraphSnapshot};
        let edges = self
            .graph
            .edge_references()
            .map(|e| {
                let source = self.graph[e.source()];
                let target = self.graph[e.target()];
                let data = e.weight();
                GraphEdgeSnapshot {
                    edge_id: data.edge_id,
                    source,
                    target,
                    relation_type: data.relation_type.clone(),
                    weight: data.weight,
                }
            })
            .collect();
        GraphSnapshot { edges }
    }

    /// Rebuild graph from a snapshot (same logic as `load_from_db`).
    pub(crate) fn from_snapshot(snap: &crate::engine::snapshot::GraphSnapshot) -> Self {
        let mut graph = Self::new();
        for edge in &snap.edges {
            graph.add_edges_from_iter(
                edge.source,
                edge.target,
                edge.edge_id,
                &edge.relation_type,
                edge.weight,
            );
        }
        graph
    }

    /// Shared edge-insertion kernel used by `load_from_db` and `from_snapshot`.
    fn add_edges_from_iter(
        &mut self,
        source: i64,
        target: i64,
        edge_id: i64,
        relation_type: &str,
        weight: f64,
    ) {
        self.add_edge(
            source,
            target,
            EdgeData {
                edge_id,
                relation_type: relation_type.to_owned(),
                weight,
            },
        );
    }
}

impl Default for MemoryGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::store::schema::{init_schema, open_memory};
    use crate::types::NewEdge;

    fn make_edge_data(id: i64, rel: &str) -> EdgeData {
        EdgeData {
            edge_id: id,
            relation_type: rel.to_string(),
            weight: 1.0,
        }
    }

    #[test]
    fn add_node_and_query() {
        let mut g = MemoryGraph::new();
        g.ensure_node(42);
        assert!(g.has_node(42));
        assert!(!g.has_node(99));
    }

    #[test]
    fn add_edge_and_neighbors() {
        let mut g = MemoryGraph::new();
        g.add_edge(1, 2, make_edge_data(1, "related"));
        assert_eq!(g.neighbors(1), vec![2]);
        assert!(g.neighbors(2).is_empty()); // directed: no outgoing from 2
    }

    #[test]
    fn degree_counts_both_directions() {
        let mut g = MemoryGraph::new();
        g.add_edge(1, 2, make_edge_data(1, "a"));
        g.add_edge(1, 3, make_edge_data(2, "b"));
        g.add_edge(4, 1, make_edge_data(3, "c"));
        // node 1: 2 outgoing + 1 incoming = 3
        assert_eq!(g.degree(1), 3);
        // node 2: 0 outgoing + 1 incoming = 1
        assert_eq!(g.degree(2), 1);
        // node 4: 1 outgoing + 0 incoming = 1
        assert_eq!(g.degree(4), 1);
        // unknown node
        assert_eq!(g.degree(99), 0);
    }

    #[test]
    fn connected_component_undirected() {
        let mut g = MemoryGraph::new();
        // Chain: 1 → 2 → 3
        g.add_edge(1, 2, make_edge_data(1, "a"));
        g.add_edge(2, 3, make_edge_data(2, "b"));
        // Isolated node
        g.ensure_node(4);

        let mut comp = g.connected_component(1);
        comp.sort_unstable();
        assert_eq!(comp, vec![1, 2, 3]);

        let comp4 = g.connected_component(4);
        assert_eq!(comp4, vec![4]);

        // Unknown node returns empty
        assert!(g.connected_component(99).is_empty());
    }

    #[test]
    fn remove_edge_by_id() {
        let mut g = MemoryGraph::new();
        g.add_edge(1, 2, make_edge_data(10, "a"));
        g.add_edge(1, 3, make_edge_data(20, "b"));
        assert_eq!(g.edge_count(), 2);

        g.remove_edge_by_id(10);
        assert_eq!(g.edge_count(), 1);
        assert!(g.neighbors(1) == vec![3]);
    }

    #[test]
    fn remove_node_keeps_surviving_nodes_accessible() {
        let mut g = MemoryGraph::new();
        // Build a chain so petgraph assigns: 1→NodeIndex(0), 2→NodeIndex(1),
        // 3→NodeIndex(2). Removing node 1 (NodeIndex(0)) makes petgraph
        // swap-remove the last node (3, at NodeIndex(2)) into slot 0, so
        // node_map[3] would point at the now-removed last slot without the fix.
        g.add_edge(1, 2, make_edge_data(10, "a"));
        g.add_edge(2, 3, make_edge_data(20, "b"));
        assert_eq!(g.node_count(), 3);

        // Remove a NON-last node to force a displacement.
        g.remove_node(1);

        // Node 1 is gone.
        assert!(!g.has_node(1));
        assert_eq!(g.node_count(), 2);

        // The displaced node (3) and the untouched node (2) must still resolve
        // to their correct edges/neighbors — the edge 2→3 survives.
        assert!(g.has_node(2));
        assert!(g.has_node(3));
        assert_eq!(g.neighbors(2), vec![3]);
        assert_eq!(g.degree(2), 1);
        assert_eq!(g.degree(3), 1);

        // Connectivity of the surviving component must be intact.
        let mut comp = g.connected_component(3);
        comp.sort_unstable();
        assert_eq!(comp, vec![2, 3]);
    }

    #[test]
    fn remove_node_in_loop_keeps_map_consistent() {
        // The single-removal test above exercises `remove_node`'s re-index guard
        // exactly once, so it cannot catch a regression that only COMPOUNDS over
        // a sequence of removals — the documented loop-safe contract. This test
        // removes two non-last nodes in sequence, and crucially the SECOND node
        // removed is one that was itself DISPLACED (swap-relocated) by the first
        // removal. A stale-map regression therefore compounds: the first removal
        // leaves a wrong index that the second removal then builds another
        // wrong index on top of.
        //
        // Layout (verified against petgraph 0.7 swap-remove order):
        //   insertion order fixes NodeIndex: 1→idx0, 2→idx1, 3→idx2, 4→idx3,
        //   5→idx4. Edges form a chain 1→2→3→4→5 plus a shortcut 1→5.
        //
        //   remove_node(1) frees idx0; petgraph swaps the last node (fact 5 at
        //   idx4) into idx0. The guard rewrites node_map[5]=idx0.
        //   remove_node(5) — fact 5 now lives at idx0 (it was displaced) — frees
        //   idx0 again; petgraph swaps the new last node (fact 4 at idx3) into
        //   idx0. The guard must rewrite node_map[4]=idx0.
        //
        // Without the guard, node_map[4] keeps its stale original idx3. After two
        // swap-removes idx3 no longer holds fact 4, so neighbors(4)/degree(4)
        // resolve against the wrong slot — empirically reporting a phantom 4→5
        // edge that should be gone. The assertions below pin the correct (guarded)
        // state and fail under that regression.
        let mut g = MemoryGraph::new();
        g.add_edge(1, 2, make_edge_data(10, "a")); // 1→idx0, 2→idx1
        g.add_edge(2, 3, make_edge_data(20, "b")); // 3→idx2
        g.add_edge(3, 4, make_edge_data(30, "c")); // 4→idx3
        g.add_edge(4, 5, make_edge_data(40, "d")); // 5→idx4
        g.add_edge(1, 5, make_edge_data(50, "e")); // shortcut
        assert_eq!(g.node_count(), 5);

        // --- First removal: a non-last node, displacing fact 5 into its slot. ---
        g.remove_node(1);
        assert!(!g.has_node(1));
        assert_eq!(g.node_count(), 4);
        // Every survivor still resolves to its correct neighbors after removal #1.
        assert_eq!(g.neighbors(2), vec![3]);
        assert_eq!(g.neighbors(3), vec![4]);
        assert_eq!(g.neighbors(4), vec![5]);
        assert!(g.neighbors(5).is_empty());
        // Fact 5 was the displaced node; the 4→5 edge must still be visible.
        assert_eq!(g.degree(5), 1);

        // --- Second removal: the DISPLACED node itself, compounding the swap. ---
        g.remove_node(5);
        assert!(!g.has_node(5));
        assert_eq!(g.node_count(), 3);

        // After the loop, the whole map must be consistent. Fact 4 (the node now
        // swapped into the freed slot) is the canary: its only out-edge (4→5) is
        // gone with fact 5, so its outgoing neighbors are empty. Under the stale-
        // map regression this wrongly reports a phantom [5].
        assert_eq!(g.neighbors(4), Vec::<i64>::new());
        // 4 still has its incoming edge 3→4, so total degree is the inbound 1.
        assert_eq!(g.degree(4), 1);

        // The untouched interior nodes resolve correctly end to end.
        assert!(g.has_node(2));
        assert!(g.has_node(3));
        assert!(g.has_node(4));
        assert_eq!(g.neighbors(2), vec![3]);
        assert_eq!(g.neighbors(3), vec![4]);
        assert_eq!(g.degree(2), 1);
        assert_eq!(g.degree(3), 2); // 2→3 (in) + 3→4 (out)

        // The surviving chain 2→3→4 is one connected component, nothing leaked.
        let mut comp = g.connected_component(2);
        comp.sort_unstable();
        assert_eq!(comp, vec![2, 3, 4]);
        assert_eq!(g.edge_count(), 2); // only 2→3 and 3→4 remain
    }

    #[test]
    fn remove_edges_by_fact_with_self_loop() {
        let mut g = MemoryGraph::new();
        // Self-loop on fact 1, plus a genuine edge 1→2 and an unrelated edge 3→4.
        // The self-loop is yielded by BOTH the Outgoing and Incoming iterators,
        // so its EdgeIndex is collected twice — without dedup the second remove
        // would swap-delete an innocent edge.
        g.add_edge(1, 1, make_edge_data(10, "self"));
        g.add_edge(1, 2, make_edge_data(20, "a"));
        g.add_edge(3, 4, make_edge_data(30, "unrelated"));
        assert_eq!(g.edge_count(), 3);

        // Must not panic (the double-collected self-loop would otherwise try to
        // remove an already-relocated index).
        g.remove_edges_by_fact(1);

        // The self-loop (1→1) and the fact's edge (1→2) are gone.
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.degree(1), 0);
        assert!(g.neighbors(1).is_empty());

        // The unrelated edge 3→4 must survive intact, right endpoints and all.
        assert_eq!(g.neighbors(3), vec![4]);
        assert_eq!(g.degree(3), 1);
        assert_eq!(g.degree(4), 1);
    }

    #[test]
    fn remove_edges_by_fact_multi_edge_preserves_others() {
        let mut g = MemoryGraph::new();
        // This exact layout reproduces the swap-remove invalidation against real
        // petgraph 0.7: removing fact 2's edges frees a non-last slot, so
        // petgraph swap-relocates the current last edge into it, invalidating a
        // still-pending cached EdgeIndex in `to_remove`. The buggy
        // collection-order removal then either skips that stale (now
        // out-of-bounds) index — LEAKING a fact-2 edge that should be gone — or
        // deletes the wrong relocated edge. Verified empirically: under the bug,
        // edge 13 (3→2) survives; the fix removes all of fact 2's edges and
        // leaves only the genuine survivor 11 (1→3).
        //
        // Edges touching fact 2: 1→2 (in), 2→1 (out), 3→2 (in).
        // The only edge NOT touching fact 2: 1→3 (must survive verbatim).
        g.add_edge(1, 2, make_edge_data(10, "drop_in")); // EdgeIndex 0
        g.add_edge(1, 3, make_edge_data(11, "survivor")); // EdgeIndex 1
        g.add_edge(2, 1, make_edge_data(12, "drop_out")); // EdgeIndex 2
        g.add_edge(3, 2, make_edge_data(13, "drop_in2")); // EdgeIndex 3
        assert_eq!(g.edge_count(), 4);

        g.remove_edges_by_fact(2);

        // Exactly one edge survives — the only one not touching fact 2.
        assert_eq!(g.edge_count(), 1);

        // Fact 2 is fully disconnected: none of its three edges leaked.
        assert_eq!(g.degree(2), 0);
        assert!(g.neighbors(2).is_empty());

        // The survivor 1→3 resolves to the correct endpoints/degree.
        assert_eq!(g.neighbors(1), vec![3]);
        assert_eq!(g.degree(1), 1);
        assert_eq!(g.degree(3), 1);

        // Cross-check via the snapshot: exactly the survivor, all data intact.
        let snap = g.to_snapshot();
        assert_eq!(snap.edges.len(), 1);
        let e = &snap.edges[0];
        assert_eq!(e.edge_id, 11);
        assert_eq!(e.source, 1);
        assert_eq!(e.target, 3);
        assert_eq!(e.relation_type, "survivor");
        assert!((e.weight - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn load_from_db_skips_expired() {
        let conn = setup_db();
        let store = EdgeStore::new(&conn);

        let now = Utc::now();
        let e1 = NewEdge {
            source_fact_id: 1,
            target_fact_id: 2,
            relation_type: "active".to_string(),
            weight: 1.0,
            scope_id: 1,
            t_created: now,
            t_expired: None,
        };
        let e2 = NewEdge {
            source_fact_id: 2,
            target_fact_id: 3,
            relation_type: "expired".to_string(),
            weight: 1.0,
            scope_id: 1,
            t_created: now,
            t_expired: None,
        };

        let id1 = store.insert(&e1).unwrap();
        let id2 = store.insert(&e2).unwrap();
        store.expire(id2, now).unwrap();

        let graph = MemoryGraph::load_from_db(&conn).unwrap();
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
}
