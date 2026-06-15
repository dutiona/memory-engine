//! Shared test infrastructure for the evaluation harness.
//!
//! Self-contained — does NOT deduplicate helpers in other test files
//! (e.g., `ann_recall_test.rs` uses intentionally different embedder variants).

use chrono::{DateTime, Duration, Utc};
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::traits::{
    ConflictArbiter, CrudDecision, EmbeddingProvider, ForgetPolicy, PersistenceClassifier,
    SummaryGenerator,
};
use memory_engine::types::{AddFactOptions, AddFactRequest, Fact, FactType};

use crate::corpus::CorpusDefinition;

/// Embedding dimension used across all eval tests.
pub const DIM: usize = 128;

// ---------------------------------------------------------------------------
// TestEmbedder — blake3-based deterministic embeddings
// ---------------------------------------------------------------------------

/// Deterministic embedder using blake3 hash → normalized vector.
///
/// Produces varied but reproducible embeddings: same text always yields
/// the same vector, different texts yield different (pseudo-random) vectors.
/// No semantic understanding — FTS5 carries retrieval quality.
pub struct TestEmbedder;

impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let mut embedding = vec![0.0_f32; DIM];
        for (i, val) in embedding.iter_mut().enumerate() {
            let byte = bytes[i % 32];
            *val = (f32::from(byte) - 128.0) / 128.0;
        }
        // Normalize to unit vector
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut embedding {
                *val /= norm;
            }
        }
        Ok(embedding)
    }
}

// ---------------------------------------------------------------------------
// MockSummaryGenerator — deterministic summarization
// ---------------------------------------------------------------------------

/// Mock summarizer: joins fact content with "; " and embeds via blake3.
pub struct MockSummaryGenerator;

impl SummaryGenerator for MockSummaryGenerator {
    fn summarize(&self, facts: &[Fact]) -> Result<String> {
        Ok(facts
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("; "))
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        TestEmbedder.embed(text)
    }
}

// ---------------------------------------------------------------------------
// FixedArbiter — parameterized conflict resolution
// ---------------------------------------------------------------------------

/// Arbiter that always returns the same decision.
pub struct FixedArbiter {
    pub decision: CrudDecision,
}

impl ConflictArbiter for FixedArbiter {
    fn arbitrate(&self, _old: &Fact, _new: &Fact) -> Result<CrudDecision> {
        Ok(self.decision)
    }
}

// ---------------------------------------------------------------------------
// PinByType — pins facts of a specific type
// ---------------------------------------------------------------------------

/// Classifier that pins facts matching a given `FactType`.
pub struct PinByType {
    pub pinned_type: FactType,
}

impl PersistenceClassifier for PinByType {
    fn should_pin(&self, fact: &Fact) -> bool {
        fact.fact_type == self.pinned_type
    }
}

// ---------------------------------------------------------------------------
// CorpusBuilder — populates an engine from a corpus definition
// ---------------------------------------------------------------------------

/// Populates a `MemoryEngine` from a `CorpusDefinition`, returning the
/// mapping from corpus index to inserted fact ID.
pub struct CorpusBuilder;

impl CorpusBuilder {
    /// Insert all facts from `corpus` into `engine`. Returns a vec where
    /// `result[i]` is the fact ID for `corpus.facts[i]`.
    pub fn populate(engine: &MemoryEngine, corpus: &CorpusDefinition) -> Result<Vec<i64>> {
        let embedder = TestEmbedder;
        let mut ids = Vec::with_capacity(corpus.facts.len());
        for fact in &corpus.facts {
            let opts = fact.opts.as_ref().map(|o| AddFactOptions {
                importance: o.importance,
                pinned: o.pinned,
                t_valid: o.t_valid,
                t_invalid: o.t_invalid,
                t_created: o.t_created,
                last_accessed: o.last_accessed,
                ..Default::default()
            });
            let id = engine.add_fact(
                &AddFactRequest {
                    content: fact.content.to_string(),
                    fact_type: fact.fact_type.clone(),
                    source_event_id: None,
                    scope: fact.scope.map(String::from),
                    opts,
                },
                &embedder,
                None,
            )?;
            ids.push(id);
        }
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// Convenience helpers
// ---------------------------------------------------------------------------

/// Create an in-memory engine with the standard eval dimension.
pub fn eval_engine() -> MemoryEngine {
    MemoryEngine::open_memory(DIM).expect("failed to create in-memory engine")
}

/// Shorthand to add a single fact with minimal options.
pub fn add_fact(engine: &MemoryEngine, content: &str, fact_type: FactType) -> i64 {
    engine
        .add_fact(
            &AddFactRequest {
                content: content.to_string(),
                fact_type,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &TestEmbedder,
            None,
        )
        .expect("add_fact failed")
}

/// Add a fact with scope.
pub fn add_scoped_fact(
    engine: &MemoryEngine,
    content: &str,
    fact_type: FactType,
    scope: &str,
) -> i64 {
    engine
        .add_fact(
            &AddFactRequest {
                content: content.to_string(),
                fact_type,
                source_event_id: None,
                scope: Some(scope.to_string()),
                opts: None,
            },
            &TestEmbedder,
            None,
        )
        .expect("add_fact failed")
}

/// Add a fact with full options.
pub fn add_fact_with_opts(
    engine: &MemoryEngine,
    content: &str,
    fact_type: FactType,
    scope: Option<&str>,
    opts: AddFactOptions,
) -> i64 {
    engine
        .add_fact(
            &AddFactRequest {
                content: content.to_string(),
                fact_type,
                source_event_id: None,
                scope: scope.map(String::from),
                opts: Some(opts),
            },
            &TestEmbedder,
            None,
        )
        .expect("add_fact failed")
}

/// Create a `ForgetPolicy` that aggressively forgets (high threshold).
pub fn aggressive_forget_policy() -> ForgetPolicy {
    ForgetPolicy {
        min_importance: 0.99,
        ..Default::default()
    }
}

/// Compute the age offset: `now - days`.
pub fn days_ago(days: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(days)
}
