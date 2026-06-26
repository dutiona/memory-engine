#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use chrono::Utc;
use memory_engine::EmbeddingFingerprint;
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, EventType, FactType, NewEvent};
use memory_engine::{MatchType, SearchMode, SearchQuery};

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
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", self.dim)
    }
}

#[tokio::test]
// End-to-end scenario kept as one linear test for readability.
#[allow(clippy::too_many_lines)]
async fn full_roundtrip() {
    let dim = 8;
    let engine = MemoryEngine::builder(dim).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(TestEmbedder { dim });

    // 1. Ingest an event
    let event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"user": "test", "msg": "learning Rust"}),
        source: "integration_test".into(),
        session_id: Some("sess-roundtrip".into()),
        scope_id: 1,
        origin_node_id: "local".into(),
        sequence_id: 0,
        created_at: None,
    };
    let event_id = engine.ingest(&event).await.unwrap();
    assert!(event_id > 0);

    // 2. Add facts derived from the event
    let fact1_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is a systems programming language".into(),
                fact_type: FactType::Semantic,
                source_event_id: Some(event_id),
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    let fact2_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Rust has zero-cost abstractions and memory safety".into(),
                fact_type: FactType::Semantic,
                source_event_id: Some(event_id),
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    let fact3_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Python is popular for machine learning".into(),
                fact_type: FactType::Episodic,
                source_event_id: Some(event_id),
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    assert!(fact1_id > 0);
    assert!(fact2_id > 0);
    assert!(fact3_id > 0);

    // 3. Query via FTS — "Rust" should match facts 1 and 2
    let fts_results = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("Rust"))
        .await
        .unwrap();
    assert_eq!(fts_results.len(), 2);
    assert!(fts_results.iter().all(|r| r.match_type == MatchType::Fts));
    assert!(fts_results.iter().all(|r| r.fact.content.contains("Rust")));

    // 4. Query via vector — use the embedding of "Rust systems programming"
    let query_emb = embedder.embed("Rust systems programming").unwrap();
    let vec_results = engine
        .query(&SearchQuery::new(SearchMode::Vector, 10).embedding(query_emb))
        .await
        .unwrap();
    assert!(!vec_results.is_empty());
    assert!(
        vec_results
            .iter()
            .all(|r| r.match_type == MatchType::Vector)
    );

    // 5. Query via hybrid — combine text and embedding
    let hybrid_emb = embedder.embed("Rust programming language").unwrap();
    let hybrid_results = engine
        .query(
            &SearchQuery::new(SearchMode::Hybrid, 10)
                .text("Rust")
                .embedding(hybrid_emb),
        )
        .await
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
        .query(
            &SearchQuery::new(SearchMode::Fts, 10)
                .text("Rust")
                .fact_type(FactType::Semantic),
        )
        .await
        .unwrap();
    assert!(
        semantic_only
            .iter()
            .all(|r| r.fact.fact_type == FactType::Semantic)
    );
}
