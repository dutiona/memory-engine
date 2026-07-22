//! wiremock-based coverage for `HttpSummaryGenerator`'s response auto-detection.
//!
//! `HttpSummaryGenerator::summarize` POSTs to an `OpenAI`-compatible chat endpoint
//! and then auto-detects which of two response shapes it got back, plus two error
//! shapes. The four branches exercised here, one per test:
//!
//! 1. `OpenAI`:      `{ "choices": [{ "message": { "content": "..." } }] }` → `Ok`
//! 2. `Ollama`:      `{ "message": { "content": "..." } }`                  → `Ok`
//! 3. Embedded error: a 200 carrying `{ "error": { ... } }`                → `Err`
//! 4. Unknown shape:  a 200 matching neither extractor                     → `Err`
//!    (the error message includes the raw response body).
//!
//! `HttpSummaryGenerator` (like `HttpEmbeddingProvider`) wraps a
//! `reqwest::blocking::Client`, which builds its own internal tokio runtime. Both
//! constructing it and calling `summarize` from inside an async runtime would panic
//! ("cannot start a runtime from within a runtime"), so the whole construct-and-call
//! is offloaded onto `spawn_blocking` and only the `Result` is `.await`ed back — the
//! same discipline as `embedding_integration.rs`.

use memory_engine::error::MemoryError;
use memory_engine::traits::{SummarizableContent, SummaryGenerator};
use memory_engine_mcp::summary::HttpSummaryGenerator;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount a single POST `/v1/chat/completions` mock returning `body` with status 200,
/// then build an `HttpSummaryGenerator` aimed at it and run one `summarize` call.
///
/// Construction and the blocking call both happen on `spawn_blocking` (the blocking
/// reqwest client cannot live on the async runtime thread). One throwaway item is fed;
/// the parsing under test is independent of the request payload.
async fn summarize_against(body: serde_json::Value) -> Result<String, MemoryError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let endpoint = format!("{}/v1/chat/completions", server.uri());
    tokio::task::spawn_blocking(move || {
        let generator = HttpSummaryGenerator::new(endpoint, "test-model".to_string(), None, 5)
            .expect("client builds");
        let embedding = [0.0_f32; 4];
        let items = [SummarizableContent::new("alpha fact", &embedding)];
        generator.summarize(&items)
    })
    .await
    .expect("spawn_blocking join")
}

// ---------------------------------------------------------------------------
// Branch 1: OpenAI format — choices[0].message.content
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_format_extracts_content() {
    let result = summarize_against(json!({
        "choices": [{ "message": { "content": "summary text" } }]
    }))
    .await;

    assert_eq!(result.expect("OpenAI branch yields Ok"), "summary text");
}

// ---------------------------------------------------------------------------
// Branch 2: Ollama format — message.content
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ollama_format_extracts_content() {
    let result = summarize_against(json!({
        "message": { "content": "summary text" }
    }))
    .await;

    assert_eq!(result.expect("Ollama branch yields Ok"), "summary text");
}

// ---------------------------------------------------------------------------
// Branch 3: embedded API error in a 200 response — { "error": { ... } }
// ---------------------------------------------------------------------------

#[tokio::test]
async fn embedded_api_error_is_rejected() {
    let result = summarize_against(json!({
        "error": { "message": "rate limited" }
    }))
    .await;

    let err = result.expect_err("embedded-error branch yields Err");
    let msg = err.to_string();
    // `MemoryError::Internal` renders as "internal error: {0}"; the embedded-error
    // branch sets {0} = "summary API returned error: {serialized error object}".
    assert!(
        msg.contains("summary API returned error:"),
        "error message should name the embedded-API-error branch, got: {msg}"
    );
    // The serialized error object is forwarded so the operator sees the provider's reason.
    assert!(
        msg.contains("rate limited"),
        "error message should forward the provider's reason, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Branch 4: unknown shape (neither choices nor message) — Err includes the body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_shape_errors_with_body() {
    let result = summarize_against(json!({
        "unexpected": "payload"
    }))
    .await;

    let err = result.expect_err("unknown-shape branch yields Err");
    let msg = err.to_string();
    assert!(
        msg.contains("cannot extract summary from response:"),
        "error message should name the no-extractor branch, got: {msg}"
    );
    // The branch echoes the raw response body so the operator can see what arrived.
    assert!(
        msg.contains("\"unexpected\":\"payload\""),
        "error message should echo the unparseable response body, got: {msg}"
    );
}
