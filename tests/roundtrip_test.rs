use chrono::Utc;
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::search::hybrid::{MatchType, SearchMode, SearchQuery};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{EventType, FactType, NewEvent};

/// Mock embedder that returns a vector pointing in the direction
/// determined by a simple hash of the text, so different texts
/// get somewhat different embeddings.
struct TestEmbedder {
    dim: usize,
}

impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let mut embedding = vec![0.0_f32; self.dim];
        for (i, val) in embedding.iter_mut().enumerate() {
            // Use hash bytes cyclically to produce deterministic but varied values
            let byte = bytes[i % 32];
            *val = (f32::from(byte) - 128.0) / 128.0;
        }
        // Normalize
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut embedding {
                *val /= norm;
            }
        }
        Ok(embedding)
    }
}

#[test]
fn full_roundtrip() {
    let dim = 8;
    let engine = MemoryEngine::open_memory(dim).unwrap();
    let embedder = TestEmbedder { dim };

    // 1. Ingest an event
    let event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"user": "test", "msg": "learning Rust"}),
        source: "integration_test".into(),
        session_id: Some("sess-roundtrip".into()),
    };
    let event_id = engine.ingest(&event).unwrap();
    assert!(event_id > 0);

    // 2. Add facts derived from the event
    let fact1_id = engine
        .add_fact(
            "Rust is a systems programming language",
            FactType::Semantic,
            Some(event_id),
            &embedder,
        )
        .unwrap();
    let fact2_id = engine
        .add_fact(
            "Rust has zero-cost abstractions and memory safety",
            FactType::Semantic,
            Some(event_id),
            &embedder,
        )
        .unwrap();
    let fact3_id = engine
        .add_fact(
            "Python is popular for machine learning",
            FactType::Episodic,
            Some(event_id),
            &embedder,
        )
        .unwrap();
    assert!(fact1_id > 0);
    assert!(fact2_id > 0);
    assert!(fact3_id > 0);

    // 3. Query via FTS — "Rust" should match facts 1 and 2
    let fts_results = engine
        .query(&SearchQuery {
            text: Some("Rust".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            valid_at: None,
            fact_type: None,
        })
        .unwrap();
    assert_eq!(fts_results.len(), 2);
    assert!(fts_results.iter().all(|r| r.match_type == MatchType::Fts));
    assert!(fts_results.iter().all(|r| r.fact.content.contains("Rust")));

    // 4. Query via vector — use the embedding of "Rust systems programming"
    let query_emb = embedder.embed("Rust systems programming").unwrap();
    let vec_results = engine
        .query(&SearchQuery {
            text: None,
            embedding: Some(query_emb),
            mode: SearchMode::Vector,
            limit: 10,
            valid_at: None,
            fact_type: None,
        })
        .unwrap();
    assert!(!vec_results.is_empty());
    assert!(vec_results
        .iter()
        .all(|r| r.match_type == MatchType::Vector));

    // 5. Query via hybrid — combine text and embedding
    let hybrid_emb = embedder.embed("Rust programming language").unwrap();
    let hybrid_results = engine
        .query(&SearchQuery {
            text: Some("Rust".into()),
            embedding: Some(hybrid_emb),
            mode: SearchMode::Hybrid,
            limit: 10,
            valid_at: None,
            fact_type: None,
        })
        .unwrap();
    assert!(!hybrid_results.is_empty());
    // At least one result should come from both sources
    let has_both = hybrid_results
        .iter()
        .any(|r| r.match_type == MatchType::Both);
    // Rust facts appear in FTS and should also rank high in vector
    assert!(
        has_both,
        "expected at least one result from both FTS and vector"
    );

    // 6. Verify fact_type filter
    let semantic_only = engine
        .query(&SearchQuery {
            text: Some("Rust".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            valid_at: None,
            fact_type: Some(FactType::Semantic),
        })
        .unwrap();
    assert!(semantic_only
        .iter()
        .all(|r| r.fact.fact_type == FactType::Semantic));
}
