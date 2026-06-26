//! Tests for explicit search mode vs engine-inferred mode behavior.
//!
//! Validates the mode selection logic in `handle_query`:
//! - Explicit `mode=fts` → FTS-only, no embedding needed
//! - Explicit `mode=vector` → requires embedding (provider or pre-computed)
//! - Explicit `mode=hybrid` → requires embedding
//! - No mode + text only + no embedder → engine infers FTS
//! - No mode + text + embedder → engine infers hybrid (embedding provided)
//! - No mode + embedding only → engine infers vector

use std::sync::Arc;

use memory_engine::MemoryEngine;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::AddFactRequest;
use memory_engine_mcp::tools;
use serde_json::{Map, Value, json};

const DIM: usize = 8;

struct TestEmbedder {
    dim: usize,
}

impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, text: &str) -> memory_engine::error::Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let mut embedding = vec![0.0_f32; self.dim];
        for (i, val) in embedding.iter_mut().enumerate() {
            let byte = bytes[i % 32];
            *val = (f32::from(byte) - 128.0) / 128.0;
        }
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut embedding {
                *val /= norm;
            }
        }
        Ok(embedding)
    }

    fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
        memory_engine::EmbeddingFingerprint::new("mock", "test", self.dim)
    }
}

fn make_engine() -> MemoryEngine {
    MemoryEngine::builder(DIM)
        .build()
        .expect("in-memory engine")
}

async fn seed_facts(engine: &MemoryEngine) {
    let emb: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust ownership model prevents data races".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            emb.clone(),
            None,
        )
        .await
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Python GIL limits true parallelism".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            emb.clone(),
            None,
        )
        .await
        .unwrap();
}

fn args(pairs: Value) -> Map<String, Value> {
    match pairs {
        Value::Object(m) => m,
        _ => panic!("args() requires a JSON object"),
    }
}

fn unwrap_ok(result: Result<rmcp::model::CallToolResult, rmcp::model::ErrorData>) -> Value {
    let call_result = result.expect("dispatch should succeed");
    let content = call_result.content.first().expect("no content in result");
    let text = content
        .as_text()
        .expect("expected Text content")
        .text
        .as_str();
    serde_json::from_str(text).expect("content is not valid JSON")
}

// ---------------------------------------------------------------------------
// Explicit mode: fts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn explicit_fts_no_embedder_succeeds() {
    let engine = make_engine();
    seed_facts(&engine).await;

    // mode=fts + text + no embedder → should work fine
    let result = tools::dispatch(
        "memory_query",
        args(json!({ "text": "Rust", "mode": "fts" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    let body = unwrap_ok(result);
    assert!(body["count"].as_u64().unwrap() >= 1);
}

// ---------------------------------------------------------------------------
// Explicit mode: vector (requires embedding)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn explicit_vector_no_embedder_no_embedding_fails() {
    let engine = make_engine();
    seed_facts(&engine).await;

    // mode=vector + text + no embedder + no pre-computed embedding → error
    let result = tools::dispatch(
        "memory_query",
        args(json!({ "text": "Rust", "mode": "vector" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn explicit_vector_with_precomputed_embedding_succeeds() {
    let engine = make_engine();
    seed_facts(&engine).await;

    let embedder = TestEmbedder { dim: DIM };
    let emb = embedder.embed("Rust ownership").unwrap();

    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "Rust",
            "mode": "vector",
            "embedding": emb,
            "model": "mock",
            "provider": "test",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    let body = unwrap_ok(result);
    assert!(body["count"].as_u64().unwrap() >= 1);
}

// ---------------------------------------------------------------------------
// Explicit mode: hybrid
// ---------------------------------------------------------------------------

#[tokio::test]
async fn explicit_hybrid_no_embedder_no_embedding_fails() {
    let engine = make_engine();
    seed_facts(&engine).await;

    // mode=hybrid + text + no embedder + no embedding → error
    let result = tools::dispatch(
        "memory_query",
        args(json!({ "text": "Rust", "mode": "hybrid" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn explicit_hybrid_with_precomputed_embedding_succeeds() {
    let engine = make_engine();
    seed_facts(&engine).await;

    let embedder = TestEmbedder { dim: DIM };
    let emb = embedder.embed("Rust ownership").unwrap();

    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "Rust",
            "mode": "hybrid",
            "embedding": emb,
            "model": "mock",
            "provider": "test",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    let body = unwrap_ok(result);
    assert!(body["count"].as_u64().unwrap() >= 1);
}

// ---------------------------------------------------------------------------
// No explicit mode — engine inference
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inferred_mode_text_only_no_embedder_falls_back_to_fts() {
    let engine = make_engine();
    seed_facts(&engine).await;

    // No mode + text + no embedder → engine should infer FTS (no embedding provided)
    let result = tools::dispatch(
        "memory_query",
        args(json!({ "text": "Rust" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    let body = unwrap_ok(result);
    assert!(body["count"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn inferred_mode_embedding_only_works() {
    let engine = make_engine();
    seed_facts(&engine).await;

    let embedder = TestEmbedder { dim: DIM };
    let emb = embedder.embed("ownership memory safety").unwrap();

    // No mode + no text + embedding → engine should infer vector search
    let result = tools::dispatch(
        "memory_query",
        args(json!({ "embedding": emb, "model": "mock", "provider": "test" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    let body = unwrap_ok(result);
    // Should return some results via vector search
    assert!(body.as_object().unwrap().contains_key("results"));
}

// ---------------------------------------------------------------------------
// Unknown mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_mode_returns_error() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_query",
        args(json!({ "text": "test", "mode": "bogus" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Scope + mode interaction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_with_scope_parameters_accepted() {
    let engine = make_engine();
    let emb: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder { dim: DIM });

    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust borrow checker ensures safety".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: Some("lang/rust".into()),
                opts: None,
            },
            emb.clone(),
            None,
        )
        .await
        .unwrap();

    // Verify all scope_mode variants are accepted by the dispatch layer
    for scope_mode in &["subtree", "exact", "ancestors", "inherited"] {
        let result = tools::dispatch(
            "memory_query",
            args(json!({
                "text": "Rust",
                "mode": "fts",
                "scope": "lang/rust",
                "scope_mode": scope_mode,
            })),
            &engine,
            None,
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        )
        .await;
        // Should succeed (no validation error), regardless of result count
        assert!(
            result.is_ok(),
            "scope_mode '{scope_mode}' should be accepted"
        );
    }
}

#[tokio::test]
async fn query_with_invalid_scope_mode_returns_error() {
    let engine = make_engine();
    let emb: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder { dim: DIM });

    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust borrow checker ensures safety".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: Some("lang/rust".into()),
                opts: None,
            },
            emb.clone(),
            None,
        )
        .await
        .unwrap();

    // An explicit but unknown scope_mode must error rather than silently
    // falling back to subtree.
    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "Rust",
            "mode": "fts",
            "scope": "lang/rust",
            "scope_mode": "bogus",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    let err = result.expect_err("invalid scope_mode should error");
    assert!(
        err.message.contains("scope_mode"),
        "error message should mention scope_mode, got: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// Fact type filter with mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fts_with_fact_type_filter() {
    let engine = make_engine();
    let emb: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder { dim: DIM });

    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust compile-time guarantees".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            emb.clone(),
            None,
        )
        .await
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Today I learned about Rust lifetimes".into(),
                fact_type: memory_engine::types::FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            emb.clone(),
            None,
        )
        .await
        .unwrap();

    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "Rust",
            "mode": "fts",
            "fact_type": "Episodic",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    let body = unwrap_ok(result);
    assert_eq!(body["count"].as_u64().unwrap(), 1);
}
