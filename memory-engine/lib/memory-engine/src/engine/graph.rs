use chrono::Utc;

use crate::error::{MemoryError, Result};
use crate::graph::EdgeData;
use crate::types::RelationType;

use super::MemoryEngine;

impl MemoryEngine {
    // --- Public API: Co-session edge creation ---

    /// Relation type (canonical wire string) for edges linking facts that
    /// co-occur in the same session. Derived from [`RelationType::CoSession`] so
    /// the string passed to the storage seam and the variant mirrored into the
    /// in-memory graph share a single source of truth.
    const CO_SESSION_RELATION: &str = RelationType::CoSession.as_str();
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
    /// `CO_SESSION_WEIGHT` and
    /// `scope_id = CO_SESSION_SCOPE_ID`
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
    /// Returns `MemoryError::Storage` on SQL failure.
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
                        relation_type: RelationType::CoSession,
                        weight: Self::CO_SESSION_WEIGHT,
                    },
                );
            }
        }

        Ok(new_edges.len())
    }

    // --- Public API: Edge expiry ---

    /// Soft-expire a single edge by id, keeping the in-memory graph in sync.
    ///
    /// Sets the edge's `t_expired` in the backend, then — only after the commit
    /// succeeds — drops it from the in-memory petgraph (via the crate-internal
    /// `MemoryGraph::remove_edge_by_id`).
    /// This is the edge counterpart of the `add_edge` mirror in
    /// [`link_session_facts`](Self::link_session_facts): without the graph step,
    /// degree/neighbor/component queries would keep reporting the expired edge
    /// until the next full rebuild (#879).
    ///
    /// **Graph/DB consistency:** the graph is mirrored only after the DB write
    /// returns `Ok`. A panic in the small window between commit and mirror leaves
    /// the graph transiently holding the stale edge for the rest of the session;
    /// the next `open()` recovers it via `MemoryGraph::load_from_db`. (Same
    /// post-commit-mirror contract as [`resolve_conflict`](Self::resolve_conflict).)
    ///
    /// # Errors
    ///
    /// - [`MemoryError::ReadOnly`](crate::error::MemoryError::ReadOnly) — the
    ///   engine was opened read-only (the backend rejects the write; the graph is
    ///   left untouched).
    /// - [`MemoryError::NotFound`](crate::error::MemoryError::NotFound) — no
    ///   *active* edge with `edge_id` exists (unknown id, or already expired); the
    ///   write affected 0 rows, so the graph is not modified.
    /// - [`MemoryError::Storage`](crate::error::MemoryError::Storage) on SQL
    ///   failure.
    pub async fn expire_edge(&self, edge_id: i64) -> Result<()> {
        let now = Utc::now();
        // DB write below the seam first. On NotFound/ReadOnly/Database the graph
        // is deliberately left untouched (no edge transitioned to expired).
        self.storage.expire_edge(edge_id, now).await?;

        // Mirror the in-memory graph AFTER the commit (no guard held across
        // `.await`). `remove_edge_by_id` is a no-op if the edge is absent from the
        // graph (e.g. graph built without this edge), so it is safe even when the
        // DB and graph briefly disagree.
        self.graph.write().remove_edge_by_id(edge_id);

        Ok(())
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
