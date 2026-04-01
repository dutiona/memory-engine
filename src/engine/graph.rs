use chrono::Utc;

use crate::error::{MemoryError, Result};
use crate::graph::EdgeData;
use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;
use crate::types::NewEdge;

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Co-session edge creation ---

    /// Relation type for edges linking facts that co-occur in the same session.
    pub(super) const CO_SESSION_RELATION: &str = "co_session";
    /// Default weight for co-session edges — weaker than explicit semantic
    /// relationships by design intent. Note: the current forgetting system uses
    /// raw `graph.degree()` (unweighted), so the weight does not yet reduce
    /// connectivity impact. It will matter once weighted traversal ships (Phase 5).
    pub(super) const CO_SESSION_WEIGHT: f64 = 0.5;
    /// Scope ID for co-session edges — root scope, since co-session is cross-scope.
    pub(super) const CO_SESSION_SCOPE_ID: i64 = 1;

    /// Create `co_session` edges between all active facts sharing a session.
    ///
    /// Edges are bidirectional (A→B and B→A), with weight
    /// [`CO_SESSION_WEIGHT`](Self::CO_SESSION_WEIGHT) and
    /// `scope_id =` [`CO_SESSION_SCOPE_ID`](Self::CO_SESSION_SCOPE_ID)
    /// (root — cross-scope by nature). Idempotent: calling twice for the same
    /// session does not create duplicate edges.
    ///
    /// When `scope` is `Some`, only facts within that scope subtree are
    /// considered. When `None`, all scopes are included (global lookup,
    /// backward-compatible).
    ///
    /// Returns the number of new edges created.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    /// Returns `MemoryError::NotFound` if `scope` is `Some` but the path
    /// does not exist.
    pub fn link_session_facts(&self, session_id: &str, scope: Option<&str>) -> Result<usize> {
        // Resolve scope path → subtree IDs (short-lived read lock)
        let scope_ids: Vec<i64> = match scope {
            Some(path) => {
                let tree = self.scope_tree.read();
                let id = tree
                    .resolve_path(path)
                    .ok_or_else(|| MemoryError::NotFound(format!("scope path: {path}")))?;
                tree.subtree(id)
            }
            None => Vec::new(),
        };

        let conn = self.write_conn()?;
        let facts =
            FactStore::new(&conn, self.embed_dim).list_active_by_session(session_id, &scope_ids)?;

        if facts.len() < 2 {
            drop(conn);
            return Ok(0);
        }

        let now = Utc::now();
        let mut new_edges: Vec<(i64, i64, i64)> = Vec::new(); // (edge_id, src, tgt)

        {
            let tx = conn.unchecked_transaction()?;
            let edge_store = EdgeStore::new(&tx);

            // Batch-fetch existing co_session edges for dedup (1 query instead of N²)
            let fact_ids: Vec<i64> = facts.iter().map(|f| f.id).collect();
            let existing =
                edge_store.list_active_pairs_by_facts(&fact_ids, Self::CO_SESSION_RELATION)?;

            for i in 0..facts.len() {
                for j in (i + 1)..facts.len() {
                    let a_id = facts[i].id;
                    let b_id = facts[j].id;

                    for (src, tgt) in [(a_id, b_id), (b_id, a_id)] {
                        if !existing.contains(&(src, tgt)) {
                            let edge_id = edge_store.insert(&NewEdge {
                                source_fact_id: src,
                                target_fact_id: tgt,
                                relation_type: Self::CO_SESSION_RELATION.to_string(),
                                weight: Self::CO_SESSION_WEIGHT,
                                scope_id: Self::CO_SESSION_SCOPE_ID,
                                t_created: now,
                                t_expired: None,
                            })?;
                            new_edges.push((edge_id, src, tgt));
                        }
                    }
                }
            }

            tx.commit()?;
        }
        drop(conn);

        // Sync in-memory graph after successful commit
        if !new_edges.is_empty() {
            let mut graph = self.graph.write();
            for &(edge_id, src, tgt) in &new_edges {
                graph.add_edge(
                    src,
                    tgt,
                    EdgeData {
                        edge_id,
                        relation_type: Self::CO_SESSION_RELATION.to_string(),
                        weight: Self::CO_SESSION_WEIGHT,
                    },
                );
            }
        }

        Ok(new_edges.len())
    }

    // --- Public API: Graph queries (no lock exposure) ---

    /// Get the degree (in + out edges) for a fact in the graph.
    #[must_use]
    pub fn graph_degree(&self, fact_id: i64) -> usize {
        self.graph.read().degree(fact_id)
    }

    /// Get the connected component containing a fact.
    #[must_use]
    pub fn graph_component(&self, fact_id: i64) -> Vec<i64> {
        self.graph.read().connected_component(fact_id)
    }

    /// Get outgoing neighbors of a fact in the graph.
    #[must_use]
    pub fn graph_neighbors(&self, fact_id: i64) -> Vec<i64> {
        self.graph.read().neighbors(fact_id)
    }

    /// Graph statistics: (node_count, edge_count).
    #[must_use]
    pub fn graph_stats(&self) -> (usize, usize) {
        let g = self.graph.read();
        (g.node_count(), g.edge_count())
    }

    /// Check if a node exists in the graph.
    #[must_use]
    pub fn graph_has_node(&self, fact_id: i64) -> bool {
        self.graph.read().has_node(fact_id)
    }
}
