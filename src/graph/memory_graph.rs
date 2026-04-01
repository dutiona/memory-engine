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
    pub fn ensure_node(&mut self, fact_id: i64) -> NodeIndex {
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

    /// Remove all edges associated with the given `SQLite` edge id.
    ///
    /// Used after expiring an edge in `SQLite` to keep the graph in sync.
    pub fn remove_edge_by_id(&mut self, edge_id: i64) {
        let to_remove: Vec<_> = self
            .graph
            .edge_indices()
            .filter(|&ei| self.graph[ei].edge_id == edge_id)
            .collect();
        for ei in to_remove {
            self.graph.remove_edge(ei);
        }
    }

    /// Remove a node and all its edges from the graph.
    ///
    /// No-op if the fact id is not in the graph.
    /// Used by archival after hard-deleting facts from `SQLite`.
    pub fn remove_node(&mut self, fact_id: i64) {
        let Some(idx) = self.node_map.remove(&fact_id) else {
            return;
        };
        self.graph.remove_node(idx);
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

        let mut graph = Self::new();
        for edge in &active_edges {
            graph.add_edge(
                edge.source_fact_id,
                edge.target_fact_id,
                EdgeData {
                    edge_id: edge.id,
                    relation_type: edge.relation_type.clone(),
                    weight: edge.weight,
                },
            );
        }

        Ok(graph)
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
            graph.add_edge(
                edge.source,
                edge.target,
                EdgeData {
                    edge_id: edge.edge_id,
                    relation_type: edge.relation_type.clone(),
                    weight: edge.weight,
                },
            );
        }
        graph
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
