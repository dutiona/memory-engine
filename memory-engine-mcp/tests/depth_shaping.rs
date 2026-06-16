//! Insta snapshot tests for depth-shaping at each tier (sparse/standard/full).
//!
//! Validates that the JSON shape returned by each `shape_*` function matches
//! the documented contract: sparse ~4 fields, standard ~15, full includes
//! `embedding_dim` and `content_hash`.

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
}

fn make_engine() -> MemoryEngine {
    MemoryEngine::builder(DIM)
        .build()
        .expect("in-memory engine")
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

/// Redact volatile fields (timestamps, IDs) for stable snapshots.
fn redact(v: &mut Value) {
    if let Value::Object(map) = v {
        for key in [
            "t_created",
            "t_expired",
            "t_valid",
            "t_invalid",
            "last_accessed",
            "surfaced_at",
        ] {
            if map.contains_key(key) {
                map.insert(key.to_owned(), json!("[REDACTED]"));
            }
        }
        for val in map.values_mut() {
            redact(val);
        }
    } else if let Value::Array(arr) = v {
        for val in arr.iter_mut() {
            redact(val);
        }
    }
}

// ---------------------------------------------------------------------------
// memory_get_fact — fact shaping at all three depths
// ---------------------------------------------------------------------------

#[test]
fn get_fact_sparse() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is a systems programming language with zero-cost abstractions"
                    .into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id, "depth": "sparse" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("get_fact_sparse", body);
}

#[test]
fn get_fact_standard() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is a systems programming language with zero-cost abstractions"
                    .into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id, "depth": "standard" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("get_fact_standard", body);
}

#[test]
fn get_fact_full() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is a systems programming language with zero-cost abstractions"
                    .into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id, "depth": "full" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("get_fact_full", body);
}

// ---------------------------------------------------------------------------
// memory_explain_fact — explanation shaping at all three depths
// ---------------------------------------------------------------------------

#[test]
fn explain_fact_sparse() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Ebbinghaus forgetting curve".into(),
                fact_type: memory_engine::types::FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_explain_fact",
        args(json!({ "fact_id": fact_id, "depth": "sparse" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("explain_fact_sparse", body);
}

#[test]
fn explain_fact_standard() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Ebbinghaus forgetting curve".into(),
                fact_type: memory_engine::types::FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_explain_fact",
        args(json!({ "fact_id": fact_id, "depth": "standard" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("explain_fact_standard", body);
}

#[test]
fn explain_fact_full() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Ebbinghaus forgetting curve".into(),
                fact_type: memory_engine::types::FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_explain_fact",
        args(json!({ "fact_id": fact_id, "depth": "full" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("explain_fact_full", body);
}

// ---------------------------------------------------------------------------
// memory_query — search result shaping at all three depths
// ---------------------------------------------------------------------------

#[test]
fn query_result_sparse() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Neural networks learn representations".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_query",
        args(json!({ "text": "neural", "mode": "fts", "depth": "sparse" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("query_result_sparse", body);
}

#[test]
fn query_result_standard() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Neural networks learn representations".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_query",
        args(json!({ "text": "neural", "mode": "fts", "depth": "standard" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("query_result_standard", body);
}

#[test]
fn query_result_full() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Neural networks learn representations".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_query",
        args(json!({ "text": "neural", "mode": "fts", "depth": "full" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("query_result_full", body);
}

// ---------------------------------------------------------------------------
// memory_resume_context — resume shaping at all three depths
// ---------------------------------------------------------------------------

#[test]
fn resume_context_sparse() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let opts = memory_engine::types::AddFactOptions {
        importance: Some(0.95),
        pinned: Some(true),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Critical pinned fact".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_resume_context",
        args(json!({ "depth": "sparse" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("resume_context_sparse", body);
}

#[test]
fn resume_context_standard() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let opts = memory_engine::types::AddFactOptions {
        importance: Some(0.95),
        pinned: Some(true),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Critical pinned fact".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_resume_context",
        args(json!({ "depth": "standard" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("resume_context_standard", body);
}

#[test]
fn resume_context_full() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let opts = memory_engine::types::AddFactOptions {
        importance: Some(0.95),
        pinned: Some(true),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Critical pinned fact".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut body = unwrap_ok(tools::dispatch(
        "memory_resume_context",
        args(json!({ "depth": "full" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("resume_context_full", body);
}

// ---------------------------------------------------------------------------
// memory_list_due — list shaping at all three depths
// ---------------------------------------------------------------------------

#[test]
fn list_due_sparse() {
    let engine = make_engine();
    let mut body = unwrap_ok(tools::dispatch(
        "memory_list_due",
        args(json!({ "depth": "sparse" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    redact(&mut body);
    insta::assert_yaml_snapshot!("list_due_sparse", body);
}

// ---------------------------------------------------------------------------
// Depth field-count assertions (structural, not snapshot)
// ---------------------------------------------------------------------------

#[test]
fn sparse_has_exactly_4_fields() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Field count test".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let body = unwrap_ok(tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id, "depth": "sparse" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    assert_eq!(body.as_object().unwrap().len(), 4);
}

#[test]
fn standard_excludes_embedding_and_hash() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Field exclusion test".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let body = unwrap_ok(tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id, "depth": "standard" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    let obj = body.as_object().unwrap();
    assert!(!obj.contains_key("embedding"));
    assert!(!obj.contains_key("content_hash"));
    assert!(!obj.contains_key("embedding_dim"));
}

#[test]
fn full_includes_embedding_dim_and_hash() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Full depth test".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let body = unwrap_ok(tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id, "depth": "full" })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    let obj = body.as_object().unwrap();
    assert!(obj.contains_key("content_hash"));
    assert!(obj.contains_key("embedding_dim"));
    assert_eq!(obj["embedding_dim"], DIM);
}

// ---------------------------------------------------------------------------
// Default depth is standard
// ---------------------------------------------------------------------------

#[test]
fn default_depth_is_standard() {
    let engine = make_engine();
    let embedder = TestEmbedder { dim: DIM };
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Default depth test".into(),
                fact_type: memory_engine::types::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // No depth parameter → default (standard)
    let body = unwrap_ok(tools::dispatch(
        "memory_get_fact",
        args(json!({ "fact_id": fact_id })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    ));
    let obj = body.as_object().unwrap();
    // Standard has content + fact_type but no embedding_dim
    assert!(obj.contains_key("fact_type"));
    assert!(!obj.contains_key("embedding_dim"));
}
