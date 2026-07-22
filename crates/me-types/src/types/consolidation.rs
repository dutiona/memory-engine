//! Consolidation-pipeline seam types (relocated from the monolith's
//! `consolidation/mod.rs` + `consolidation/dedup.rs`, Wave 2 #816 E.4b Phase B).
//!
//! Pure data passed between the monolith's lock-free compute phase and its
//! atomic apply phase (#409). The consolidation *logic* (`load_snapshot`,
//! `compute_plan`, `apply_plan`, `compute_dedup`, `apply_dedup`, ...) stays in
//! the monolith; only the data these functions build and consume lives here.
//!
//! Fields are pub for cross-crate (workspace) access; encapsulation is at the
//! module-tree level.

use chrono::{DateTime, Utc};

use crate::types::{EmbeddingFingerprint, Fact, NewSummary};

/// An immutable read-phase snapshot of everything `compute_plan` needs.
///
/// Captured under a brief lock and then released so the compute phase runs
/// lock-free (#409).
///
/// Fields are pub for cross-crate (workspace) access; encapsulation is at the
/// module-tree level.
pub struct Snapshot {
    /// The active set, loaded once (#389). Empty when `over_both_caps` (the load was
    /// short-circuited, #659) — distinguished from a genuinely empty store by the flag.
    pub active_facts: Vec<Fact>,
    /// `last_consolidated_at` watermark scoping the dedup "new facts" set.
    pub last: Option<DateTime<Utc>>,
    /// Single run-level timestamp; the watermark advances to this on success.
    pub now: DateTime<Utc>,
    /// The corpus exceeded BOTH safety caps, so the (expensive) `list_active` load was
    /// skipped (#659): the plan is a complete no-op (dedup skipped, clustering skipped).
    pub over_both_caps: bool,
}

/// The fully-computed plan for one consolidation run — pure data.
///
/// Produced lock-free by `compute_plan` and applied atomically by `apply_plan`
/// (#409). Holds no borrow of the store, so it survives the gap between
/// releasing the read lock and acquiring the write lock.
///
/// Fields are pub for cross-crate (workspace) access; encapsulation is at the
/// module-tree level.
pub struct ConsolidationPlan {
    /// Dedup decision (expirations + importance inheritances) as data.
    pub dedup: DedupComputed,
    /// Whether clustering ran (vs. skipped over the cap). Gates the summary writes so
    /// existing summaries are preserved when the pass could not run (#345).
    pub cluster_ran: bool,
    /// New cluster summaries to write (already summarized + embedded).
    pub cluster_summaries: Vec<NewSummary>,
    /// New global summary to write, if any.
    pub global_summary: Option<NewSummary>,
    /// The embedder's fingerprint to stamp, set **iff** a summary vector was produced
    /// (#643). Captured during compute so `apply_plan` needs no embedder.
    pub embedding_fingerprint: Option<EmbeddingFingerprint>,
    /// Run-level timestamp (expiry stamp + watermark).
    pub now: DateTime<Utc>,
}

/// One planned expiry: a `loser` folded into its `survivor`.
///
/// The pairing is what lets `apply_dedup` honor the #409 read→write gap: if
/// the `survivor` was concurrently expired between the lock-free snapshot and
/// the write, the merge decision is void and the `loser` must be kept (not
/// expired), so the duplicate group is never left without an active
/// representative. `survivor` is the **immediate** survivor of the pairwise
/// decision; chain correctness comes from snapshotting every survivor's
/// liveness in `apply_dedup` *before* any expiry (see there).
///
/// Fields are pub for cross-crate (workspace) access; encapsulation is at the
/// module-tree level.
pub struct Expiry {
    /// The lower-importance fact to expire.
    pub loser: i64,
    /// The fact `loser` was folded into (the higher-importance member of the pair).
    pub survivor: i64,
}

/// Result of the pure dedup computation, as data rather than DB writes.
///
/// Which facts to expire (with their survivors) and which importance values
/// the survivors inherit. Produced by `compute_dedup` (no `Connection`, so it
/// runs lock-free during the engine's compute phase, #409) and applied by
/// `apply_dedup` inside the final write transaction. Base-`importance`
/// inheritance is carried separately from `importance_score`: under the
/// current expiry rule (always expire the lower-importance fact) the base
/// list is always empty, but it is materialized through the same guard so the
/// applied write set is provably identical to the old inline pass.
///
/// Fields are pub for cross-crate (workspace) access; encapsulation is at the
/// module-tree level.
pub struct DedupComputed {
    /// The corpus exceeded the cap, so the pass was skipped: no writes, and the
    /// caller must NOT advance the watermark.
    pub skipped: bool,
    /// Planned expiries (loser + its survivor), the lower-importance member of each
    /// duplicate pair paired with the fact it merges into.
    pub expirations: Vec<Expiry>,
    /// Base-`importance` inheritances `(survivor_id, importance)` — empty under the
    /// current expiry rule (kept for write-set fidelity; see the struct docs).
    pub importance_updates: Vec<(i64, f64)>,
    /// `importance_score` inheritances `(survivor_id, score)` — the #264 running-max,
    /// one final entry per survivor that absorbed a strictly-higher score.
    pub importance_score_updates: Vec<(i64, f64)>,
}

impl DedupComputed {
    /// The skipped (over-cap) result: no writes, watermark held.
    #[must_use]
    pub const fn skipped() -> Self {
        Self {
            skipped: true,
            expirations: Vec::new(),
            importance_updates: Vec::new(),
            importance_score_updates: Vec::new(),
        }
    }
}
