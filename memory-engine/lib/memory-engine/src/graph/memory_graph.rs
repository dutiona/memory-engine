use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use rusqlite::Connection;

use crate::error::Result;
use crate::store::edges::EdgeStore;

/// Hard upper bound on the number of edges accepted from a single snapshot
/// sidecar (50M).
///
/// Defense in depth (CWE-400 / CWE-770). [`MemoryGraph::from_snapshot`] iterates
/// the deserialized `snap.edges` and, per edge, allocates a petgraph edge plus up
/// to two nodes and two `HashMap` entries.
///
/// This 50M cap is a **cheap, fingerprint-less function-level invariant — not the
/// primary runtime defense.** At runtime it is subsumed twice over: the on-disk
/// sidecar is already gated by `MAX_SNAPSHOT_BYTES` (512 MiB, which bounds a
/// `MessagePack` edge stream to an ~9.4M-edge ceiling — well under this cap), and
/// the sole caller (`MemoryEngine::try_load_snapshot`) additionally cross-checks
/// the edge count against the already-validated `DbFingerprint::active_edge_count`
/// and discards on any disagreement. This constant is therefore the residual guard
/// that still holds for a caller with **no fingerprint in hand**, and it makes the
/// bound on `from_snapshot` explicit rather than implicit. It is chosen consistent
/// with `MAX_SNAPSHOT_BYTES`, so it never rejects a legitimate corpus.
const MAX_SNAPSHOT_EDGES: usize = 50_000_000;

/// Reject a snapshot edge count that exceeds [`MAX_SNAPSHOT_EDGES`].
///
/// Extracted as a free function so the bound is unit-testable without
/// materializing tens of millions of edge descriptors.
fn check_edge_count_bound(count: usize) -> Result<()> {
    if count > MAX_SNAPSHOT_EDGES {
        return Err(crate::error::MemoryError::Internal(format!(
            "snapshot edge count {count} exceeds the maximum of {MAX_SNAPSHOT_EDGES}; \
             discarding sidecar and rebuilding from the database"
        )));
    }
    Ok(())
}

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
    /// Intended for the edge-expiry graph-sync path (keep the in-memory graph in
    /// step after `EdgeStore::expire`), but that wiring is not yet landed, so the
    /// only current caller is the unit test below — hence `#[cfg(test)]` to keep
    /// the lint honest without an `#[allow(dead_code)]` mask. Drop the gate when a
    /// production caller is added (tracked as a follow-up to #276).
    #[cfg(test)]
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

    /// Rebuild graph from a snapshot, bounding and revalidating the edge set
    /// against the live set of *existing* fact ids.
    ///
    /// The `.snapshot` sidecar is an untrusted input: its blake3 checksum is
    /// *unkeyed* (a corruption check, not an authenticity control), so a local
    /// actor able to write the sidecar — or plain on-disk corruption — can present
    /// a structurally-invalid edge list. This builds the same edge-only graph as
    /// [`load_from_db`](Self::load_from_db) and validates against **exactly**
    /// `load_from_db`'s trust boundary — neither looser nor stricter.
    ///
    /// That boundary is the `edges.source_fact_id/target_fact_id REFERENCES
    /// facts(id)` foreign key, which `load_from_db` relies on to guarantee every
    /// loaded edge's endpoints *exist*. Crucially, the FK guarantees existence, not
    /// activeness: `SQLite` foreign keys cannot be conditional on `t_expired`, and an
    /// active edge legitimately references an *expired* fact (the conflict-resolution
    /// `contradicts` edge `new → old` is created active while `old` is expired in the
    /// same transaction; the dream-cycle `supersedes` edge `synthetic → src` stays
    /// active while every source is expired). The validation therefore checks every
    /// endpoint against the `existing_fact_ids` set (`SELECT id FROM facts`, any
    /// `t_expired`) — **all** facts, not active-only. Validating against the
    /// active-only set would be *stricter* than `load_from_db` and would falsely
    /// reject a snapshot that faithfully mirrors a real rebuild, defeating the
    /// snapshot optimization for a needless DB rebuild.
    ///
    /// Three defense-in-depth gates (#257, #412, #499):
    ///
    /// 1. **Count bound** — reject more than [`MAX_SNAPSHOT_EDGES`] edges before
    ///    any per-edge allocation (CWE-400 / CWE-770: unbounded petgraph/`HashMap`
    ///    growth at cold start). This is a cheap, fingerprint-less function-level
    ///    invariant, *not* the primary runtime defense: at runtime the on-disk
    ///    `MAX_SNAPSHOT_BYTES` byte cap (512 MiB, an ~9.4M-edge ceiling for
    ///    `MessagePack` edge records) plus the caller's tighter cross-check against
    ///    the validated `DbFingerprint::active_edge_count` already subsume it. It
    ///    is the floor that still holds for a caller with no fingerprint in hand.
    /// 2. **Endpoint positivity** — every `source`, `target`, and `edge_id` is a
    ///    `SQLite` rowid and is therefore strictly positive. A non-positive value
    ///    is structurally impossible (corruption or tamper); reject it. This is a
    ///    cheap pre-filter for gate 3.
    /// 3. **Referential validation** (#257) — every edge `source`/`target` MUST be
    ///    present in `existing_fact_ids` (the live `SELECT id FROM facts` set, any
    ///    `t_expired`). A positive-but-absent endpoint is a phantom-node injection:
    ///    it would otherwise materialize a node for a fact that does not exist. This
    ///    mirrors the FK guarantee `load_from_db` enjoys — existence, not activeness
    ///    — and closes the gap the positivity check alone left open, without
    ///    over-rejecting the legitimate active-edge-to-expired-fact case.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Internal`](crate::error::MemoryError::Internal) when
    /// the edge count exceeds the cap, any edge carries a non-positive
    /// `source`/`target`/`edge_id`, or any edge `source`/`target` is absent from
    /// `existing_fact_ids`. The caller treats this as "discard the sidecar and
    /// rebuild from the authoritative database"; it never panics.
    pub(crate) fn from_snapshot(
        snap: &crate::engine::snapshot::GraphSnapshot,
        existing_fact_ids: &HashSet<i64>,
    ) -> Result<Self> {
        check_edge_count_bound(snap.edges.len())?;

        let mut graph = Self::new();
        for edge in &snap.edges {
            // Gate 2: cheap structural pre-filter — rowids are strictly positive.
            if edge.source <= 0 || edge.target <= 0 || edge.edge_id <= 0 {
                return Err(crate::error::MemoryError::Internal(format!(
                    "snapshot edge {edge_id} carries a non-positive rowid \
                     (source={source}, target={target}, edge_id={edge_id}); \
                     discarding sidecar and rebuilding from the database",
                    edge_id = edge.edge_id,
                    source = edge.source,
                    target = edge.target,
                )));
            }
            // Gate 3: referential validation — both endpoints must reference a fact
            // that EXISTS (active or expired), matching the FK target `load_from_db`
            // trusts. A positive-but-absent id is a phantom-node injection.
            if !existing_fact_ids.contains(&edge.source)
                || !existing_fact_ids.contains(&edge.target)
            {
                return Err(crate::error::MemoryError::Internal(format!(
                    "snapshot edge {edge_id} references a fact id absent from the \
                     existing-fact set (source={source}, target={target}); discarding \
                     sidecar and rebuilding from the database",
                    edge_id = edge.edge_id,
                    source = edge.source,
                    target = edge.target,
                )));
            }
            graph.add_edges_from_iter(
                edge.source,
                edge.target,
                edge.edge_id,
                &edge.relation_type,
                edge.weight,
            );
        }
        Ok(graph)
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
    fn from_snapshot_accepts_valid_edges() {
        // Baseline: a well-formed snapshot (positive ids, within the cap, all
        // endpoints present in the existing fact set) builds the graph faithfully —
        // the revalidation does not reject good data.
        let snap = crate::engine::snapshot::GraphSnapshot {
            edges: vec![
                crate::engine::snapshot::GraphEdgeSnapshot {
                    edge_id: 1,
                    source: 10,
                    target: 20,
                    relation_type: "supplements".into(),
                    weight: 0.5,
                },
                crate::engine::snapshot::GraphEdgeSnapshot {
                    edge_id: 2,
                    source: 20,
                    target: 30,
                    relation_type: "related".into(),
                    weight: 1.0,
                },
            ],
        };
        let existing: HashSet<i64> = [10, 20, 30].into_iter().collect();
        let g = MemoryGraph::from_snapshot(&snap, &existing).expect("valid snapshot must build");
        assert_eq!(g.edge_count(), 2);
        assert_eq!(g.neighbors(10), vec![20]);
        assert_eq!(g.neighbors(20), vec![30]);
    }

    #[test]
    fn from_snapshot_accepts_edge_to_expired_fact() {
        // #257 regression guard: the validation set is the set of facts that
        // *exist* (active OR expired) — exactly `load_from_db`'s FK trust boundary,
        // NOT the stricter active-only set. `load_from_db` loads every active edge,
        // and an active edge legitimately references an expired fact (e.g. the
        // dream-cycle `supersedes` edge `synthetic → src`, where the source is
        // expired in the same cycle, or the conflict `contradicts` edge `new → old`
        // where `old` is expired). Here the endpoint `20` models a fact that exists
        // but is expired: it is present in `existing` (the `SELECT id FROM facts`
        // set), so the snapshot edge that mirrors what `load_from_db` would load MUST
        // be accepted — never falsely rejected into a needless full DB rebuild.
        let snap = crate::engine::snapshot::GraphSnapshot {
            edges: vec![crate::engine::snapshot::GraphEdgeSnapshot {
                edge_id: 1,
                source: 10, // active synthetic
                target: 20, // existing-but-EXPIRED source
                relation_type: "supersedes".into(),
                weight: 1.0,
            }],
        };
        // `20` exists (expired facts are still in `SELECT id FROM facts`).
        let existing: HashSet<i64> = [10, 20].into_iter().collect();
        let g = MemoryGraph::from_snapshot(&snap, &existing)
            .expect("an active edge to an existing-but-expired fact must be accepted");
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.neighbors(10), vec![20]);
    }

    #[test]
    fn from_snapshot_rejects_phantom_node_reference() {
        // #257 (true referential validation): an edge endpoint that is *positive*
        // but absent from the live existing-fact set is a phantom-node injection —
        // the positivity pre-filter alone cannot catch it. The snapshot path must
        // be as strict as `load_from_db` (whose endpoints are guaranteed present by
        // the DB foreign key), so such an edge is rejected with a typed error rather
        // than silently materializing a node that does not exist. Note: the set is
        // existing facts (active OR expired), so this rejection is for a *truly
        // nonexistent* id, not merely an expired one (see
        // `from_snapshot_accepts_edge_to_expired_fact`).
        let existing: HashSet<i64> = [10, 20].into_iter().collect();

        // Phantom target (30 does not exist).
        let snap_target = crate::engine::snapshot::GraphSnapshot {
            edges: vec![crate::engine::snapshot::GraphEdgeSnapshot {
                edge_id: 1,
                source: 10,
                target: 30,
                relation_type: "x".into(),
                weight: 1.0,
            }],
        };
        assert!(
            matches!(
                MemoryGraph::from_snapshot(&snap_target, &existing),
                Err(crate::error::MemoryError::Internal(_))
            ),
            "positive-but-nonexistent target must be rejected as a phantom node"
        );

        // Phantom source (99 does not exist).
        let snap_source = crate::engine::snapshot::GraphSnapshot {
            edges: vec![crate::engine::snapshot::GraphEdgeSnapshot {
                edge_id: 1,
                source: 99,
                target: 20,
                relation_type: "x".into(),
                weight: 1.0,
            }],
        };
        assert!(
            matches!(
                MemoryGraph::from_snapshot(&snap_source, &existing),
                Err(crate::error::MemoryError::Internal(_))
            ),
            "positive-but-nonexistent source must be rejected as a phantom node"
        );
    }

    #[test]
    fn from_snapshot_rejects_nonpositive_endpoint() {
        // #499 (graph/data-integrity-trusts-snapshot-edges): fact ids are SQLite
        // rowids and are always positive. An edge whose source/target is <= 0
        // cannot reference a real fact — it is a dangling/absent-node reference
        // (corruption or tamper) and must be rejected with a typed error, not
        // silently materialized into a phantom node. The cheap positivity check
        // fires before the set membership lookup, so an empty existing set suffices.
        let existing: HashSet<i64> = HashSet::new();
        for (source, target) in [(0, 20), (-1, 20), (10, 0), (10, -5)] {
            let snap = crate::engine::snapshot::GraphSnapshot {
                edges: vec![crate::engine::snapshot::GraphEdgeSnapshot {
                    edge_id: 1,
                    source,
                    target,
                    relation_type: "x".into(),
                    weight: 1.0,
                }],
            };
            assert!(
                matches!(
                    MemoryGraph::from_snapshot(&snap, &existing),
                    Err(crate::error::MemoryError::Internal(_))
                ),
                "non-positive endpoint ({source}, {target}) must be rejected"
            );
        }
    }

    #[test]
    fn from_snapshot_rejects_nonpositive_edge_id() {
        // An edge_id is a SQLite rowid too; <= 0 signals a corrupt/tampered
        // edge record. Endpoints are valid + existing so only the edge_id triggers.
        let existing: HashSet<i64> = [10, 20].into_iter().collect();
        let snap = crate::engine::snapshot::GraphSnapshot {
            edges: vec![crate::engine::snapshot::GraphEdgeSnapshot {
                edge_id: 0,
                source: 10,
                target: 20,
                relation_type: "x".into(),
                weight: 1.0,
            }],
        };
        assert!(matches!(
            MemoryGraph::from_snapshot(&snap, &existing),
            Err(crate::error::MemoryError::Internal(_))
        ));
    }

    #[test]
    fn from_snapshot_rejects_over_cap_edge_count() {
        // Defense-in-depth bound: a snapshot claiming more edges than
        // MAX_SNAPSHOT_EDGES is rejected before any per-edge allocation, so a
        // tampered sidecar cannot drive unbounded petgraph/HashMap growth at
        // cold start (CWE-400/770). We assert the *length gate* fires using a
        // tiny stand-in cap via the testable kernel, without allocating 50M
        // edges.
        // The cap is generously large; materializing cap+1 edge descriptors is
        // infeasible, so we exercise the bound-check kernel directly: exactly the
        // cap is accepted, one over is rejected.
        assert!(check_edge_count_bound(MAX_SNAPSHOT_EDGES).is_ok());
        let err = check_edge_count_bound(MAX_SNAPSHOT_EDGES + 1)
            .expect_err("over-cap edge count must be rejected");
        assert!(matches!(err, crate::error::MemoryError::Internal(_)));
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
