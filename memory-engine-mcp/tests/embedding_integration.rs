//! wiremock-based HTTP embedding round-trip tests.
//!
//! Validates `HttpEmbeddingProvider` against all three supported response
//! formats (`OpenAI`, Ollama, direct) and exercises end-to-end embedding via
//! `memory_add_fact` and `memory_flush_insights`.
//!
//! `HttpEmbeddingProvider` uses `reqwest::blocking::Client` which creates an
//! internal tokio runtime. To avoid nested-runtime panics, the provider must
//! be created AND dropped on the blocking thread pool. After async wiremock
//! setup, all provider work runs inside a single `spawn_blocking` block.

use memory_engine::engine::MemoryEngine;
use memory_engine_mcp::embedding::HttpEmbeddingProvider;
use memory_engine_mcp::tools;
use serde_json::{Map, Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIM: usize = 4;

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

fn make_embedding(dim: usize) -> Vec<f32> {
    vec![0.25; dim]
}

// ---------------------------------------------------------------------------
// OpenAI response format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_format_embedding() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": emb }],
            "model": "test-model",
            "usage": { "prompt_tokens": 5, "total_tokens": 5 }
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let body = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "test-model".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "OpenAI format test" })),
            &engine,
            Some(&provider),
            DIM,
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// Ollama response format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ollama_format_embedding() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embeddings": [emb]
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let body = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "nomic-embed-text".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Ollama format test" })),
            &engine,
            Some(&provider),
            DIM,
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// Direct (single embedding) response format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn direct_format_embedding() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embedding": emb
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let body = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "custom-model".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Direct format test" })),
            &engine,
            Some(&provider),
            DIM,
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// Dimension mismatch from remote server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wrong_dimension_from_server() {
    let server = MockServer::start().await;
    let wrong_emb = vec![0.1; DIM + 3]; // wrong dimension

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": wrong_emb }]
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let is_err = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "test-model".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Dim mismatch test" })),
            &engine,
            Some(&provider),
            DIM,
        )
        .is_err()
    })
    .await
    .unwrap();
    assert!(is_err);
}

// ---------------------------------------------------------------------------
// Server error propagation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn server_500_propagates_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;

    let uri = server.uri();
    let is_err = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "test-model".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Server error test" })),
            &engine,
            Some(&provider),
            DIM,
        )
        .is_err()
    })
    .await
    .unwrap();
    assert!(is_err);
}

// ---------------------------------------------------------------------------
// Bearer auth header
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_auth_sent_when_configured() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer test-key-123",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": emb }]
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let body = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "test-model".into(),
            Some("test-key-123".into()),
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Auth test" })),
            &engine,
            Some(&provider),
            DIM,
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// flush_insights with wiremock embedder
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flush_insights_with_http_embedder() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": emb }]
        })))
        .expect(2) // Two insights = two embedding calls
        .mount(&server)
        .await;

    let uri = server.uri();
    let body = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "test-model".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let result = tools::dispatch(
            "memory_flush_insights",
            args(json!({
                "insights": [
                    { "content": "Insight one", "importance": 0.8 },
                    { "content": "Insight two", "fact_type": "Procedural" }
                ]
            })),
            &engine,
            Some(&provider),
            DIM,
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert_eq!(body["added"].as_u64().unwrap(), 2);
    assert_eq!(body["failed_count"].as_u64().unwrap(), 0);
}

#[tokio::test]
async fn flush_insights_partial_failure() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": emb }]
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let body = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "test-model".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let result = tools::dispatch(
            "memory_flush_insights",
            args(json!({
                "insights": [
                    { "content": "Good insight" },
                    "not an object",
                    { "no_content_field": true }
                ]
            })),
            &engine,
            Some(&provider),
            DIM,
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert_eq!(body["added"].as_u64().unwrap(), 1);
    assert_eq!(body["failed_count"].as_u64().unwrap(), 2);
}

// ---------------------------------------------------------------------------
// Query with server-side embedding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_hybrid_with_http_embedder() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": emb }]
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let emb_clone = emb;
    let body = tokio::task::spawn_blocking(move || {
        let provider = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "test-model".into(),
            None,
            DIM,
            5,
        )
        .unwrap();
        let engine = MemoryEngine::open_memory(DIM).unwrap();

        // Add a fact with pre-computed embedding (no provider needed)
        tools::dispatch(
            "memory_add_fact",
            args(json!({
                "content": "Tokio async runtime",
                "embedding": emb_clone,
            })),
            &engine,
            None,
            DIM,
        )
        .unwrap();

        // Query using server-side embedding
        let result = tools::dispatch(
            "memory_query",
            args(json!({ "text": "async runtime", "mode": "hybrid" })),
            &engine,
            Some(&provider),
            DIM,
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert!(body["count"].as_u64().unwrap() >= 1);
}
