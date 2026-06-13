//! Implementing all three consumer traits: `EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`.
//!
//! Run with: `cargo run --example custom_traits`
// Toy embedder: char codes (< 2^21) convert to f32 without precision loss.
#![allow(clippy::cast_precision_loss)]

use memory_engine::MemoryEngine;
use memory_engine::error::MemoryError;
use memory_engine::traits::{
    ConflictArbiter, ConsolidationConfig, CrudDecision, EmbeddingProvider, ForgetPolicy,
    SummaryGenerator,
};
use memory_engine::types::{AddFactRequest, Fact, FactType, NewFact};

// --- EmbeddingProvider: consumers bring their own embedding model ---

struct SimpleEmbedder;

impl EmbeddingProvider for SimpleEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        // Toy: use first 4 char codes normalized to [0,1].
        // Replace with ONNX Runtime, API call, etc. in production.
        let mut v = vec![0.0_f32; 4];
        for (i, ch) in text.chars().take(4).enumerate() {
            v[i] = (ch as u32 as f32) / 128.0;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Ok(v)
    }
}

// --- SummaryGenerator: consumers bring their own LLM for summarization ---

struct ConcatSummarizer;

impl SummaryGenerator for ConcatSummarizer {
    fn summarize(&self, facts: &[Fact]) -> Result<String, MemoryError> {
        // Toy: concatenate fact contents. Replace with LLM call in production.
        let summary = facts
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Ok(format!("Summary: {summary}"))
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        SimpleEmbedder.embed(text)
    }
}

// --- ConflictArbiter: consumers define how to resolve contradictions ---

struct RecencyArbiter;

impl ConflictArbiter for RecencyArbiter {
    fn arbitrate(&self, old: &Fact, new: &Fact) -> Result<CrudDecision, MemoryError> {
        // Simple strategy: newer fact always wins (Update replaces old).
        // Production: use LLM to compare semantics, check confidence scores, etc.
        if new.t_created > old.t_created {
            Ok(CrudDecision::Update)
        } else {
            Ok(CrudDecision::Noop)
        }
    }
}

// Example walkthrough kept linear for readability rather than split into helpers.
#[allow(clippy::too_many_lines)]
fn main() -> Result<(), MemoryError> {
    let engine = MemoryEngine::open_memory(4)?;
    let embedder = SimpleEmbedder;

    // Add some facts
    engine.add_fact(
        &AddFactRequest {
            content: "Rust uses ownership for memory safety".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
        &embedder,
        None,
    )?;
    engine.add_fact(
        &AddFactRequest {
            content: "Rust was created by Graydon Hoare".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
        &embedder,
        None,
    )?;
    engine.add_fact(
        &AddFactRequest {
            content: "Rust 1.85 introduced edition 2024".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
        &embedder,
        None,
    )?;
    engine.add_fact(
        &AddFactRequest {
            content: "Rust uses ownership for memory safety".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
        &embedder,
        None,
    )?; // duplicate

    println!("Added 4 facts (1 duplicate)");
    println!("Active facts: {}", engine.list_active_facts(None)?.len());

    // --- Consolidation: uses SummaryGenerator ---
    let summarizer = ConcatSummarizer;
    let stats = engine.consolidate(
        &summarizer,
        &ConsolidationConfig {
            dedup_threshold: 0.99, // high threshold to catch near-exact duplicates
            min_cluster_size: 2,
        },
    )?;
    println!(
        "\nConsolidation: {} duplicates removed, {} clusters, {} global summaries",
        stats.duplicates_removed, stats.clusters_created, stats.global_summaries
    );
    println!(
        "Active facts after consolidation: {}",
        engine.list_active_facts(None)?.len()
    );

    // --- Forgetting: uses ForgetPolicy (configurable struct, not a trait) ---
    let policy = ForgetPolicy {
        half_life_days: 1.0, // aggressive for demo
        min_importance: 0.3,
        ..ForgetPolicy::default()
    };
    let prune_stats = engine.forget(&policy)?;
    println!(
        "\nForgetting: {}/{} facts expired",
        prune_stats.facts_expired, prune_stats.facts_evaluated
    );

    // --- Conflict resolution: uses ConflictArbiter ---
    let fact_id = engine.add_fact(
        &AddFactRequest {
            content: "Rust 1.85 is the latest stable".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
        &embedder,
        None,
    )?;

    let conflicting = NewFact {
        content: "Rust 1.86 is the latest stable".into(),
        content_hash: String::new(),
        embedding: embedder.embed("Rust 1.86 is the latest stable")?,
        fact_type: FactType::Semantic,
        t_created: chrono::Utc::now(),
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: 1,
        importance: 0.5,
        access_count: 0,
        last_accessed: chrono::Utc::now(),
        metadata: serde_json::json!({}),
        is_pinned: false,
    };

    let arbiter = RecencyArbiter;
    let resolution = engine.resolve_conflict(&arbiter, fact_id, &conflicting)?;
    println!(
        "\nConflict resolution: {:?} (old={}, new={:?})",
        resolution.decision, resolution.old_fact_id, resolution.new_fact_id
    );

    // Final state
    let facts = engine.list_active_facts(None)?;
    println!("\nFinal active facts: {}", facts.len());
    for f in &facts {
        println!("  [id={}] {}", f.id, f.content);
    }

    Ok(())
}
