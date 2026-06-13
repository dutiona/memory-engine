//! Basic round-trip: open engine, ingest event, add fact, query.
//!
//! Run with: `cargo run --example basic_roundtrip`
// Example var names (fact1/fact2, event/edge) are intentionally short.
#![allow(clippy::similar_names)]

use memory_engine::MemoryEngine;
use memory_engine::error::MemoryError;
use memory_engine::search::hybrid::{SearchMode, SearchQuery};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, EventType, FactType, NewEvent};

/// Zero-vector embedder for examples (no external model needed).
struct DummyEmbedder;

impl EmbeddingProvider for DummyEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.1; 4])
    }
}

fn main() -> Result<(), MemoryError> {
    let engine = MemoryEngine::open_memory(4)?;
    let embedder = DummyEmbedder;

    // 1. Ingest an event (raw audit log)
    let event_id = engine.ingest(&NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"user": "alice", "message": "Tell me about Rust"}),
        source: "chat".into(),
        session_id: Some("session-1".into()),
        scope_id: 1,
        origin_node_id: "local".into(),
        sequence_id: 0,
        created_at: None,
    })?;
    println!("Ingested event id={event_id}");

    // 2. Add facts derived from the interaction
    let fact1 = engine.add_fact(
        &AddFactRequest {
            content: "Rust is a systems programming language focused on safety and performance"
                .into(),
            fact_type: FactType::Semantic,
            source_event_id: Some(event_id),
            scope: None,
            opts: None,
        },
        &embedder,
        None, // no persistence classifier
    )?;
    println!("Added fact id={fact1}");

    let fact2 = engine.add_fact(
        &AddFactRequest {
            content: "Alice asked about Rust programming".into(),
            fact_type: FactType::Episodic,
            source_event_id: Some(event_id),
            scope: None,
            opts: None,
        },
        &embedder,
        None,
    )?;
    println!("Added fact id={fact2}");

    // 3. Query with hybrid search
    let results = engine.query(&SearchQuery {
        text: Some("Rust programming".into()),
        embedding: Some(vec![0.1; 4]),
        mode: SearchMode::Hybrid,
        limit: 5,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    })?;

    println!("\nSearch results:");
    for r in &results {
        println!(
            "  [{:.4}] [{}] {}",
            r.score,
            match r.fact.fact_type {
                FactType::Episodic => "episodic",
                FactType::Semantic => "semantic",
                FactType::Procedural => "procedural",
            },
            r.fact.content
        );
    }

    // 4. Check engine stats
    let (nodes, edges) = engine.graph_stats();
    let facts = engine.list_active_facts(None)?;
    println!(
        "\nEngine stats: {} facts, {} graph nodes, {} edges",
        facts.len(),
        nodes,
        edges
    );

    Ok(())
}
