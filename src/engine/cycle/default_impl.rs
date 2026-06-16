//! The shipped default [`DreamCycle`] implementation.
//!
//! [`DefaultDreamCycle`] is **pure** and **deterministic**: it reads the cycle's
//! window and emits a [`CycleReport`] of proposed deltas, never writing to the store
//! (the engine applies the report). Because it makes no LLM call and uses a stable
//! iteration order, it is itself immune to context collapse — the delta vocabulary's
//! collapse-resistance (R7) is for *consumer* implementations.
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
//! Consolidation (the existing 3-pass `consolidate()`) is intentionally **not** run
//! here: it mutates the store, which would break this producer's purity and
//! determinism. Operators schedule `MemoryEngine::consolidate` as a separate step.
//! Identity computation (ANCHORS/CORE/PREDICTIONS) is #57; this emits
//! [`IdentityOutput::empty`]. Abstract pattern extraction / synthesized `AddFact`
//! (R9), hierarchical composition (R13), and content-based correction detection are
//! deferred to #578.

use chrono::Utc;
use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::traits::DreamCycle;
use crate::types::{DreamCycleConfig, Fact, FactId, FactType, PromotionProvenance};

use super::context::CycleContext;
use super::dbscan::dbscan;
use super::report::{CycleDelta, CycleMetadata, CycleReport, IdentityOutput};

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
#[derive(Debug, Clone)]
pub struct DefaultDreamCycle {
    config: DreamCycleConfig,
}

impl DefaultDreamCycle {
    /// Construct with an explicit [`DreamCycleConfig`].
    #[must_use]
    pub fn new(config: DreamCycleConfig) -> Self {
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
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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
    PromotionProvenance {
        source_count,
        session_count: 1, // best-effort in v1
        date_range_start: start,
        date_range_end: end,
        confidence: 1.0 - 1.0 / (cluster.len() as f64),
        method_version: METHOD_VERSION.to_owned(),
        representative_ids,
        lineage_id: 0,
    }
}

impl DreamCycle for DefaultDreamCycle {
    fn run(&self, ctx: &CycleContext) -> Result<CycleReport> {
        let window = ctx.time_window();
        let facts = ctx.dream().list_undreamt_in_period(window)?;
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
            let importances: Vec<f64> = bucket.iter().map(|f| f.importance).collect();
            let p75 = percentile(&importances, self.config.promotion_percentile);

            let points: Vec<(FactId, &[f32])> = bucket
                .iter()
                .map(|f| (f.id, f.embedding.as_slice()))
                .collect();
            for cluster in dbscan(&points, EPS, MIN_PTS) {
                // Representative = highest-importance member (medoid proxy).
                let Some(&medoid) = cluster.iter().max_by(|a, b| {
                    by_id[a]
                        .importance
                        .partial_cmp(&by_id[b].importance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }) else {
                    continue;
                };
                if by_id[&medoid].importance >= p75 {
                    deltas.push(CycleDelta::Promote {
                        fact_id: medoid,
                        provenance: cluster_provenance(&cluster, &by_id),
                    });
                    promoted.insert(medoid);
                }
            }
        }

        // 3. Outcome-driven rescoring + quarantine (skip facts already promoted).
        for fact in &facts {
            if promoted.contains(&fact.id) {
                continue;
            }
            let counts = ctx.dream().outcome_counts(fact.id)?;
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
    use crate::engine::MemoryEngine;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactOptions, AddFactRequest, FactType, Outcome};

    const DIM: usize = 4;

    struct FixedEmbed;
    impl EmbeddingProvider for FixedEmbed {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0, 0.0, 0.0, 0.0])
        }
    }

    fn add(engine: &MemoryEngine, content: &str, ft: FactType, importance: f64) -> i64 {
        let req = AddFactRequest {
            content: content.into(),
            fact_type: ft,
            source_event_id: None,
            scope: None,
            opts: Some(AddFactOptions {
                importance: Some(importance),
                ..Default::default()
            }),
        };
        engine.add_fact(&req, &FixedEmbed, None).unwrap()
    }

    #[test]
    fn dense_cluster_yields_a_promote_delta() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        // Three identical-embedding Semantic facts → one DBSCAN cluster of 3.
        for i in 0..3 {
            add(&engine, &format!("pattern {i}"), FactType::Semantic, 0.8);
        }
        let report = engine
            .run_dream_cycle(&DefaultDreamCycle::with_defaults())
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

    #[test]
    fn negative_outcomes_rescore_down() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let id = add(&engine, "lone fact", FactType::Episodic, 0.5);
        engine.record_outcome(id, Outcome::Negative).unwrap();
        engine.record_outcome(id, Outcome::Negative).unwrap();
        let report = engine
            .run_dream_cycle(&DefaultDreamCycle::with_defaults())
            .unwrap();
        // single fact → no cluster (min_pts=3) → only the rescore delta.
        assert!(report.deltas.iter().any(|d| matches!(
            d,
            CycleDelta::AdjustScore { fact_id, adjustment: -2 } if *fact_id == id
        )));
    }

    #[test]
    fn consistent_negative_outcomes_quarantine() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let id = add(&engine, "bad fact", FactType::Episodic, 0.5);
        for _ in 0..3 {
            engine.record_outcome(id, Outcome::Negative).unwrap();
        }
        let report = engine
            .run_dream_cycle(&DefaultDreamCycle::with_defaults())
            .unwrap();
        assert!(report.deltas.iter().any(|d| matches!(
            d,
            CycleDelta::Quarantine { fact_id, .. } if *fact_id == id
        )));
    }

    #[test]
    fn run_is_deterministic_on_deltas() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        for i in 0..3 {
            add(&engine, &format!("p {i}"), FactType::Semantic, 0.8);
        }
        let a = engine
            .run_dream_cycle(&DefaultDreamCycle::with_defaults())
            .unwrap();
        let b = engine
            .run_dream_cycle(&DefaultDreamCycle::with_defaults())
            .unwrap();
        assert_eq!(
            a.deltas, b.deltas,
            "deltas must be deterministic across runs"
        );
    }
}
