//! wiremock-based HTTP embedding round-trip tests.
//!
//! Validates `HttpEmbeddingProvider` against all three supported response
//! formats (`OpenAI`, Ollama, direct) and exercises end-to-end embedding via
//! `memory_add_fact` and `memory_flush_insights`.
//!
//! `HttpEmbeddingProvider` uses `reqwest::blocking::Client`, which builds an
//! internal tokio runtime at construction. Building it inside an async runtime
//! would panic ("cannot start a runtime from within a runtime"), so the provider
//! (and the engine, alongside it) is constructed on the blocking thread pool and
//! handed back as a `Send` value. The now-async `tools::dispatch` is then
//! `.await`ed directly on the test runtime (#631): the engine offloads every
//! `reqwest::blocking` consumer-trait call into its own `spawn_blocking`, so the
//! actual HTTP embed never runs on — nor panics from — the async runtime thread.

use std::sync::Arc;

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

/// Convenience filter config used by every dispatch in this file.
fn cfg() -> memory_engine::ActivityFilterConfig {
    memory_engine::ActivityFilterConfig::default()
}

/// Build the `HttpEmbeddingProvider` (and a fresh in-process engine) on the
/// blocking thread pool, returning them as `Send` values for the async test body.
///
/// `HttpEmbeddingProvider::new` builds a `reqwest::blocking::Client`, which would
/// panic if constructed inside the async runtime — so construction stays on
/// `spawn_blocking`. The returned provider is `Arc`-wrapped because the now-async
/// engine takes consumer traits as owned `Arc<dyn _>` (#631 §1.2).
async fn build_engine_and_provider(
    endpoint: String,
    model: &str,
    provider: &str,
    api_key: Option<String>,
    query_instruction: Option<String>,
) -> (MemoryEngine, Arc<HttpEmbeddingProvider>) {
    let model = model.to_string();
    let provider = provider.to_string();
    tokio::task::spawn_blocking(move || {
        let mut p = HttpEmbeddingProvider::new(endpoint, model, provider, api_key, DIM, 5).unwrap();
        if let Some(instruction) = query_instruction {
            p = p.with_query_instruction(instruction);
        }
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        (engine, Arc::new(p))
    })
    .await
    .unwrap()
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;
    let body = unwrap_ok(
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "OpenAI format test" })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "nomic-embed-text",
        "test-provider",
        None,
        None,
    )
    .await;
    let body = unwrap_ok(
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Ollama format test" })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "custom-model",
        "test-provider",
        None,
        None,
    )
    .await;
    let body = unwrap_ok(
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Direct format test" })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
    assert!(body["fact_id"].as_i64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// flush_insights → get_recent_insights round-trip (#225)
// ---------------------------------------------------------------------------

/// Proves the writer (`memory_flush_insights` stamps the shared `INSIGHT_MARKER_KEY`)
/// connects to the reader (`memory_get_recent_insights` queries that marker).
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;

    // Writer: flush an insight scoped to project:p (creates the scope, stamps the marker).
    unwrap_ok(
        tools::dispatch(
            "memory_flush_insights",
            args(json!({ "insights": [{ "content": "re-gate before merge", "scope": "project:p" }] })),
            &engine,
            Some(Arc::clone(&provider)),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
    // Add a plain (non-insight) fact in the same scope — must be excluded.
    unwrap_ok(
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "ordinary fact", "scope": "project:p" })),
            &engine,
            Some(Arc::clone(&provider)),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
    // Reader.
    let body = unwrap_ok(
        tools::dispatch(
            "memory_get_recent_insights",
            args(json!({ "project_path": "project:p" })),
            &engine,
            None,
            None,
            DIM,
            &cfg(),
        )
        .await,
    );

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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;

    unwrap_ok(
        tools::dispatch(
            "memory_flush_insights",
            args(json!({ "insights": [
                { "content": "insight alpha", "scope": "project:p" },
                { "content": "insight beta", "scope": "project:p" }
            ] })),
            &engine,
            Some(Arc::clone(&provider)),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
    let body = unwrap_ok(
        tools::dispatch(
            "memory_get_recent_insights",
            args(json!({ "project_path": "project:p" })),
            &engine,
            None,
            None,
            DIM,
            &cfg(),
        )
        .await,
    );

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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;
    let is_err = tools::dispatch(
        "memory_add_fact",
        args(json!({ "content": "Dim mismatch test" })),
        &engine,
        Some(provider),
        None,
        DIM,
        &cfg(),
    )
    .await
    .is_err();
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;
    let is_err = tools::dispatch(
        "memory_add_fact",
        args(json!({ "content": "Server error test" })),
        &engine,
        Some(provider),
        None,
        DIM,
        &cfg(),
    )
    .await
    .is_err();
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        Some("test-key-123".into()),
        None,
    )
    .await;
    let body = unwrap_ok(
        tools::dispatch(
            "memory_add_fact",
            args(json!({ "content": "Auth test" })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;
    let body = unwrap_ok(
        tools::dispatch(
            "memory_flush_insights",
            args(json!({
                "insights": [
                    { "content": "Insight one", "importance": 0.8 },
                    { "content": "Insight two", "fact_type": "Procedural" }
                ]
            })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;
    let body = unwrap_ok(
        tools::dispatch(
            "memory_flush_insights",
            args(json!({
                "insights": [
                    { "content": "Good insight" },
                    "not an object",
                    { "no_content_field": true }
                ]
            })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;
    let body = unwrap_ok(
        tools::dispatch(
            "memory_flush_insights",
            args(json!({
                "insights": [
                    { "content": "valid", "metadata": { "k": "v" } },
                    { "content": "bad meta", "metadata": "not-an-object" }
                ]
            })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
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

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "test-model",
        "test-provider",
        None,
        None,
    )
    .await;

    // Add a fact via the HTTP embedder — a real-embedder write that also stamps the
    // store's embedding identity (so the subsequent query has an identity to match).
    tools::dispatch(
        "memory_add_fact",
        args(json!({ "content": "Tokio async runtime" })),
        &engine,
        Some(Arc::clone(&provider)),
        None,
        DIM,
        &cfg(),
    )
    .await
    .unwrap();

    // Query using server-side embedding
    let body = unwrap_ok(
        tools::dispatch(
            "memory_query",
            args(json!({ "text": "async runtime", "mode": "hybrid" })),
            &engine,
            Some(provider),
            None,
            DIM,
            &cfg(),
        )
        .await,
    );
    assert!(body["count"].as_u64().unwrap() >= 1);
}

// ---------------------------------------------------------------------------
// Asymmetric query path (#618): memory_query uses embed_query (prefix applied),
// memory_add_fact stays on the document embed (no prefix).
// ---------------------------------------------------------------------------

/// Parse the JSON `input` field of the single request the mock server received.
async fn sole_input(server: &MockServer) -> Value {
    let reqs = server.received_requests().await.expect("recording enabled");
    assert_eq!(reqs.len(), 1, "expected exactly one embedding request");
    let body: Value = serde_json::from_slice(&reqs[0].body).expect("request body is JSON");
    body.get("input")
        .cloned()
        .expect("request has an 'input' field")
}

#[tokio::test]
async fn query_path_applies_query_instruction_prefix() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "index": 0, "embedding": emb }]
        })))
        .mount(&server)
        .await;

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "Qwen/Qwen3-Embedding-0.6B",
        "tei",
        None,
        Some("Q-PREFIX: ".to_string()),
    )
    .await;
    // A vector-mode query with text forces server-side query embedding.
    let _ = tools::dispatch(
        "memory_query",
        args(json!({ "text": "search terms", "mode": "vector" })),
        &engine,
        Some(provider),
        None,
        DIM,
        &cfg(),
    )
    .await;

    // The query embedding request must carry the instruction prefix.
    assert_eq!(sole_input(&server).await, json!("Q-PREFIX: search terms"));
}

#[tokio::test]
async fn add_fact_path_does_not_apply_query_instruction() {
    let server = MockServer::start().await;
    let emb = make_embedding(DIM);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "index": 0, "embedding": emb }]
        })))
        .mount(&server)
        .await;

    let (engine, provider) = build_engine_and_provider(
        format!("{}/v1/embeddings", server.uri()),
        "Qwen/Qwen3-Embedding-0.6B",
        "tei",
        None,
        Some("Q-PREFIX: ".to_string()),
    )
    .await;
    // add_fact embeds the document via engine.add_fact -> embed (no prefix).
    let _ = tools::dispatch(
        "memory_add_fact",
        args(json!({ "content": "document text" })),
        &engine,
        Some(provider),
        None,
        DIM,
        &cfg(),
    )
    .await;

    // Documents are embedded prefix-free even when a query instruction is configured.
    assert_eq!(sole_input(&server).await, json!("document text"));
}
