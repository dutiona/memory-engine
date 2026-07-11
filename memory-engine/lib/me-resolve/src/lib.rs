//! Bi-temporal conflict resolution: arbiter-driven CRUD on contradicting facts.
//!
//! Extracted from the facade in Wave 2 #816 / S3. The engine's
//! `MemoryEngine::resolve_conflict` is a one-line delegate over
//! [`resolve_conflict`], which operates on a [`MemoryCtx`] + the in-memory graph.

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use me_index::graph::{EdgeData, MemoryGraph};
use me_storage::MemoryCtx;
use me_traits::{ConflictArbiter, ConflictResolution, CrudDecision};
use me_types::error::Result;
use me_types::types::{Fact, NewFact, RelationType};

/// Relation for the edge created when both facts coexist (`Add`). Stable on-wire / DB string.
const CONFLICT_SUPPLEMENTS_RELATION: &str = "supplements";
/// Relation for the edge created when the new fact supersedes the old (`Update`). Stable on-wire / DB string.
const CONFLICT_CONTRADICTS_RELATION: &str = "contradicts";
/// Default weight for conflict-resolution edges (former `crate::conflict::temporal::DEFAULT_EDGE_WEIGHT`).
const CONFLICT_EDGE_WEIGHT: f64 = 1.0;

/// Resolve a conflict between an existing fact and a candidate new fact.
///
/// Delegates the decision to the consumer-provided [`ConflictArbiter`]; the graph
/// is mirrored only after the persistence op commits.
///
/// # Errors
/// - [`me_types::error::MemoryError::EmbeddingReopenRequired`] — the handle is dim-fenced (#742).
/// - Conflict/PayloadTooLarge — the candidate exceeds the size bound (`check_new_fact`).
/// - `NotFound` — `old_id` missing or already expired (re-validated inside the atomic op; #335 TOCTOU guard).
/// - Propagates arbiter / storage errors.
// One match dispatches the 4 CRUD decisions; splitting per-arm helpers would
// scatter the persist→graph-mirror ordering invariant across functions.
#[allow(clippy::too_many_lines)]
pub async fn resolve_conflict(
    ctx: MemoryCtx<'_>,
    graph: &RwLock<MemoryGraph>,
    arbiter: &dyn ConflictArbiter,
    old_id: i64,
    new_fact: &NewFact,
    now: DateTime<Utc>,
) -> Result<ConflictResolution> {
    ctx.ensure_open()?;
    // The candidate fact is persisted verbatim on an Add/Update decision, so
    // it is a consumer ingest path and must respect the same size bound as
    // `add_fact` (issue #572 / L10).
    me_types::limits::check_new_fact(new_fact)?;

    // Read the old fact through the port (was `FactStore::get` inside the old
    // free-function transaction).
    let old_fact = ctx.storage.get_fact(old_id).await?;

    // Build a temporary Fact from NewFact for the arbiter (it needs both as Fact).
    let new_as_fact = Fact::from_new_for_arbiter(new_fact);

    // CONSUMER TRAIT — the main loop drives this; left exactly as-is (rule 5).
    let decision = arbiter.arbitrate(&old_fact, &new_as_fact)?;

    // The arbiter's decision (consumer trait) is made above. The DB writes it
    // implies now run in ONE transaction below the seam via
    // `resolve_conflict_atomic` — restoring the all-or-nothing semantics the
    // cutover's per-call port decomposition had lost (a mid-sequence failure could
    // otherwise leave the old fact expired+invalidated with no inserted successor =
    // silent data loss). The in-memory graph is mirrored only AFTER the commit.
    let relation = match decision {
        CrudDecision::Add => CONFLICT_SUPPLEMENTS_RELATION,
        CrudDecision::Update => CONFLICT_CONTRADICTS_RELATION,
        // Delete/Noop create no edge; the relation string is unused by the port.
        CrudDecision::Delete | CrudDecision::Noop => "",
    };

    let (new_fact_id, edge_id) = ctx
        .storage
        .resolve_conflict_atomic(
            decision,
            old_id,
            new_fact,
            relation,
            CONFLICT_EDGE_WEIGHT,
            now,
        )
        .await?;

    // Mirror the in-memory graph AFTER the commit (no guard held across `.await`).
    {
        let mut g = graph.write();
        match decision {
            CrudDecision::Add => {
                if let (Some(new_id), Some(edge_id)) = (new_fact_id, edge_id) {
                    g.add_edge(
                        new_id,
                        old_id,
                        EdgeData {
                            edge_id,
                            relation_type: RelationType::from(relation),
                            weight: CONFLICT_EDGE_WEIGHT,
                        },
                    );
                }
            }
            CrudDecision::Update => {
                g.remove_edges_by_fact(old_id);
                if let (Some(new_id), Some(edge_id)) = (new_fact_id, edge_id) {
                    g.add_edge(
                        new_id,
                        old_id,
                        EdgeData {
                            edge_id,
                            relation_type: RelationType::from(relation),
                            weight: CONFLICT_EDGE_WEIGHT,
                        },
                    );
                }
            }
            CrudDecision::Delete => {
                g.remove_edges_by_fact(old_id);
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
