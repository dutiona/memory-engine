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

/// Policy for forgetting/pruning stale facts (Phase 2).
#[derive(Debug, Clone)]
pub struct ForgetPolicy {
    pub min_importance: f32,
}
