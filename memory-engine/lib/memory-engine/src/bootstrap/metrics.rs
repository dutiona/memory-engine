//! Bootstrap configuration, reporting, and pre-warming metrics.

use serde::Serialize;

/// Configuration for the bootstrap pipeline.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    /// Scope path to ingest facts into (e.g., `"project:memory-engine"`).
    /// `None` falls back to the root scope.
    pub scope: Option<String>,
    /// Maximum turns to process per session. `0` = no limit.
    ///
    /// When the session exceeds this limit the **most-recent** turns are kept
    /// (tail selection), because outcome evidence — commits, test results —
    /// concentrates at the end of sessions. Outcome classification always runs
    /// on the full turn list *before* truncation, so capping does not change the
    /// session outcome; it only bounds how many turns reach the keyword
    /// pre-filter and fact extraction.
    pub max_turns: usize,
    /// Skip sessions already bootstrapped (idempotency via `session_id` check).
    pub skip_existing: bool,
    /// Redact secrets/PII from every candidate's content **before** it is
    /// embedded or stored (#45/#51 gate). Default `true`; the CLI offers no
    /// flag to disable it in normal operation. The switch exists for library
    /// callers and for tests that assert on raw content. Redaction touches the
    /// ME copy only — the source `.jsonl`/`.md` backbone is never modified.
    pub redact: bool,
    /// Author-seeded known-secret literals (loaded from the gitignored denylist
    /// file, see [`crate::bootstrap::load_secret_denylist`]). Augments the
    /// signature detectors; empty = signatures-only. Ignored when `redact` is
    /// `false`.
    pub denylist: Vec<String>,
    /// Maximum total bytes read from a single session stream before parsing
    /// stops (#293). Bounds the per-stream I/O against a hostile or corrupt
    /// `.jsonl` of many in-bounds lines; the reader is wrapped in
    /// [`std::io::Read::take`]. `0` = no per-stream limit. Defaults to
    /// `DEFAULT_MAX_SESSION_BYTES` (256 MiB).
    pub max_session_bytes: u64,
    /// Maximum number of parsed entries retained from a single session before
    /// parsing stops with a truncation `warn` (#293). Bounds the in-memory
    /// entry `Vec` — and every downstream linear pass — against an entry-count
    /// flood. `0` = no entry-count limit. Defaults to
    /// `DEFAULT_MAX_ENTRIES` (1,000,000).
    pub max_entries: usize,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            scope: None,
            max_turns: 0,
            skip_existing: true,
            redact: true,
            denylist: Vec::new(),
            max_session_bytes: super::parse::DEFAULT_MAX_SESSION_BYTES,
            max_entries: super::parse::DEFAULT_MAX_ENTRIES,
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
    /// Existing facts reinforced (dedup-with-reinforcement) instead of duplicated.
    pub facts_reinforced: usize,
    /// Secret/PII findings scrubbed before any content reaches an extractor, the
    /// embedder, or storage (#51). Counts individual findings, not facts. The
    /// jsonl path scrubs whole turns upfront; the md path scrubs body + metadata.
    /// A redundant re-run reports `0`: the jsonl path skips already-bootstrapped
    /// sessions before redacting, and the md path counts only on fact creation.
    pub secrets_redacted: usize,
    /// Native `.md` memory files parsed (the `--memory-dir` path only).
    pub memory_files_parsed: usize,
    /// Native `.md` memory files skipped — unreadable or no parseable body
    /// (the `--memory-dir` path only).
    pub memory_files_skipped: usize,
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
        self.facts_reinforced += other.facts_reinforced;
        self.secrets_redacted += other.secrets_redacted;
        self.memory_files_parsed += other.memory_files_parsed;
        self.memory_files_skipped += other.memory_files_skipped;
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
            // Episode tallies are tiny (<< 2^52), so these usize -> f64 casts
            // cannot lose precision; the lint is a non-issue and a saturating
            // `try_from` would be less honest than the direct cast here.
            #[allow(clippy::cast_precision_loss)]
            {
                self.prewarm_metrics.avg_importance = other.prewarm_metrics.avg_importance.mul_add(
                    total_new as f64,
                    self.prewarm_metrics.avg_importance * total_old as f64,
                ) / total as f64;
            }
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
            facts_reinforced: 2,
            secrets_redacted: 3,
            memory_files_parsed: 0,
            memory_files_skipped: 0,
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
