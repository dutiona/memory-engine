//! Bi-temporal facts and temporal filtering.
//!
//! Demonstrates `t_valid`/`t_invalid` (real-world validity) and `valid_at` queries.
//!
//! Run with: `cargo run --example bi_temporal_query`

use chrono::{Duration, Utc};
use memory_engine::MemoryEngine;
use memory_engine::error::MemoryError;
use memory_engine::search::hybrid::{SearchMode, SearchQuery};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, FactType};

struct DummyEmbedder;

impl EmbeddingProvider for DummyEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.1; 4])
    }
}

fn main() -> Result<(), MemoryError> {
    let engine = MemoryEngine::open_memory(4)?;
    let embedder = DummyEmbedder;
    let now = Utc::now();

    // Fact valid from yesterday, no expiry
    engine.add_fact(
        "The project deadline is March 15",
        FactType::Semantic,
        None,
        &embedder,
        None,
        Some(&AddFactOptions {
            importance: Some(0.9),
            t_valid: Some(now - Duration::days(1)),
            t_invalid: Some(now + Duration::days(5)),
            ..Default::default()
        }),
    )?;

    // Fact valid only in the future (scheduled memory)
    engine.add_fact(
        "Remember to review PR after March 20",
        FactType::Procedural,
        None,
        &embedder,
        None,
        Some(&AddFactOptions {
            importance: Some(0.8),
            t_valid: Some(now + Duration::days(10)),
            ..Default::default()
        }),
    )?;

    // Fact that expired yesterday (historical knowledge)
    engine.add_fact(
        "The meeting was scheduled for March 8",
        FactType::Episodic,
        None,
        &embedder,
        None,
        Some(&AddFactOptions {
            t_valid: Some(now - Duration::days(5)),
            t_invalid: Some(now - Duration::days(1)),
            ..Default::default()
        }),
    )?;

    // Query: what's valid RIGHT NOW?
    let results_now = engine.query(&SearchQuery {
        text: Some("deadline meeting review".into()),
        embedding: Some(vec![0.1; 4]),
        mode: SearchMode::Hybrid,
        limit: 10,
        valid_at: Some(now),
        fact_type: None,
        scope: None,
    })?;

    println!("Facts valid NOW ({}):", now.format("%Y-%m-%d"));
    for r in &results_now {
        println!("  [{}] {}", r.fact.fact_type_str(), r.fact.content);
    }

    // Query: what was valid 3 days ago?
    let past = now - Duration::days(3);
    let results_past = engine.query(&SearchQuery {
        text: Some("deadline meeting review".into()),
        embedding: Some(vec![0.1; 4]),
        mode: SearchMode::Hybrid,
        limit: 10,
        valid_at: Some(past),
        fact_type: None,
        scope: None,
    })?;

    println!("\nFacts valid 3 days ago ({}):", past.format("%Y-%m-%d"));
    for r in &results_past {
        println!("  [{}] {}", r.fact.fact_type_str(), r.fact.content);
    }

    // Query: what will be valid in 2 weeks?
    let future = now + Duration::days(14);
    let results_future = engine.query(&SearchQuery {
        text: Some("deadline meeting review".into()),
        embedding: Some(vec![0.1; 4]),
        mode: SearchMode::Hybrid,
        limit: 10,
        valid_at: Some(future),
        fact_type: None,
        scope: None,
    })?;

    println!("\nFacts valid in 2 weeks ({}):", future.format("%Y-%m-%d"));
    for r in &results_future {
        println!("  [{}] {}", r.fact.fact_type_str(), r.fact.content);
    }

    Ok(())
}

/// Helper trait to display fact type as string.
trait FactTypeDisplay {
    fn fact_type_str(&self) -> &'static str;
}

impl FactTypeDisplay for memory_engine::types::Fact {
    fn fact_type_str(&self) -> &'static str {
        match self.fact_type {
            FactType::Episodic => "episodic",
            FactType::Semantic => "semantic",
            FactType::Procedural => "procedural",
        }
    }
}
