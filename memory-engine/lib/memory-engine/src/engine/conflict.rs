use chrono::Utc;

use crate::error::Result;
use crate::graph::EdgeData;
use crate::traits::{ConflictArbiter, ConflictResolution, CrudDecision};
use crate::types::NewFact;

use super::MemoryEngine;

impl MemoryEngine {
    /// Relation type for the edge created when both facts coexist (`Add`). Stable
    /// on-wire / DB string — must not change.
    const CONFLICT_SUPPLEMENTS_RELATION: &str = "supplements";
    /// Relation type for the edge created when the new fact supersedes the old
    /// one (`Update`). Stable on-wire / DB string — must not change.
    const CONFLICT_CONTRADICTS_RELATION: &str = "contradicts";
    /// Default weight for conflict-resolution edges (ported verbatim from the
    /// former `crate::conflict::temporal::DEFAULT_EDGE_WEIGHT`).
    const CONFLICT_EDGE_WEIGHT: f64 = 1.0;

    /// Resolve a conflict between an existing fact and a candidate new fact.
    ///
    /// Delegates the decision to the consumer-provided [`ConflictArbiter`].
    /// Graph is updated only after the persistence operations succeed.
    ///
    /// **Arbiter input caveat:** the `new_fact` passed to
    /// [`ConflictArbiter::arbitrate`] is a pre-insert synthetic `Fact` built via
    /// [`Fact::from_new_for_arbiter`](crate::types::Fact). Its `id` is always `0`
    /// (not yet assigned by the DB) and `importance_score` is the
    /// [`Fact::UNSCORED_IMPORTANCE`](crate::types::Fact::UNSCORED_IMPORTANCE)
    /// sentinel (`0.5`), NOT the eventual stored score. Arbiters must rely on
    /// `content`, `fact_type`, `base_importance`, and `metadata` — never on `id`
    /// or `importance_score`.
    ///
    /// **Graph/DB consistency:** the in-memory graph is updated only after the DB
    /// commit succeeds. A panic in the small window between the commit and the
    /// graph mirror leaves the graph and DB transiently diverged for the rest of
    /// the session; the next `open()` recovers the graph via
    /// `MemoryGraph::load_from_db`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::NotFound` if the old fact doesn't exist.
    /// Propagates errors from the arbiter or database operations.
    // One match dispatches the 4 CRUD decisions; splitting per-arm helpers would
    // scatter the persist→graph-mirror ordering invariant across functions.
    #[allow(clippy::too_many_lines)]
    pub async fn resolve_conflict(
        &self,
        arbiter: &dyn ConflictArbiter,
        old_id: i64,
        new_fact: &NewFact,
    ) -> Result<ConflictResolution> {
        self.ensure_open()?;
        // The candidate fact is persisted verbatim on an Add/Update decision, so
        // it is a consumer ingest path and must respect the same size bound as
        // `add_fact` (issue #572 / L10).
        crate::limits::check_new_fact(new_fact)?;

        let now = Utc::now();

        // Read the old fact through the port (was `FactStore::get` inside the old
        // free-function transaction).
        let old_fact = self.storage.get_fact(old_id).await?;

        // Build a temporary Fact from NewFact for the arbiter (it needs both as
        // Fact). Ported verbatim from `crate::conflict::temporal::resolve_conflict`.
        let new_as_fact = crate::types::Fact::from_new_for_arbiter(new_fact);

        // CONSUMER TRAIT — the main loop drives this; left exactly as-is (rule 5).
        let decision = arbiter.arbitrate(&old_fact, &new_as_fact)?;

        // The arbiter's decision (consumer trait) is made above, engine-side. The DB
        // writes it implies now run in ONE transaction below the seam via
        // `resolve_conflict_atomic` — restoring the all-or-nothing semantics the
        // cutover's per-call port decomposition had lost (a mid-sequence failure could
        // otherwise leave the old fact expired+invalidated with no inserted successor =
        // silent data loss). The in-memory graph is mirrored only AFTER the commit.
        let relation = match decision {
            CrudDecision::Add => Self::CONFLICT_SUPPLEMENTS_RELATION,
            CrudDecision::Update => Self::CONFLICT_CONTRADICTS_RELATION,
            // Delete/Noop create no edge; the relation string is unused by the port.
            CrudDecision::Delete | CrudDecision::Noop => "",
        };

        let (new_fact_id, edge_id) = self
            .storage
            .resolve_conflict_atomic(
                decision,
                old_id,
                new_fact,
                relation,
                Self::CONFLICT_EDGE_WEIGHT,
                now,
            )
            .await?;

        // Mirror the in-memory graph AFTER the commit (no guard held across `.await`).
        {
            let mut graph = self.graph.write();
            match decision {
                CrudDecision::Add => {
                    if let (Some(new_id), Some(edge_id)) = (new_fact_id, edge_id) {
                        graph.add_edge(
                            new_id,
                            old_id,
                            EdgeData {
                                edge_id,
                                relation_type: relation.to_string(),
                                weight: Self::CONFLICT_EDGE_WEIGHT,
                            },
                        );
                    }
                }
                CrudDecision::Update => {
                    graph.remove_edges_by_fact(old_id);
                    if let (Some(new_id), Some(edge_id)) = (new_fact_id, edge_id) {
                        graph.add_edge(
                            new_id,
                            old_id,
                            EdgeData {
                                edge_id,
                                relation_type: relation.to_string(),
                                weight: Self::CONFLICT_EDGE_WEIGHT,
                            },
                        );
                    }
                }
                CrudDecision::Delete => {
                    graph.remove_edges_by_fact(old_id);
                }
                CrudDecision::Noop => {}
            }
        }

        Ok(ConflictResolution {
            decision,
            old_fact_id: old_id,
            new_fact_id,
        })
    }
}
