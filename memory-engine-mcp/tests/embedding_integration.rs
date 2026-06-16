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

use memory_engine::MemoryEngine;
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "OpenAI format test" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Ollama format test" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Direct format test" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// flush_insights → get_recent_insights round-trip (#225)
// ---------------------------------------------------------------------------

/// Proves the writer (memory_flush_insights stamps the shared INSIGHT_MARKER_KEY)
/// connects to the reader (memory_get_recent_insights queries that marker).
#[tokio::test]
async fn flush_insights_then_get_recent_insights_roundtrip() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        // `index` is REQUIRED for the batch (`embed_batch`) parse path that
        // `flush_insights` exercises via `add_facts_batch` — the OpenAI batch
        // parser sorts by it for order-safety. (The single-embed path tolerates
        // its absence, which is why the other tests omit it.)
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "index": 0, "embedding": emb }]
        })))
        .mount(&server)
        .await;

    let uri = server.uri();
    let body = tokio::task::spawn_blocking(move || {
        let provider =
            HttpEmbeddingProvider::new(format!("{uri}/v1/embeddings"), "test-model".into(), None, DIM, 5)
                .unwrap();
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let cfg = memory_engine::ActivityFilterConfig::default();

        // Writer: flush an insight scoped to project:p (creates the scope, stamps the marker).
        unwrap_ok(tools::dispatch(
            "memory_flush_insights",
            args(json!({ "insights": [{ "content": "re-gate before merge", "scope": "project:p" }] })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &cfg,
        ));
        // Add a plain (non-insight) fact in the same scope — must be excluded.
        unwrap_ok(tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "ordinary fact", "scope": "project:p" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &cfg,
        ));
        // Reader.
        unwrap_ok(tools::dispatch(
            "memory_get_recent_insights",
            args(json!({ "project_path": "project:p" })),
            &engine,
            None,
            None,
            DIM,
            &cfg,
        ))
    })
    .await
    .unwrap();

    assert_eq!(
        body["count"].as_i64().unwrap(),
        1,
        "only the flushed insight is returned"
    );
    assert!(
        body["insights"][0]["content"]
            .as_str()
            .unwrap()
            .contains("re-gate")
    );
}

/// Two insights flushed in one batch must BOTH be readable — guards against a
/// batch-indexing bug that drops or reorders the second item (the single-insight
/// roundtrip above cannot catch that).
#[tokio::test]
async fn flush_two_insights_then_get_recent_returns_both() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "index": 0, "embedding": emb.clone() },
                { "index": 1, "embedding": emb }
            ]
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let cfg = memory_engine::ActivityFilterConfig::default();

        unwrap_ok(tools::dispatch(
            "memory_flush_insights",
            args(json!({ "insights": [
                { "content": "insight alpha", "scope": "project:p" },
                { "content": "insight beta", "scope": "project:p" }
            ] })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &cfg,
        ));
        unwrap_ok(tools::dispatch(
            "memory_get_recent_insights",
            args(json!({ "project_path": "project:p" })),
            &engine,
            None,
            None,
            DIM,
            &cfg,
        ))
    })
    .await
    .unwrap();

    assert_eq!(
        body["count"].as_i64().unwrap(),
        2,
        "both flushed insights must be readable"
    );
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Dim mismatch test" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Server error test" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let result = tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Auth test" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
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

#[tokio::test(flavor = "multi_thread")]
async fn flush_insights_with_http_embedder() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    // Batch embed: one call with both texts, response needs index fields
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "index": 0, "embedding": emb.clone() },
                { "index": 1, "embedding": emb }
            ]
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
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
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert_eq!(body["added"].as_u64().unwrap(), 2);
    assert_eq!(body["failed_count"].as_u64().unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn flush_insights_partial_failure() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);

    // Only 1 valid insight passes pre-validation → batch embed with 1 text
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "index": 0, "embedding": emb }]
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
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
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert_eq!(body["added"].as_u64().unwrap(), 1);
    assert_eq!(body["failed_count"].as_u64().unwrap(), 2);
}

/// A present-but-non-object `metadata` is rejected per-entry into `failed` (not
/// silently coerced to `{}`), while a sibling insight with valid metadata still flushes.
#[tokio::test(flavor = "multi_thread")]
async fn flush_insights_non_object_metadata_is_rejected() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);
    // Only the one valid insight reaches the batch embed → 1 indexed embedding.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "index": 0, "embedding": emb }]
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let result = tools::dispatch(
            "memory_flush_insights",
            args(json!({
                "insights": [
                    { "content": "valid", "metadata": { "k": "v" } },
                    { "content": "bad meta", "metadata": "not-an-object" }
                ]
            })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert_eq!(body["added"].as_u64().unwrap(), 1);
    assert_eq!(body["failed_count"].as_u64().unwrap(), 1);
    assert_eq!(body["failed"][0]["index"], json!(1));
    assert!(
        body["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("metadata must be an object")
    );
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();

        // Add a fact with pre-computed embedding (no provider needed)
        tools::dispatch(
            "memory_add_fact",
            args(json!({
                "content": "Tokio async runtime",
                "embedding": emb_clone,
            })),
            &engine,
            None,
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        )
        .unwrap();

        // Query using server-side embedding
        let result = tools::dispatch(
            "memory_query",
            args(json!({ "text": "async runtime", "mode": "hybrid" })),
            &engine,
            Some(&provider),
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        );
        unwrap_ok(result)
    })
    .await
    .unwrap();
    assert!(body["count"].as_u64().unwrap() >= 1);
}
