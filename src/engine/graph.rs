use chrono::Utc;

use crate::error::{MemoryError, Result};
use crate::graph::EdgeData;

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Co-session edge creation ---

    /// Relation type for edges linking facts that co-occur in the same session.
    const CO_SESSION_RELATION: &str = "co_session";
    /// Default weight for co-session edges — weaker than explicit semantic
    /// relationships by design intent. Note: the current forgetting system uses
    /// raw `graph.degree()` (unweighted), so the weight does not yet reduce
    /// connectivity impact. It will matter once weighted traversal ships (Phase 5).
    pub(super) const CO_SESSION_WEIGHT: f64 = 0.5;
    /// Scope ID for co-session edges — root scope, since co-session is cross-scope.
    const CO_SESSION_SCOPE_ID: i64 = 1;

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
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Database` on SQL failure.
    /// Returns `MemoryError::NotFound` if `scope` is `Some` but the path
    /// does not exist.
    pub async fn link_session_facts(&self, session_id: &str, scope: Option<&str>) -> Result<usize> {
        // Resolve scope path → subtree IDs (short-lived read lock, dropped at the
        // end of the match arm so no guard is held across the awaits below).
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

        let facts = self
            .storage
            .list_active_facts_by_session(session_id, &scope_ids)
            .await?;

        if facts.len() < 2 {
            return Ok(0);
        }

        let now = Utc::now();
        let relation = Self::CO_SESSION_RELATION.to_string();

        // Batch-dedup + edge inserts run in one transaction below the seam
        // (`insert_cosession_edges_atomic`). The engine resolves `scope_ids`
        // before the call and updates the in-memory graph after it.
        let fact_ids: Vec<i64> = facts.iter().map(|f| f.id).collect();
        let new_edges: Vec<(i64, i64, i64)> = self
            .storage
            .insert_cosession_edges_atomic(
                &fact_ids,
                Self::CO_SESSION_RELATION,
                Self::CO_SESSION_WEIGHT,
                Self::CO_SESSION_SCOPE_ID,
                now,
            )
            .await?;

        // Sync in-memory graph after successful commit
        if !new_edges.is_empty() {
            let mut graph = self.graph.write();
            for &(edge_id, src, tgt) in &new_edges {
                graph.add_edge(
                    src,
                    tgt,
                    EdgeData {
                        edge_id,
                        relation_type: relation.clone(),
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

    /// Graph statistics: (`node_count`, `edge_count`).
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
