use crate::error::Result;
use crate::search::hybrid::SearchResult;
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

    /// Compute embedding vectors for multiple texts in a single call.
    ///
    /// The default implementation loops `embed()` sequentially.
    /// Providers with native batch APIs (e.g., OpenAI `/v1/embeddings`)
    /// should override this for a single HTTP round-trip.
    ///
    /// # Contract
    ///
    /// The returned `Vec` **must** have the same length as `texts`.
    /// Each element corresponds positionally to the input text at that index.
    ///
    /// # Errors
    ///
    /// Returns an error if any embedding computation fails.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
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

// --- Phase 3b: Persistence classifier ---

/// Trait for classifying whether a fact should be pinned (unforgettable).
///
/// Consumers implement this to apply domain-specific rules:
/// LLM-based classification, regex matching, importance thresholds, etc.
///
/// Default implementation returns `false` — opt-in, zero behavior change.
///
/// **Classifier input caveat:** The `Fact` passed to `should_pin()` during
/// `add_fact()` is a pre-insert synthetic with `id=0`, `scope_id=0`,
/// `importance_score` seeded from base `importance`, and no graph connectivity.
/// Classifiers should only rely on `content`, `fact_type`, `importance`
/// (caller hint), and `metadata` — not on `id`, `scope_id`,
/// `importance_score`, or `access_count`.
pub trait PersistenceClassifier {
    /// Decide if a fact should be pinned (never forgotten).
    fn should_pin(&self, fact: &Fact) -> bool {
        let _ = fact;
        false
    }
}

// --- Phase 4a: Reranker ---

/// Trait for reranking search results after initial retrieval (Phase 4a).
///
/// Cross-encoder rerankers score (query, candidate) pairs precisely,
/// improving nDCG@10 by 5-15% on top-K candidates after RRF merge.
///
/// Optional — when no reranker is provided, RRF results pass through unchanged.
///
/// # Contract
///
/// - Input: query text + candidates from hybrid search (FTS + vector + RRF)
/// - Output: `Vec<(usize, f64)>` — each tuple is `(index_into_candidates, new_score)`
/// - The returned vec length must be <= input length
/// - Every index must be in range `0..candidates.len()`
/// - No duplicate indices in the output
/// - All scores must be finite (not NaN or Inf)
///
/// Returning indices instead of full `SearchResult` values **structurally prevents**
/// the reranker from mutating fact content, embeddings, or match types (issue #144).
///
/// These invariants are enforced at runtime by `MemoryEngine::query()`.
/// Violations produce `MemoryError::Reranker`.
pub trait Reranker: Send + Sync {
    /// Rerank candidates for the given query text.
    ///
    /// Returns `(index, score)` pairs referencing positions in the `candidates` slice.
    /// The engine reconstructs the final result set from these indices, preserving
    /// the original `Fact` and `MatchType` values unchanged.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Reranker` if reranking fails (e.g., API call, inference error).
    fn rerank(&self, query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>>;

    /// Human-readable name for logging and debug output.
    fn name(&self) -> &str;
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
        // Note: weights intentionally do NOT need to sum to 1.0 (ADR-0006).
        // Each signal is independently normalized to [0,1] via ln_1p + ceilings,
        // so the weighted sum naturally falls in [0, sum_of_weights].
        // The result is compared against min_importance, not against 1.0.
        Ok(())
    }
}
