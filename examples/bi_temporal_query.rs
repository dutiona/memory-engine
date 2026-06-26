//! Bi-temporal facts and temporal filtering.
//!
//! Demonstrates `t_valid`/`t_invalid` (real-world validity) and `valid_at` queries.
//!
//! Run with: `cargo run --example bi_temporal_query`

use chrono::{Duration, Utc};
use memory_engine::EmbeddingFingerprint;
use memory_engine::MemoryEngine;
use memory_engine::error::MemoryError;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use memory_engine::{SearchMode, SearchQuery};

struct DummyEmbedder;

impl EmbeddingProvider for DummyEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.1; 4])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", 4)
    }
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), MemoryError> {
    let engine = MemoryEngine::builder(4).build()?;
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(DummyEmbedder);
    let now = Utc::now();

    // Fact valid from yesterday, no expiry
    engine
        .add_fact(
            &AddFactRequest {
                content: "The project deadline is March 15".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    base_importance: Some(0.9),
                    t_valid: Some(now - Duration::days(1)),
                    t_invalid: Some(now + Duration::days(5)),
                    ..Default::default()
                }),
            },
            embedder.clone(),
            None,
        )
        .await?;

    // Fact valid only in the future (scheduled memory)
    engine
        .add_fact(
            &AddFactRequest {
                content: "Remember to review PR after March 20".into(),
                fact_type: FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    base_importance: Some(0.8),
                    t_valid: Some(now + Duration::days(10)),
                    ..Default::default()
                }),
            },
            embedder.clone(),
            None,
        )
        .await?;

    // Fact that expired yesterday (historical knowledge)
    engine
        .add_fact(
            &AddFactRequest {
                content: "The meeting was scheduled for March 8".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    t_valid: Some(now - Duration::days(5)),
                    t_invalid: Some(now - Duration::days(1)),
                    ..Default::default()
                }),
            },
            embedder.clone(),
            None,
        )
        .await?;

    // Query: what's valid RIGHT NOW?
    let results_now = engine
        .query(
            &SearchQuery::new(SearchMode::Hybrid, 10)
                .text("deadline meeting review")
                .embedding(vec![0.1; 4])
                .valid_at(now),
        )
        .await?;

    println!("Facts valid NOW ({}):", now.format("%Y-%m-%d"));
    for r in &results_now {
        println!("  [{}] {}", r.fact.fact_type_str(), r.fact.content);
    }

    // Query: what was valid 3 days ago?
    let past = now - Duration::days(3);
    let results_past = engine
        .query(
            &SearchQuery::new(SearchMode::Hybrid, 10)
                .text("deadline meeting review")
                .embedding(vec![0.1; 4])
                .valid_at(past),
        )
        .await?;

    println!("\nFacts valid 3 days ago ({}):", past.format("%Y-%m-%d"));
    for r in &results_past {
        println!("  [{}] {}", r.fact.fact_type_str(), r.fact.content);
    }

    // Query: what will be valid in 2 weeks?
    let future = now + Duration::days(14);
    let results_future = engine
        .query(
            &SearchQuery::new(SearchMode::Hybrid, 10)
                .text("deadline meeting review")
                .embedding(vec![0.1; 4])
                .valid_at(future),
        )
        .await?;

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
