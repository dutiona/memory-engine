//! Delta-based cycle report types (R7).
//!
//! A [`DreamCycle`](crate::traits::DreamCycle) does not mutate the store directly.
//! Instead it returns a [`CycleReport`] — an ordered log of [`CycleDelta`] operations
//! plus a three-layer [`IdentityOutput`] and bookkeeping [`CycleMetadata`]. The engine
//! validates and applies the whole report atomically via
//! [`MemoryEngine::apply_cycle_report`](crate::MemoryEngine::apply_cycle_report).
//!
//! This is the ACE incremental-delta discipline (arXiv:2510.04618): a cycle *proposes*
//! a bounded, typed, replayable set of mutations rather than rewriting accumulated
//! state wholesale — making the DC context-collapse failure mode (arXiv:2504.07952)
//! structurally impossible for consumer LLM implementations. The shipped default
//! [`DefaultDreamCycle`](crate::DefaultDreamCycle) is pure-Rust and deterministic, so
//! it needs no such protection itself; the delta vocabulary exists for the *consumer*.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{FactId, NewFact, Outcome, PromotionProvenance};

/// Quantum for importance adjustments.
///
/// [`CycleDelta::AdjustScore`]'s `adjustment` is a **count of quanta** (±2 per cycle,
/// cumulative across cycles — see the synthesis rescoring policy). The store delta
/// applied to a fact's **base `importance`** is `adjustment as f64 * IMPORTANCE_STEP`,
/// clamped into `[0.0, 1.0]`. The adjustment targets the durable base `importance`
/// signal, **not** the materialized `importance_score` (which is recomputed by
/// Ebbinghaus decay and would overwrite a direct adjustment).
pub const IMPORTANCE_STEP: f64 = 0.05;

/// Maximum magnitude of a single [`CycleDelta::AdjustScore`] adjustment (±2 symmetric).
pub const MAX_ADJUSTMENT: i16 = 2;

/// A bounded, half-open real-world time window a cycle processed: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Three-layer behavioral identity output (ANCHORS / CORE / PREDICTIONS).
///
/// **This crate defines the type only.** The shipped [`DefaultDreamCycle`](crate::DefaultDreamCycle)
/// emits [`IdentityOutput::empty`]; the computation of the three layers is owned by #57.
///
/// Marked `#[non_exhaustive]` so #57 can add fields without breaking existing
/// [`DreamCycle`](crate::traits::DreamCycle) implementations — external code constructs
/// it via [`IdentityOutput::empty`] / [`Default`], not a struct literal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IdentityOutput {
    /// Stable, rarely-changing self-facts. Empty until #57.
    pub anchors: Vec<FactId>,
    /// Active working-set identity entries. Empty until #57.
    pub core: Vec<FactId>,
    /// Behavioral predictions. Empty until #57.
    pub predictions: Vec<FactId>,
}

impl IdentityOutput {
    /// The empty identity emitted by the default DBSCAN cycle (#57 fills the layers).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// One atomic mutation proposed by a [`DreamCycle`](crate::traits::DreamCycle).
///
/// The engine validates the whole `Vec<CycleDelta>` then applies it in a single
/// transaction (all-or-nothing). Marked `#[non_exhaustive]`: #578 may add variants
/// (R9/R13), so downstream `match` expressions must carry a wildcard arm — but
/// existing variants stay constructible by external implementations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CycleDelta {
    /// Synthesize a brand-new fact (e.g. a derived pattern). Not a promotion.
    AddFact(NewFact),
    /// Adjust a fact's base `importance` by `adjustment` quanta (±2 symmetric,
    /// cumulative). See [`IMPORTANCE_STEP`]. The target fact must be active.
    AdjustScore { fact_id: FactId, adjustment: i16 },
    /// Remove a fact from retrieval immediately (binary), keeping the row for
    /// audit/mining. Implemented as transaction-time soft-deletion (`t_expired`)
    /// plus a `{"quarantine":{…}}` metadata marker — NOT valid-time invalidation.
    Quarantine { fact_id: FactId, reason: String },
    /// Promote a fact to wisdom with provenance (shares the engine's promotion path).
    Promote {
        fact_id: FactId,
        provenance: PromotionProvenance,
    },
    /// Record an outcome signal for a fact (an `OutcomeSignal` event).
    TagOutcome { fact_id: FactId, outcome: Outcome },
    /// Mark `old_id` superseded by `new_id`: expire `old_id` + create a
    /// `"supersedes"` graph edge `new_id → old_id`. **Both facts must already exist**
    /// (the applier resolves them by id). A forward reference to a fact added by an
    /// [`CycleDelta::AddFact`] in the same report is not supported — its id is not
    /// knowable until insert.
    Supersede { old_id: FactId, new_id: FactId },
    /// Atomically merge `sources` into a freshly-synthesized `new_fact`.
    ///
    /// The merge primitive for an LLM consolidation backend (#554): inserts
    /// `new_fact`, then for **each** source expires it and creates a `"supersedes"`
    /// graph edge `new_fact_id → source`, plus a single `lineage` record mapping the
    /// synthetic to all its sources — all in the one apply transaction.
    ///
    /// This exists because the merge cannot be expressed as `AddFact + Supersede` in
    /// one report: `Supersede`'s `new_id` must already exist, but the synthetic's id
    /// is not knowable until its insert runs. It is **not** `AddFact + Quarantine`
    /// either — `Quarantine` creates no supersession edge and no lineage, orphaning the
    /// summary from the sources it replaces (the knowledge-base contradiction-resolution
    /// depends on those edges).
    ///
    /// `sources` must be non-empty; every source must exist and be active at apply
    /// time. The synthetic's id is reported in [`ApplyResult::synthesized_fact_ids`]
    /// and is dream-marked in the same transaction (invariant M).
    Synthesize {
        sources: Vec<FactId>,
        new_fact: NewFact,
    },
}

/// Bookkeeping for one cycle. The `CycleMetadata` (not the full delta log) is what
/// the engine persists into config so the next cycle's `prior_reports` can see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleMetadata {
    /// Cycle sequence number, assigned by the producer (the default impl derives it
    /// as `max(prior_reports.cycle_id) + 1`). Monotonic for sequentially-applied
    /// cycles; two reports produced from the same context before either is applied
    /// may share an id (the dream-cycle marker still makes the second apply a near
    /// no-op).
    pub cycle_id: u64,
    /// When the cycle ran.
    pub ran_at: DateTime<Utc>,
    /// The window of facts this cycle considered.
    pub time_window: TimeWindow,
    /// Number of facts selected into the cycle's working set.
    pub facts_selected: usize,
    /// Identifier of the producing implementation (e.g. `"dbscan-v1"`).
    pub method_version: String,
    /// Facts the cycle processed; the engine stamps each with the dream-cycled
    /// marker at apply time so a re-run excludes them (idempotency).
    pub processed_ids: Vec<FactId>,
}

/// Report returned by [`DreamCycle::run`](crate::traits::DreamCycle::run): an ordered
/// delta log + identity output + metadata.
///
/// A plain constructible struct (intentionally **not** `#[non_exhaustive]`): external
/// `DreamCycle` implementations must build it, which `#[non_exhaustive]` would forbid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleReport {
    pub deltas: Vec<CycleDelta>,
    pub identity: IdentityOutput,
    pub metadata: CycleMetadata,
}

/// Why [`MemoryEngine::run_dream_cycle_guarded`](crate::MemoryEngine::run_dream_cycle_guarded)
/// declined to run a cycle.
///
/// `#[non_exhaustive]`: new skip *reasons* (e.g. a future distributed-lock contention
/// from #207) are additive and must not break exhaustive matches on the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SkipReason {
    /// The caller wrote facts since the last cycle decision (#209): a fact with
    /// `id > since_fact_id` exists. The cursor was advanced to `new_max_fact_id`;
    /// the next quiet invocation (no newer writes) will run and clear the backlog.
    CallerWroteFacts {
        since_fact_id: i64,
        new_max_fact_id: i64,
    },
}

/// Outcome of [`MemoryEngine::run_dream_cycle_guarded`](crate::MemoryEngine::run_dream_cycle_guarded):
/// the cycle either **ran** (producing a [`CycleReport`]) or was **skipped** (deferred).
///
/// Deliberately **not** `#[non_exhaustive]` so consumers can match `Ran`/`Skipped`
/// exhaustively. ⚠️ Adding a third variant (e.g. a lock-contention skip, #207) is a
/// **semver-breaking** change — accepted as the cost of exhaustive matching. Encode new
/// *reasons* on [`SkipReason`] (which is `#[non_exhaustive]`) instead, not new variants here.
#[derive(Debug, Clone, PartialEq)]
pub enum CycleOutcome {
    /// The cycle ran; the (still unapplied) report is attached.
    Ran(CycleReport),
    /// The cycle was skipped this invocation; see [`SkipReason`].
    Skipped(SkipReason),
}

/// Per-variant tally of what an applied report changed.
///
/// Returned by [`MemoryEngine::apply_cycle_report`](crate::MemoryEngine::apply_cycle_report);
/// the old counts-based report is derivable from this for any future observability
/// layer (e.g. an MCP wrapper). Engine-constructed only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ApplyResult {
    pub facts_added: usize,
    pub scores_adjusted: usize,
    pub quarantined: usize,
    pub promoted: usize,
    pub outcomes_tagged: usize,
    pub superseded: usize,
    /// Number of [`CycleDelta::Synthesize`] merges applied.
    pub synthesized: usize,
    /// Ids of facts created by [`CycleDelta::AddFact`], in delta order.
    pub new_fact_ids: Vec<FactId>,
    /// Ids of the synthetic merge facts created by [`CycleDelta::Synthesize`], in
    /// delta order. Each is dream-marked in the apply transaction (invariant M, #209)
    /// — like [`Self::new_fact_ids`] and [`Self::promoted_fact_ids`] — so the cycle's
    /// own merge output never looks like a fresh caller write to the #209 cursor.
    pub synthesized_fact_ids: Vec<FactId>,
    /// Ids of the pinned wisdom facts created by [`CycleDelta::Promote`], in delta order.
    ///
    /// Captured so the apply pass can dream-mark the cycle's *own* promoted outputs
    /// (invariant M, #209): every fact a cycle creates must carry the `dream_cycle`
    /// marker, else it re-enters the next cycle's input and the #209 caller-write
    /// cursor trips on the cycle's own work.
    pub promoted_fact_ids: Vec<FactId>,
}

/// Reserved for future soft-fail per-delta reporting (empty in v1; the v1 applier
/// is hard-fail — the first invalid delta aborts the whole report).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CycleAnomaly {}

#[cfg(test)]
mod tests {
    use super::*;

    fn tw() -> TimeWindow {
        let t = DateTime::parse_from_rfc3339("2026-06-16T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        TimeWindow {
            start: t,
            end: t + chrono::Duration::hours(1),
        }
    }

    // R-A spike: the i16 quantum count maps to a base-importance delta and clamps.
    #[test]
    fn adjust_score_quantum_arithmetic_and_clamp() {
        let apply = |start: f64, adj: i16| -> f64 {
            f64::from(adj)
                .mul_add(IMPORTANCE_STEP, start)
                .clamp(0.0, 1.0)
        };
        // +2 thrice from 0.5 → 0.5 + 6*0.05 = 0.8
        let mut s = 0.5;
        for _ in 0..3 {
            s = apply(s, 2);
        }
        assert!((s - 0.8).abs() < 1e-9, "got {s}");
        // symmetric −2 thrice → back to 0.5
        for _ in 0..3 {
            s = apply(s, -2);
        }
        assert!((s - 0.5).abs() < 1e-9, "got {s}");
        // clamp at the ceiling and floor
        assert!((apply(0.95, 2) - 1.0).abs() < 1e-9);
        assert!((apply(0.05, -2) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn cycle_report_serde_round_trip_all_delta_variants() {
        let prov = PromotionProvenance {
            source_count: 2,
            session_count: 1,
            date_range_start: tw().start,
            date_range_end: tw().end,
            confidence: 0.9,
            method_version: "dbscan-v1".into(),
            representative_ids: vec![1, 2],
        };
        let report = CycleReport {
            deltas: vec![
                CycleDelta::AddFact(NewFact {
                    content: "p".into(),
                    content_hash: String::new(),
                    embedding: vec![0.1, 0.2],
                    fact_type: crate::types::FactType::Semantic,
                    t_created: tw().start,
                    t_expired: None,
                    t_valid: None,
                    t_invalid: None,
                    source_event_id: None,
                    importance: 0.5,
                    access_count: 0,
                    last_accessed: tw().start,
                    metadata: serde_json::json!({}),
                    scope_id: 1,
                    is_pinned: false,
                }),
                CycleDelta::AdjustScore {
                    fact_id: 1,
                    adjustment: -2,
                },
                CycleDelta::Quarantine {
                    fact_id: 2,
                    reason: "explicit correction".into(),
                },
                CycleDelta::Promote {
                    fact_id: 3,
                    provenance: prov,
                },
                CycleDelta::TagOutcome {
                    fact_id: 4,
                    outcome: Outcome::Negative,
                },
                CycleDelta::Supersede {
                    old_id: 5,
                    new_id: 6,
                },
                CycleDelta::Synthesize {
                    sources: vec![7, 8],
                    new_fact: NewFact {
                        content: "merged".into(),
                        content_hash: String::new(),
                        embedding: vec![0.3, 0.4],
                        fact_type: crate::types::FactType::Semantic,
                        t_created: tw().start,
                        t_expired: None,
                        t_valid: None,
                        t_invalid: None,
                        source_event_id: None,
                        importance: 0.5,
                        access_count: 0,
                        last_accessed: tw().start,
                        metadata: serde_json::json!({}),
                        scope_id: 1,
                        is_pinned: false,
                    },
                },
            ],
            identity: IdentityOutput::empty(),
            metadata: CycleMetadata {
                cycle_id: 1,
                ran_at: tw().start,
                time_window: tw(),
                facts_selected: 6,
                method_version: "dbscan-v1".into(),
                processed_ids: vec![1, 2, 3, 4, 5],
            },
        };

        let json = serde_json::to_string(&report).unwrap();
        let back: CycleReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back, "wire format must round-trip");
        assert!(report.identity.anchors.is_empty());
    }
}
