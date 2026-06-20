//! Integration tests exercising all 10 MCP tools via `tools::dispatch()`.
//!
//! Each test creates an in-memory `MemoryEngine`, bypasses MCP transport,
//! and calls the dispatch function directly — validating argument parsing,
//! engine interaction, response shaping, and error mapping.

use memory_engine::MemoryEngine;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::AddFactRequest;
use memory_engine_mcp::tools;
use serde_json::{Map, Value, json};

const DIM: usize = 8;

// ---------------------------------------------------------------------------
// Test embedder (deterministic, blake3-based — mirrors roundtrip_test.rs)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_engine() -> MemoryEngine {
    MemoryEngine::builder(DIM)
        .build()
        .expect("in-memory engine")
}

const fn make_embedder() -> TestEmbedder {
    TestEmbedder { dim: DIM }
}

/// Stamp the store's embedding identity via a real-embedder write. A fresh store has
/// no identity, and #614 makes a precomputed `memory_add_fact` require a present one
/// (it cannot establish identity from a sentinel). Mirrors `query_with_precomputed_embedding`.
fn stamp_identity(engine: &MemoryEngine) {
    engine
        .add_fact(
            &AddFactRequest {
                content: "identity-seed".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &make_embedder(),
            None,
        )
        .expect("stamp embedding identity");
}

fn args(pairs: Value) -> Map<String, Value> {
    match pairs {
        Value::Object(m) => m,
        _ => panic!("args() requires a JSON object"),
    }
}

/// Extract the JSON body from a successful `CallToolResult`.
fn unwrap_ok(result: Result<rmcp::model::CallToolResult, rmcp::model::ErrorData>) -> Value {
    let call_result = result.expect("dispatch should succeed");
    assert!(
        !call_result.is_error.unwrap_or(false),
        "tool returned is_error=true"
    );
    let content = call_result.content.first().expect("no content in result");
    let text = content
        .as_text()
        .expect("expected Text content")
        .text
        .as_str();
    serde_json::from_str(text).expect("content is not valid JSON")
}

// ---------------------------------------------------------------------------
// 1. memory_ingest
// ---------------------------------------------------------------------------

#[test]
fn ingest_minimal() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_ingest",
        args(json!({
            "event_type": "Interaction",
            "payload": {"msg": "hello"},
            "source": "test"
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body["event_id"].as_i64().unwrap() > 0);
}

#[test]
fn ingest_with_all_optional_fields() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_ingest",
        args(json!({
            "event_type": "ToolCall",
            "payload": {"tool": "grep"},
            "source": "test",
            "session_id": "sess-1",
            "scope": "project/alpha",
            "timestamp": "2025-06-01T12:00:00Z"
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body["event_id"].as_i64().unwrap() > 0);
}

#[test]
fn ingest_invalid_event_type() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_ingest",
        args(json!({
            "event_type": "BogusType",
            "payload": {},
            "source": "test"
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

#[test]
fn ingest_missing_required_field() {
    let engine = make_engine();
    // Missing "source"
    let result = tools::dispatch(
        "memory_ingest",
        args(json!({
            "event_type": "Interaction",
            "payload": {}
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// 2. memory_add_fact
// ---------------------------------------------------------------------------

#[test]
fn add_fact_with_precomputed_embedding() {
    let engine = make_engine();
    stamp_identity(&engine);
    let emb = vec![0.1; DIM];
    let result = tools::dispatch(
        "memory_add_fact",
        args(json!({
            "content": "Rust is a systems language",
            "embedding": emb,
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

#[test]
fn add_fact_all_options() {
    let engine = make_engine();
    stamp_identity(&engine);
    let emb = vec![0.2; DIM];
    let result = tools::dispatch(
        "memory_add_fact",
        args(json!({
            "content": "Ebbinghaus forgetting curve",
            "fact_type": "Procedural",
            "scope": "research/memory",
            "importance": 0.9,
            "pinned": true,
            "metadata": {"source": "paper"},
            "t_valid": "2025-01-01T00:00:00Z",
            "t_invalid": "2026-01-01T00:00:00Z",
            "embedding": emb,
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

#[test]
fn add_fact_importance_out_of_range() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_add_fact",
        args(json!({
            "content": "test",
            "importance": 1.5,
            "embedding": vec![0.1; DIM],
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

#[test]
fn add_fact_temporal_inconsistency() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_add_fact",
        args(json!({
            "content": "test",
            "t_valid": "2026-01-01T00:00:00Z",
            "t_invalid": "2025-01-01T00:00:00Z",
            "embedding": vec![0.1; DIM],
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

#[test]
fn add_fact_wrong_embedding_dim() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_add_fact",
        args(json!({
            "content": "test",
            "embedding": vec![0.1; DIM + 5],
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

#[test]
fn add_fact_no_embedder_no_embedding() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_add_fact",
        args(json!({
            "content": "test without embedding",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    // Should fail: no pre-computed embedding and no HttpEmbeddingProvider
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// 3. memory_query
// ---------------------------------------------------------------------------

#[test]
fn query_fts_returns_results() {
    let engine = make_engine();
    let embedder = make_embedder();

    // Seed some facts
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust has zero-cost abstractions".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Python is great for ML".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "Rust",
            "mode": "fts",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body["count"].as_u64().unwrap() >= 1);
    assert!(!body["results"].as_array().unwrap().is_empty());
}

#[test]
fn query_with_precomputed_embedding() {
    let engine = make_engine();
    let embedder = make_embedder();

    engine
        .add_fact(
            &AddFactRequest {
                content: "Memory consolidation during sleep".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let query_emb = embedder.embed("sleep memory").unwrap();
    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "memory",
            "mode": "hybrid",
            "embedding": query_emb,
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body["count"].as_u64().unwrap() >= 1);
}

#[test]
fn query_one_sided_period_rejected() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "anything",
            "period_start": "2025-01-01T00:00:00Z",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

#[test]
fn query_empty_engine() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_query",
        args(json!({
            "text": "nothing here",
            "mode": "fts",
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert_eq!(body["count"].as_u64().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// 4. memory_resume_context
// ---------------------------------------------------------------------------

#[test]
fn resume_context_empty_engine() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_resume_context",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    // All tiers should be empty arrays
    assert!(body["pinned"].as_array().unwrap().is_empty());
    assert!(body["high_importance"].as_array().unwrap().is_empty());
    assert!(body["due"].as_array().unwrap().is_empty());
    assert!(body["recent"].as_array().unwrap().is_empty());
}

#[test]
fn resume_context_with_pinned_fact() {
    let engine = make_engine();
    let embedder = make_embedder();

    let opts = memory_engine::types::AddFactOptions {
        importance: Some(0.95),
        pinned: Some(true),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Critical system invariant".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = tools::dispatch(
        "memory_resume_context",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(!body["pinned"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// 5. memory_list_due
// ---------------------------------------------------------------------------

#[test]
fn list_due_empty() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_list_due",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert_eq!(body["count"].as_u64().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// 6. memory_next_due_time
// ---------------------------------------------------------------------------

#[test]
fn next_due_time_empty() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_next_due_time",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body["next_due"].is_null());
}

// ---------------------------------------------------------------------------
// 7. memory_explain_fact
// ---------------------------------------------------------------------------

#[test]
fn explain_fact_existing() {
    let engine = make_engine();
    let embedder = make_embedder();

    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Fact to explain".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = tools::dispatch(
        "memory_explain_fact",
        args(json!({ "fact_id": fact_id })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert_eq!(body["fact_id"].as_i64().unwrap(), fact_id);
}

#[test]
fn explain_fact_nonexistent() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_explain_fact",
        args(json!({ "fact_id": 9999 })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

#[test]
fn explain_fact_missing_id() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_explain_fact",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// 8. memory_get_fact
// ---------------------------------------------------------------------------

#[test]
fn get_fact_existing() {
    let engine = make_engine();
    let embedder = make_embedder();

    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Retrievable fact".into(),
                fact_type: memory_engine::types::FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert_eq!(body["id"].as_i64().unwrap(), fact_id);
    assert_eq!(body["content"].as_str().unwrap(), "Retrievable fact");
}

#[test]
fn get_fact_nonexistent() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": 9999 })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// 9. memory_statistics
// ---------------------------------------------------------------------------

#[test]
fn statistics_empty_engine() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_statistics",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    // Should return stats even on empty engine
    assert!(body.is_object());
}

#[test]
fn statistics_after_ingestion() {
    let engine = make_engine();
    let embedder = make_embedder();

    engine
        .add_fact(
            &AddFactRequest {
                content: "Fact 1".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Fact 2".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = tools::dispatch(
        "memory_statistics",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    let body = unwrap_ok(result);
    assert!(body.is_object());
}

// ---------------------------------------------------------------------------
// 10. memory_flush_insights
// ---------------------------------------------------------------------------

#[test]
fn flush_insights_no_embedder() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_flush_insights",
        args(json!({
            "insights": [
                {"content": "insight 1"},
            ]
        })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    // Requires embedder — should fail
    assert!(result.is_err());
}

#[test]
fn flush_insights_missing_array() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_flush_insights",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Unknown tool
// ---------------------------------------------------------------------------

#[test]
fn unknown_tool_returns_error() {
    let engine = make_engine();
    let result = tools::dispatch(
        "memory_nonexistent",
        args(json!({})),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

#[test]
fn all_tool_definitions_returns_26() {
    let defs = tools::all_tool_definitions();
    assert_eq!(
        defs.len(),
        26,
        "expected 10 P0 + 5 P1 + 3 P2 + 2 Phase 5a outcome + 3 activity stream + 3 cognitive (#225) tools"
    );
}

#[test]
fn all_tool_definitions_have_unique_names() {
    let defs = tools::all_tool_definitions();
    let names: Vec<&str> = defs.iter().map(|t| &*t.name).collect();
    let mut deduped = names.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(names.len(), deduped.len(), "tool names must be unique");
}
