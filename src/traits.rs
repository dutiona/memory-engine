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

        if !self.half_life_days.is_finite() || self.half_life_days <= 0.0 {
            return Err(MemoryError::Conflict("half_life_days must be > 0".into()));
        }
        for (ft, &hl) in &self.half_life_overrides {
            if !hl.is_finite() || hl <= 0.0 {
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
        if !self.recency_weight.is_finite()
            || !self.frequency_weight.is_finite()
            || !self.graph_degree_weight.is_finite()
            || !self.base_importance_weight.is_finite()
            || self.recency_weight < 0.0
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FactType;
    use chrono::Utc;

    fn stub_fact() -> Fact {
        Fact {
            id: 0,
            content: String::new(),
            content_hash: String::new(),
            embedding: vec![],
            fact_type: FactType::Semantic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::Value::Null,
            scope_id: 0,
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        }
    }

    // --- ForgetPolicy::default() ---

    #[test]
    fn default_has_expected_field_values() {
        let p = ForgetPolicy::default();
        assert!((p.half_life_days - 69.0).abs() < f64::EPSILON);
        assert!(p.half_life_overrides.is_empty());
        assert!((p.min_importance - 0.1).abs() < f64::EPSILON);
        assert!((p.recency_weight - 0.3).abs() < f64::EPSILON);
        assert!((p.frequency_weight - 0.2).abs() < f64::EPSILON);
        assert!((p.graph_degree_weight - 0.3).abs() < f64::EPSILON);
        assert!((p.base_importance_weight - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn default_validates_ok() {
        ForgetPolicy::default().validate().unwrap();
    }

    // --- ForgetPolicy::validate() error paths ---

    #[test]
    fn validate_rejects_zero_half_life() {
        let p = ForgetPolicy {
            half_life_days: 0.0,
            ..Default::default()
        };
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("half_life_days"), "error: {err}");
    }

    #[test]
    fn validate_rejects_negative_half_life() {
        let p = ForgetPolicy {
            half_life_days: -1.0,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_override_half_life() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Episodic, 0.0);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("Episodic"), "error: {err}");
    }

    #[test]
    fn validate_rejects_negative_override_half_life() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Procedural, -5.0);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_accepts_valid_override() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Episodic, 30.0);
        overrides.insert(FactType::Procedural, 365.0);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        p.validate().unwrap();
    }

    #[test]
    fn validate_rejects_min_importance_above_one() {
        let p = ForgetPolicy {
            min_importance: 1.01,
            ..Default::default()
        };
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("min_importance"), "error: {err}");
    }

    #[test]
    fn validate_rejects_negative_min_importance() {
        let p = ForgetPolicy {
            min_importance: -0.01,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_accepts_boundary_min_importance() {
        for val in [0.0, 1.0] {
            let p = ForgetPolicy {
                min_importance: val,
                ..Default::default()
            };
            p.validate().unwrap();
        }
    }

    #[test]
    fn validate_rejects_negative_weight() {
        let cases = [
            (
                "recency",
                ForgetPolicy {
                    recency_weight: -0.1,
                    ..Default::default()
                },
            ),
            (
                "frequency",
                ForgetPolicy {
                    frequency_weight: -0.1,
                    ..Default::default()
                },
            ),
            (
                "graph_degree",
                ForgetPolicy {
                    graph_degree_weight: -0.1,
                    ..Default::default()
                },
            ),
            (
                "base_importance",
                ForgetPolicy {
                    base_importance_weight: -0.1,
                    ..Default::default()
                },
            ),
        ];
        for (name, p) in &cases {
            assert!(
                p.validate().is_err(),
                "{name} weight should reject negative"
            );
        }
    }

    #[test]
    fn validate_accepts_zero_weights() {
        let p = ForgetPolicy {
            recency_weight: 0.0,
            frequency_weight: 0.0,
            graph_degree_weight: 0.0,
            base_importance_weight: 0.0,
            ..Default::default()
        };
        p.validate().unwrap();
    }

    #[test]
    fn validate_accepts_weights_summing_above_one() {
        // ADR-0006: weights don't need to sum to 1.0
        let p = ForgetPolicy {
            recency_weight: 1.0,
            frequency_weight: 1.0,
            graph_degree_weight: 1.0,
            base_importance_weight: 1.0,
            ..Default::default()
        };
        p.validate().unwrap();
    }

    // --- NaN rejection (IEEE 754: NaN comparisons always return false) ---

    #[test]
    fn validate_rejects_nan_half_life() {
        let p = ForgetPolicy {
            half_life_days: f64::NAN,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_infinity_half_life() {
        let p = ForgetPolicy {
            half_life_days: f64::INFINITY,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_nan_override_half_life() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(FactType::Semantic, f64::NAN);
        let p = ForgetPolicy {
            half_life_overrides: overrides,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_nan_weight() {
        let cases = [
            ForgetPolicy {
                recency_weight: f64::NAN,
                ..Default::default()
            },
            ForgetPolicy {
                frequency_weight: f64::NAN,
                ..Default::default()
            },
            ForgetPolicy {
                graph_degree_weight: f64::NAN,
                ..Default::default()
            },
            ForgetPolicy {
                base_importance_weight: f64::NAN,
                ..Default::default()
            },
        ];
        for (i, p) in cases.iter().enumerate() {
            assert!(p.validate().is_err(), "NaN weight case {i} should reject");
        }
    }

    #[test]
    fn validate_rejects_nan_min_importance() {
        let p = ForgetPolicy {
            min_importance: f64::NAN,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    // --- Trait object safety ---

    #[test]
    fn embedding_provider_is_object_safe() {
        struct Dummy;
        impl EmbeddingProvider for Dummy {
            fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
                Ok(vec![0.0])
            }
        }
        let _: &dyn EmbeddingProvider = &Dummy;
    }

    #[test]
    fn summary_generator_is_object_safe() {
        struct Dummy;
        impl SummaryGenerator for Dummy {
            fn summarize(&self, _facts: &[Fact]) -> crate::error::Result<String> {
                Ok(String::new())
            }
            fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
                Ok(vec![0.0])
            }
        }
        let _: &dyn SummaryGenerator = &Dummy;
    }

    #[test]
    fn conflict_arbiter_is_object_safe() {
        struct Dummy;
        impl ConflictArbiter for Dummy {
            fn arbitrate(&self, _old: &Fact, _new: &Fact) -> crate::error::Result<CrudDecision> {
                Ok(CrudDecision::Noop)
            }
        }
        let _: &dyn ConflictArbiter = &Dummy;
    }

    #[test]
    fn persistence_classifier_is_object_safe() {
        struct Dummy;
        impl PersistenceClassifier for Dummy {}
        let p: &dyn PersistenceClassifier = &Dummy;
        // Default impl returns false
        assert!(!p.should_pin(&stub_fact()));
    }

    #[test]
    fn reranker_is_object_safe() {
        struct Dummy;
        impl Reranker for Dummy {
            fn rerank(
                &self,
                _query: &str,
                _candidates: &[SearchResult],
            ) -> crate::error::Result<Vec<(usize, f64)>> {
                Ok(vec![])
            }
            fn name(&self) -> &str {
                "dummy"
            }
        }
        let r: &dyn Reranker = &Dummy;
        assert_eq!(r.name(), "dummy");
    }

    // --- EmbeddingProvider::embed_batch default ---

    #[test]
    fn embed_batch_default_loops_embed() {
        struct Counter(std::sync::atomic::AtomicUsize);
        impl EmbeddingProvider for Counter {
            fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(vec![1.0, 2.0])
            }
        }
        let c = Counter(std::sync::atomic::AtomicUsize::new(0));
        let results = c.embed_batch(&["a", "b", "c"]).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(c.0.load(std::sync::atomic::Ordering::Relaxed), 3);
        assert!(results.iter().all(|v| v.len() == 2
            && (v[0] - 1.0).abs() < f32::EPSILON
            && (v[1] - 2.0).abs() < f32::EPSILON));
    }

    #[test]
    fn embed_batch_empty_returns_empty() {
        struct Dummy;
        impl EmbeddingProvider for Dummy {
            fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
                Ok(vec![0.0])
            }
        }
        assert!(Dummy.embed_batch(&[]).unwrap().is_empty());
    }

    #[test]
    fn embed_batch_propagates_error() {
        struct Failing;
        impl EmbeddingProvider for Failing {
            fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
                Err(crate::error::MemoryError::Conflict("boom".into()))
            }
        }
        assert!(Failing.embed_batch(&["a"]).is_err());
    }

    // --- PersistenceClassifier default ---

    #[test]
    fn persistence_classifier_default_returns_false() {
        struct Blank;
        impl PersistenceClassifier for Blank {}
        assert!(!Blank.should_pin(&stub_fact()));
    }
}
