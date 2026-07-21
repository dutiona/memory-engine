//! The SQL-touching half of the dedup pass, moved below the seam (Wave 2 #816 / S2,
//! sub-PR 2b). `compute_dedup` (pure) stays in the facade's `consolidation::dedup`,
//! which re-exports [`apply_dedup`] so its own `#[cfg(test)]`-only `local_dedup` wrapper
//! (composing `compute_dedup` + `apply_dedup` for that file's unit tests) keeps
//! resolving unchanged.

use chrono::{DateTime, Utc};
use rusqlite::Transaction;

use me_types::error::{MemoryError, Result};
use me_types::types::consolidation::DedupComputed;

use crate::store::edges::EdgeStore;
use crate::store::facts::FactStore;

/// Apply a computed dedup plan inside the caller's write context (#409).
///
/// The base- and score-importance inheritances first (while every fact is still active),
/// then the expirations and their edge cascades. Returns a [`DedupApplied`] — the ids
/// actually expired by **this** call (so the engine drives `notify_expire`/stats off what
/// truly changed, not the stale plan) plus whether a survivor was lost in the gap (so the
/// caller can skip the now-stale summary writes).
///
/// # Concurrency — the read→write gap (#409)
///
/// The engine releases the write lock between snapshotting the active set and applying
/// the plan, so another writer (`prune`, conflict resolution, the dream cycle) may have
/// expired a planned-for fact in between. Two cases are handled:
///
/// - **Loser concurrently expired:** `FactStore::expire` returns
///   [`MemoryError::NotFound`]; tolerated as "already done" (the end state holds) and not
///   counted as expired-by-us.
/// - **Survivor concurrently expired:** the merge decision is void — expiring the loser
///   too would leave the duplicate group with no active representative. So liveness of
///   every survivor is **snapshotted up front** (before any expiry), and a loser is
///   expired only if its survivor was still active at that point; otherwise the loser is
///   kept. Snapshotting first (rather than checking at expiry time) is what keeps chains
///   correct: an intermediate survivor that this call legitimately expires must not be
///   mistaken for a concurrently-expired one.
///
/// The single-connection wrapper/test path has no concurrent writer, so every survivor is
/// live and every planned loser is expired — behavior identical to the old pass.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on SQL failure, or `MemoryError::NotFound` from an
/// importance update if a fact row is missing (facts are soft-deleted, so this does not
/// fire for a merely-expired row).
pub fn apply_dedup(
    tx: &Transaction,
    embed_dim: usize,
    computed: &DedupComputed,
    now: DateTime<Utc>,
) -> Result<DedupApplied> {
    let fact_store = FactStore::new(tx, embed_dim);
    let edge_store = EdgeStore::new(tx);

    for &(id, importance) in &computed.importance_updates {
        fact_store.update_base_importance(id, importance)?;
    }
    for &(id, score) in &computed.importance_score_updates {
        fact_store.update_importance_score(id, score)?;
    }

    // Snapshot each distinct survivor's liveness BEFORE expiring anything, so a survivor
    // this call itself expires (it may also be a loser in a merge chain) is not confused
    // with one a concurrent writer expired (#409, survivor case).
    let mut survivor_live: std::collections::HashMap<i64, bool> = std::collections::HashMap::new();
    for e in &computed.expirations {
        if let std::collections::hash_map::Entry::Vacant(slot) = survivor_live.entry(e.survivor) {
            slot.insert(fact_store.is_active(e.survivor)?);
        }
    }

    let mut expired = Vec::with_capacity(computed.expirations.len());
    let mut survivor_lost = false;
    for e in &computed.expirations {
        // Survivor concurrently expired → the merge is void; keep the loser as the group's
        // representative instead of orphaning the cluster. Flag it so the caller skips the
        // now-stale summary writes (the plan clustered the survivors *without* this loser).
        if !survivor_live[&e.survivor] {
            survivor_lost = true;
            continue;
        }
        match fact_store.expire(e.loser, now) {
            Ok(()) => {
                edge_store.expire_by_fact(e.loser, now)?;
                expired.push(e.loser);
            }
            // Loser concurrently expired between snapshot and apply — the desired end
            // state already holds, so it is a no-op, not a failure, and not counted as
            // expired by this call (#409, loser case). Summaries stay valid: the loser was
            // going to be removed from the cluster set anyway, just by another writer.
            Err(MemoryError::NotFound(_)) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(DedupApplied {
        expired,
        survivor_lost,
    })
}

/// Outcome of [`apply_dedup`]: the ids actually expired by this call, and whether any
/// planned loser was **kept** because its survivor was concurrently expired (#409). The
/// latter signals that the cluster/global summaries in the plan are stale — they were
/// computed over the survivors with that loser removed — so the caller should skip the
/// summary writes and let the next consolidation rebuild them.
///
/// Fields are `pub` (not `pub(super)` as in the pre-carve monolith): `apply_plan`
/// (`super`, same crate) still destructures them, but the facade's own
/// `#[cfg(test)]`-only `local_dedup` wrapper reads `.expired` across the crate boundary
/// too, which `pub(super)` cannot reach from a different crate.
pub struct DedupApplied {
    /// Ids the call actually expired (excludes concurrently-expired or kept losers).
    pub expired: Vec<i64>,
    /// A survivor disappeared in the read→write gap, so a planned loser was kept and the
    /// plan's summaries no longer match the store.
    pub survivor_lost: bool,
}
