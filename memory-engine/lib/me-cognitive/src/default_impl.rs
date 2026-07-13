//! The shipped default [`DreamCycle`] implementation.
//!
//! [`DefaultDreamCycle`] is **pure** and **deterministic**: it reads the cycle's
//! window and emits a [`CycleReport`] of proposed deltas, never writing to the store
//! (the caller applies the report via [`apply_cycle_report`](crate::apply_cycle_report)).
//! Because it makes no LLM call and uses a stable iteration order, it is itself
//! immune to context collapse — the delta vocabulary's collapse-resistance (R7) is
//! for *consumer* implementations.
//!
//! Pipeline:
//! 1. **Select** un-dream-cycled facts in the window.
//! 2. **Cluster** per [`FactType`] with DBSCAN over embeddings; a cluster whose
//!    representative (highest-importance medoid) clears the per-type P75 importance
//!    threshold yields a [`CycleDelta::Promote`].
//! 3. **Rescore** from outcome history: net `positive − negative` (clamped to ±2)
//!    becomes a [`CycleDelta::AdjustScore`]; a fact with a strong, consistently
//!    negative signal is [`CycleDelta::Quarantine`]d out of retrieval.
//!
//! Consolidation (the engine's 3-pass `consolidate()`) is intentionally **not** run
//! here: it mutates the store, which would break this producer's purity and
//! determinism. Operators schedule consolidation as a separate step.
//! Identity computation (ANCHORS/CORE/PREDICTIONS) is #57; this emits
//! [`IdentityOutput::empty`]. Abstract pattern extraction / synthesized `AddFact`
//! (R9), hierarchical composition (R13), and content-based correction detection are
//! deferred to #578.

use chrono::Utc;
use std::collections::{HashMap, HashSet};

use me_traits::{CycleCtx, DreamCycle};
use me_types::error::Result;
use me_types::types::cycle_report::{CycleDelta, CycleMetadata, CycleReport, IdentityOutput};
use me_types::types::{DreamCycleConfig, Fact, FactId, FactType, PromotionProvenance};

use crate::dbscan::dbscan;

/// DBSCAN neighbourhood radius in cosine *distance* (cos similarity ≥ 0.85).
/// The per-`FactType` `DreamCycleConfig` ratios are retention ratios, NOT distances,
/// so a fixed `eps` is used here; parametric per-type `eps` is deferred to #578.
const EPS: f32 = 0.15;
/// Minimum points (including the core point) for a DBSCAN cluster.
const MIN_PTS: usize = 3;
/// Negative-outcome count at which a fact (with no positives) is quarantined.
const QUARANTINE_NEGATIVE_THRESHOLD: u32 = 3;
/// Identifier stamped into each report's `method_version`.
const METHOD_VERSION: &str = "dbscan-v1";

/// The shipped pure-Rust DBSCAN dream cycle.
///
/// A full, runnable end-to-end example (add facts → run this cycle → apply the
/// report) needs the `MemoryEngine` facade — the `MemoryEngine: DreamCtx` implementor
/// — which this L3 crate cannot depend on (that back-edge is exactly what the Wave 2
/// #816 / S5 carve removes). See `memory_engine::MemoryEngine::run_dream_cycle`'s own
/// doc (in the facade crate) for the runnable version of this sketch.
///
/// ```ignore
/// let engine = MemoryEngine::builder(2).build()?;
/// // ... add facts via `engine.add_fact(...)` ...
/// let report = engine.run_dream_cycle(&DefaultDreamCycle::with_defaults()).await?;
/// let applied = engine.apply_cycle_report(&report).await?;
/// ```
#[derive(Debug, Clone)]
pub struct DefaultDreamCycle {
    config: DreamCycleConfig,
}

impl DefaultDreamCycle {
    /// Construct with an explicit [`DreamCycleConfig`].
    #[must_use]
    pub const fn new(config: DreamCycleConfig) -> Self {
        Self { config }
    }

    /// Construct with the default configuration (Episodic=0.2 / Semantic=0.8 /
    /// Procedural=0.8 retention, P75 promotion percentile).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DreamCycleConfig::default())
    }
}

impl Default for DefaultDreamCycle {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Nearest-rank percentile of a slice (ascending). Empty → 0.0.
fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    // Defensive clamp: a consumer-supplied `DreamCycleConfig.promotion_percentile`
    // is not guaranteed validated, and an out-of-range value would index wrongly.
    let p = p.clamp(0.0, 1.0);
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let idx = ((p * (v.len() - 1) as f64).round() as usize).min(v.len() - 1);
    v[idx]
}

/// Build a promotion provenance envelope describing a cluster.
fn cluster_provenance(cluster: &[FactId], by_id: &HashMap<FactId, &Fact>) -> PromotionProvenance {
    let members: Vec<&Fact> = cluster
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect();
    let start = members
        .iter()
        .map(|f| f.t_created)
        .min()
        .unwrap_or_else(Utc::now);
    let end = members
        .iter()
        .map(|f| f.t_created)
        .max()
        .unwrap_or_else(Utc::now);
    let mut representative_ids = cluster.to_vec();
    representative_ids.truncate(5);
    #[allow(clippy::cast_possible_truncation)]
    let source_count = cluster.len() as u32;
    #[allow(clippy::cast_precision_loss)]
    let confidence = 1.0 - 1.0 / (cluster.len() as f64);
    PromotionProvenance {
        source_count,
        session_count: 1, // best-effort in v1
        date_range_start: start,
        date_range_end: end,
        confidence,
        method_version: METHOD_VERSION.to_owned(),
        representative_ids,
    }
}

// NOTE — the recursion trap (see crate-root doc): unlike `impl DreamCtx for
// MemoryEngine` (facade), this impl calls `ctx.*` where `ctx: &dyn CycleCtx` is a
// PARAMETER, not `self`, so there is no same-name inherent-vs-trait ambiguity here at
// all. Nothing below needs (or would benefit from) fully-qualified syntax.
#[async_trait::async_trait]
impl DreamCycle for DefaultDreamCycle {
    async fn run(&self, ctx: &dyn CycleCtx) -> Result<CycleReport> {
        let window = ctx.time_window();
        let facts = ctx.list_undreamt_in_period(window).await?;
        let processed_ids: Vec<FactId> = facts.iter().map(|f| f.id).collect();
        let by_id: HashMap<FactId, &Fact> = facts.iter().map(|f| (f.id, f)).collect();

        let mut deltas: Vec<CycleDelta> = Vec::new();
        let mut promoted: HashSet<FactId> = HashSet::new();

        // 2. Per-FactType DBSCAN → promotion candidates.
        for fact_type in [FactType::Episodic, FactType::Semantic, FactType::Procedural] {
            let bucket: Vec<&Fact> = facts.iter().filter(|f| f.fact_type == fact_type).collect();
            if bucket.len() < MIN_PTS {
                continue;
            }
            let importances: Vec<f64> = bucket.iter().map(|f| f.base_importance).collect();
            let p75 = percentile(&importances, self.config.promotion_percentile);

            let points: Vec<(FactId, &[f32])> = bucket
                .iter()
                .map(|f| (f.id, f.embedding.as_slice()))
                .collect();
            for cluster in dbscan(&points, EPS, MIN_PTS) {
                // Representative = highest-importance member (medoid proxy), ties broken
                // by smallest fact id so the choice is deterministic regardless of the
                // input's tie ordering.
                let Some(&medoid) = cluster.iter().max_by(|a, b| {
                    by_id[a]
                        .base_importance
                        .partial_cmp(&by_id[b].base_importance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.cmp(a)) // smaller id wins the tie
                }) else {
                    continue;
                };
                if by_id[&medoid].base_importance >= p75 {
                    deltas.push(CycleDelta::Promote {
                        fact_id: medoid,
                        provenance: cluster_provenance(&cluster, &by_id),
                    });
                    promoted.insert(medoid);
                }
            }
        }

        // 3. Outcome-driven rescoring + quarantine (skip facts already promoted).
        // Batch-fetch outcome counts in one query (not N+1); a fact with no recorded
        // outcomes is absent from the map → default (zeros). Already-promoted facts are
        // skipped in the loop below, so omit them from the query too.
        let outcome_ids: Vec<FactId> = facts
            .iter()
            .map(|f| f.id)
            .filter(|id| !promoted.contains(id))
            .collect();
        let outcome_counts = ctx.outcome_counts_batch(&outcome_ids).await?;
        for fact in &facts {
            if promoted.contains(&fact.id) {
                continue;
            }
            let counts = outcome_counts.get(&fact.id).copied().unwrap_or_default();
            if counts.positive == 0 && counts.negative >= QUARANTINE_NEGATIVE_THRESHOLD {
                deltas.push(CycleDelta::Quarantine {
                    fact_id: fact.id,
                    reason: "consistent negative outcomes".to_owned(),
                });
                continue;
            }
            let net = i64::from(counts.positive) - i64::from(counts.negative);
            if net != 0 {
                #[allow(clippy::cast_possible_truncation)]
                let adjustment = net.clamp(-2, 2) as i16;
                deltas.push(CycleDelta::AdjustScore {
                    fact_id: fact.id,
                    adjustment,
                });
            }
        }

        let cycle_id = ctx
            .prior_reports()
            .iter()
            .map(|m| m.cycle_id)
            .max()
            .map_or(0, |m| m + 1);

        Ok(CycleReport {
            deltas,
            identity: IdentityOutput::empty(),
            metadata: CycleMetadata {
                cycle_id,
                ran_at: Utc::now(),
                time_window: window,
                facts_selected: facts.len(),
                method_version: METHOD_VERSION.to_owned(),
                processed_ids,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognitive::run_dream_cycle;
    use crate::test_support::TestEngine;
    use me_types::types::Outcome;

    const DIM: usize = 4;

    #[tokio::test]
    async fn dense_cluster_yields_a_promote_delta() {
        let engine = TestEngine::new(DIM).await;
        // Three identical-embedding Semantic facts → one DBSCAN cluster of 3.
        for i in 0..3 {
            engine
                .add_typed(&format!("pattern {i}"), FactType::Semantic, 0.8)
                .await;
        }
        let report = run_dream_cycle(engine.ctx(), &engine, &DefaultDreamCycle::with_defaults())
            .await
            .unwrap();
        let promotes = report
            .deltas
            .iter()
            .filter(|d| matches!(d, CycleDelta::Promote { .. }))
            .count();
        assert_eq!(
            promotes, 1,
            "expected one promotion, deltas: {:?}",
            report.deltas
        );
        assert_eq!(report.metadata.facts_selected, 3);
    }

    #[tokio::test]
    async fn negative_outcomes_rescore_down() {
        let engine = TestEngine::new(DIM).await;
        let id = engine.add_typed("lone fact", FactType::Episodic, 0.5).await;
        engine.record_outcome(id, Outcome::Negative).await;
        engine.record_outcome(id, Outcome::Negative).await;
        let report = run_dream_cycle(engine.ctx(), &engine, &DefaultDreamCycle::with_defaults())
            .await
            .unwrap();
        // single fact → no cluster (min_pts=3) → only the rescore delta.
        assert!(report.deltas.iter().any(|d| matches!(
            d,
            CycleDelta::AdjustScore { fact_id, adjustment: -2 } if *fact_id == id
        )));
    }

    #[tokio::test]
    async fn consistent_negative_outcomes_quarantine() {
        let engine = TestEngine::new(DIM).await;
        let id = engine.add_typed("bad fact", FactType::Episodic, 0.5).await;
        for _ in 0..3 {
            engine.record_outcome(id, Outcome::Negative).await;
        }
        let report = run_dream_cycle(engine.ctx(), &engine, &DefaultDreamCycle::with_defaults())
            .await
            .unwrap();
        assert!(report.deltas.iter().any(|d| matches!(
            d,
            CycleDelta::Quarantine { fact_id, .. } if *fact_id == id
        )));
    }

    #[tokio::test]
    async fn run_is_deterministic_on_deltas() {
        let engine = TestEngine::new(DIM).await;
        for i in 0..3 {
            engine
                .add_typed(&format!("p {i}"), FactType::Semantic, 0.8)
                .await;
        }
        let a = run_dream_cycle(engine.ctx(), &engine, &DefaultDreamCycle::with_defaults())
            .await
            .unwrap();
        let b = run_dream_cycle(engine.ctx(), &engine, &DefaultDreamCycle::with_defaults())
            .await
            .unwrap();
        assert_eq!(
            a.deltas, b.deltas,
            "deltas must be deterministic across runs"
        );
    }
}
