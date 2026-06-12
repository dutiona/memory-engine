//! Bootstrap configuration, reporting, and pre-warming metrics.

use serde::Serialize;

/// Configuration for the bootstrap pipeline.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    /// Scope path to ingest facts into (e.g., `"project:memory-engine"`).
    pub scope: Option<String>,
    /// Maximum turns to process per session. `0` = no limit.
    pub max_turns: usize,
    /// Skip sessions already bootstrapped (idempotency via `session_id` check).
    pub skip_existing: bool,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            scope: None,
            max_turns: 0,
            skip_existing: true,
        }
    }
}

/// Report returned by the bootstrap pipeline.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BootstrapReport {
    pub sessions_processed: usize,
    pub sessions_skipped: usize,
    pub entries_parsed: usize,
    pub entries_malformed: usize,
    pub turns_reconstructed: usize,
    pub candidates_found: usize,
    pub facts_created: usize,
    pub events_ingested: usize,
    pub outcome_counts: OutcomeCounts,
    pub category_counts: CategoryCounts,
    pub prewarm_metrics: PrewarmMetrics,
}

impl BootstrapReport {
    /// Merge another report into this one (for directory-level aggregation).
    pub fn merge(&mut self, other: &Self) {
        self.sessions_processed += other.sessions_processed;
        self.sessions_skipped += other.sessions_skipped;
        self.entries_parsed += other.entries_parsed;
        self.entries_malformed += other.entries_malformed;
        self.turns_reconstructed += other.turns_reconstructed;
        self.candidates_found += other.candidates_found;
        self.facts_created += other.facts_created;
        self.events_ingested += other.events_ingested;
        self.outcome_counts.success += other.outcome_counts.success;
        self.outcome_counts.failure += other.outcome_counts.failure;
        self.outcome_counts.indeterminate += other.outcome_counts.indeterminate;
        self.category_counts.bug += other.category_counts.bug;
        self.category_counts.decision += other.category_counts.decision;
        self.category_counts.convention += other.category_counts.convention;
        self.category_counts.learning += other.category_counts.learning;
        // Recompute average importance from merged prewarm metrics
        let total_old = self.prewarm_metrics.total_count();
        let total_new = other.prewarm_metrics.total_count();
        let total = total_old + total_new;
        if total > 0 {
            // Counts are small (episode tallies); convert losslessly via u32 so
            // clippy's cast_precision_loss does not fire (f64::from is lossless).
            let w_new = f64::from(u32::try_from(total_new).unwrap_or(u32::MAX));
            let w_old = f64::from(u32::try_from(total_old).unwrap_or(u32::MAX));
            let denom = f64::from(u32::try_from(total).unwrap_or(u32::MAX));
            self.prewarm_metrics.avg_importance = other
                .prewarm_metrics
                .avg_importance
                .mul_add(w_new, self.prewarm_metrics.avg_importance * w_old)
                / denom;
        }
        self.prewarm_metrics.episodic_count += other.prewarm_metrics.episodic_count;
        self.prewarm_metrics.semantic_count += other.prewarm_metrics.semantic_count;
        self.prewarm_metrics.procedural_count += other.prewarm_metrics.procedural_count;
    }
}

/// Outcome classification counts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct OutcomeCounts {
    pub success: usize,
    pub failure: usize,
    pub indeterminate: usize,
}

/// Episode category counts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CategoryCounts {
    pub bug: usize,
    pub decision: usize,
    pub convention: usize,
    pub learning: usize,
}

/// Pre-warming quality metrics (R3, APC).
#[derive(Debug, Clone, Default, Serialize)]
pub struct PrewarmMetrics {
    pub episodic_count: usize,
    pub semantic_count: usize,
    pub procedural_count: usize,
    pub avg_importance: f64,
}

impl PrewarmMetrics {
    /// Total facts across all types.
    #[must_use]
    pub const fn total_count(&self) -> usize {
        self.episodic_count + self.semantic_count + self.procedural_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_default_zeros() {
        let r = BootstrapReport::default();
        assert_eq!(r.sessions_processed, 0);
        assert_eq!(r.facts_created, 0);
        assert_eq!(r.prewarm_metrics.total_count(), 0);
    }

    #[test]
    fn report_merge() {
        let mut a = BootstrapReport {
            sessions_processed: 1,
            facts_created: 3,
            prewarm_metrics: PrewarmMetrics {
                episodic_count: 1,
                semantic_count: 1,
                procedural_count: 1,
                avg_importance: 0.6,
            },
            ..Default::default()
        };
        let b = BootstrapReport {
            sessions_processed: 2,
            facts_created: 5,
            prewarm_metrics: PrewarmMetrics {
                episodic_count: 2,
                semantic_count: 2,
                procedural_count: 1,
                avg_importance: 0.8,
            },
            ..Default::default()
        };
        a.merge(&b);
        assert_eq!(a.sessions_processed, 3);
        assert_eq!(a.facts_created, 8);
        assert_eq!(a.prewarm_metrics.total_count(), 8);
        // Weighted average: (0.6*3 + 0.8*5) / 8 = 5.8/8 = 0.725
        assert!((a.prewarm_metrics.avg_importance - 0.725).abs() < 0.001);
    }

    #[test]
    fn snapshot_default_report() {
        let report = BootstrapReport::default();
        insta::assert_yaml_snapshot!(report);
    }

    #[test]
    fn snapshot_populated_report() {
        let report = BootstrapReport {
            sessions_processed: 3,
            sessions_skipped: 1,
            entries_parsed: 42,
            entries_malformed: 2,
            turns_reconstructed: 18,
            candidates_found: 12,
            facts_created: 8,
            events_ingested: 15,
            outcome_counts: OutcomeCounts {
                success: 5,
                failure: 2,
                indeterminate: 1,
            },
            category_counts: CategoryCounts {
                bug: 3,
                decision: 2,
                convention: 1,
                learning: 2,
            },
            prewarm_metrics: PrewarmMetrics {
                episodic_count: 3,
                semantic_count: 3,
                procedural_count: 2,
                avg_importance: 0.72,
            },
        };
        insta::assert_yaml_snapshot!(report);
    }
}
