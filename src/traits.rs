use crate::error::Result;
use crate::types::Fact;

// --- Phase 1: Embedding provider (fully used) ---

/// Trait for computing text embeddings.
///
/// Consumers implement this to integrate their embedding model (local or API).
/// The engine calls `embed` during `add_fact` to compute the embedding vector.
pub trait EmbeddingProvider {
    /// Compute an embedding vector for the given text.
    ///
    /// # Errors
    ///
    /// Returns an error if embedding computation fails.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

// --- Phase 2 placeholder traits and types ---

/// Trait for generating summaries from fact clusters (Phase 2).
///
/// Used by consolidation to merge related facts into higher-level summaries.
pub trait SummaryGenerator {
    /// Generate a textual summary from a slice of facts.
    ///
    /// # Errors
    ///
    /// Returns an error if summarization fails.
    fn summarize(&self, facts: &[Fact]) -> Result<String>;

    /// Compute an embedding for the given text.
    ///
    /// # Errors
    ///
    /// Returns an error if embedding computation fails.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// Trait for arbitrating conflicts between contradicting facts (Phase 2).
pub trait ConflictArbiter {
    /// Decide how to resolve a conflict between an existing and a new fact.
    ///
    /// # Errors
    ///
    /// Returns an error if arbitration fails.
    fn arbitrate(&self, old_fact: &Fact, new_fact: &Fact) -> Result<CrudDecision>;
}

/// Decision for conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrudDecision {
    Add,
    Update,
    Delete,
    Noop,
}

/// Configuration for the consolidation process (Phase 2).
#[derive(Debug, Clone)]
pub struct ConsolidationConfig {
    pub dedup_threshold: f32,
    pub min_cluster_size: usize,
}

/// Statistics returned by consolidation (Phase 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationStats {
    pub duplicates_removed: usize,
    pub clusters_created: usize,
    pub global_summaries: usize,
}

/// Statistics returned by the forget/prune operation (Phase 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneStats {
    pub facts_expired: usize,
    pub facts_evaluated: usize,
}

/// Result of a conflict resolution (Phase 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictResolution {
    pub decision: CrudDecision,
    pub old_fact_id: i64,
    pub new_fact_id: Option<i64>,
}

/// Policy for forgetting/pruning stale facts.
///
/// Importance is computed as a weighted sum of 4 signals:
///
/// `recency × decay + frequency × log(access+1) + degree × log(edges+1) + base × importance`
///
/// Facts with computed importance below `min_importance` get soft-deleted (`t_expired` set).
#[derive(Debug, Clone)]
pub struct ForgetPolicy {
    /// Base Ebbinghaus half-life in days (default: 69.0).
    pub half_life_days: f64,
    /// Per-`FactType` half-life overrides. E.g., Episodic=30, Procedural=365.
    pub half_life_overrides: std::collections::HashMap<crate::types::FactType, f64>,
    /// Threshold below which facts are expired (default: 0.1).
    pub min_importance: f64,
    /// Weight for recency signal (Ebbinghaus decay). Default: 0.3.
    pub recency_weight: f64,
    /// Weight for access frequency signal. Default: 0.2.
    pub frequency_weight: f64,
    /// Weight for graph connectivity signal. Default: 0.3.
    pub graph_degree_weight: f64,
    /// Weight for base importance (`fact.importance`). Default: 0.2.
    pub base_importance_weight: f64,
}

impl Default for ForgetPolicy {
    fn default() -> Self {
        Self {
            half_life_days: 69.0,
            half_life_overrides: std::collections::HashMap::new(),
            min_importance: 0.1,
            recency_weight: 0.3,
            frequency_weight: 0.2,
            graph_degree_weight: 0.3,
            base_importance_weight: 0.2,
        }
    }
}

impl ForgetPolicy {
    /// Validate policy parameters.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if any parameter is out of range.
    pub fn validate(&self) -> Result<()> {
        use crate::error::MemoryError;

        if self.half_life_days <= 0.0 {
            return Err(MemoryError::Conflict("half_life_days must be > 0".into()));
        }
        for (ft, &hl) in &self.half_life_overrides {
            if hl <= 0.0 {
                return Err(MemoryError::Conflict(format!(
                    "half_life for {ft:?} must be > 0"
                )));
            }
        }
        if !(0.0..=1.0).contains(&self.min_importance) {
            return Err(MemoryError::Conflict(
                "min_importance must be in [0, 1]".into(),
            ));
        }
        if self.recency_weight < 0.0
            || self.frequency_weight < 0.0
            || self.graph_degree_weight < 0.0
            || self.base_importance_weight < 0.0
        {
            return Err(MemoryError::Conflict("weights must be >= 0".into()));
        }
        Ok(())
    }
}
