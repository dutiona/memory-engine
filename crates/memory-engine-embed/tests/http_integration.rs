//! wiremock-based end-to-end tests for `HttpEmbeddingProvider`'s public
//! `EmbeddingProvider` contract (single + batch happy paths, the failure each answer
//! maps to, and the response-body size cap).
//!
//! `HttpEmbeddingProvider` uses `reqwest::blocking::Client` (internal tokio
//! runtime), so all provider work runs inside `spawn_blocking` to avoid the
//! nested-runtime panic — mirroring `asymmetric_embedding.rs`.
#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::error::{EmbedderError, MemoryError};
use memory_engine::traits::EmbeddingProvider;
use memory_engine_embed::HttpEmbeddingProvider;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount a `/v1/embeddings` mock returning the given JSON body with a 200 status.
async fn mount_json(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Build a provider pointed at the mock server's `/v1/embeddings`.
fn provider_for(uri: &str, expected_dim: usize) -> HttpEmbeddingProvider {
    HttpEmbeddingProvider::new(
        format!("{uri}/v1/embeddings"),
        "m".into(),
        "tei".into(),
        None,
        expected_dim,
        5,
    )
    .expect("client build should not fail")
}

#[tokio::test]
async fn embed_oversized_response_is_rejected() {
    let server = MockServer::start().await;
    // A body larger than the 32 MiB cap. The cap must reject it rather than
    // buffer the whole thing into memory.
    let oversized = "x".repeat(33 * 1024 * 1024);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_string(oversized))
        .mount(&server)
        .await;
    let uri = server.uri();

    let result = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap();

    let err = result.expect_err("oversized body must be rejected");
    assert!(
        matches!(&err, MemoryError::Internal(m) if m.contains("too large")),
        "expected a too-large internal error, got: {err}"
    );
}

#[tokio::test]
async fn embed_batch_oversized_response_is_rejected() {
    let server = MockServer::start().await;
    let oversized = "x".repeat(33 * 1024 * 1024);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_string(oversized))
        .mount(&server)
        .await;
    let uri = server.uri();

    let result =
        tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed_batch(&["a", "b"]))
            .await
            .unwrap();

    let err = result.expect_err("oversized body must be rejected");
    assert!(
        matches!(&err, MemoryError::Internal(m) if m.contains("too large")),
        "expected a too-large internal error, got: {err}"
    );
}

#[tokio::test]
async fn embed_openai_format_roundtrip() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        json!({ "data": [{ "index": 0, "embedding": [0.1f32, 0.2, 0.3, 0.4] }] }),
    )
    .await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap()
        .expect("happy-path embed should succeed");
    assert_eq!(out, vec![0.1f32, 0.2, 0.3, 0.4]);
}

#[tokio::test]
async fn embed_batch_openai_multi_doc_happy_path() {
    let server = MockServer::start().await;
    // Two docs, returned out of index order to exercise the index-reorder path.
    mount_json(
        &server,
        json!({ "data": [
            { "index": 1, "embedding": [0.5f32, 0.6] },
            { "index": 0, "embedding": [0.1f32, 0.2] },
        ] }),
    )
    .await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || provider_for(&uri, 2).embed_batch(&["a", "b"]))
        .await
        .unwrap()
        .expect("happy-path batch should succeed");
    // Reordered by index: doc 0 first, doc 1 second.
    assert_eq!(out, vec![vec![0.1f32, 0.2], vec![0.5f32, 0.6]]);
}

#[tokio::test]
async fn embed_batch_ollama_multi_doc_happy_path() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        json!({ "embeddings": [[0.1f32, 0.2], [0.3f32, 0.4]] }),
    )
    .await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || provider_for(&uri, 2).embed_batch(&["a", "b"]))
        .await
        .unwrap()
        .expect("happy-path Ollama batch should succeed");
    assert_eq!(out, vec![vec![0.1f32, 0.2], vec![0.3f32, 0.4]]);
}

#[tokio::test]
async fn embed_http_500_is_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream model crashed"))
        .mount(&server)
        .await;
    let uri = server.uri();

    let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap()
        .expect_err("a 500 must propagate as an error");
    assert!(
        matches!(&err, MemoryError::Embedder(EmbedderError::Failed(m))
            if m.contains("500") && m.contains("upstream model crashed")),
        "expected the 500 status and body in a failed embed, got: {err}"
    );
}

#[tokio::test]
async fn embed_batch_http_500_is_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let uri = server.uri();

    let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed_batch(&["a", "b"]))
        .await
        .unwrap()
        .expect_err("a 500 must propagate as an error");
    assert!(
        matches!(&err, MemoryError::Embedder(EmbedderError::Failed(m))
            if m.contains("500") && m.contains("boom")),
        "expected the 500 status and body in a failed embed, got: {err}"
    );
}

/// What a retry can do about `err`, as the embedding queue reads it.
const fn kind(err: &MemoryError) -> &'static str {
    match err {
        MemoryError::Embedder(EmbedderError::Unreachable(_)) => "unreachable",
        MemoryError::Embedder(EmbedderError::KeyRejected(_)) => "key rejected",
        MemoryError::Embedder(EmbedderError::InputRejected(_)) => "input rejected",
        MemoryError::Embedder(EmbedderError::Failed(_)) => "failed",
        _ => "not an embedder error",
    }
}

#[tokio::test]
async fn a_status_maps_to_its_embedder_error() {
    for (status, want) in [
        (400, "input rejected"),
        (401, "key rejected"),
        (403, "key rejected"),
        (404, "failed"),
        (413, "input rejected"),
        (422, "input rejected"),
        (500, "failed"),
        (503, "failed"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(status).set_body_string("refused"))
            .mount(&server)
            .await;
        let uri = server.uri();
        let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
            .await
            .unwrap()
            .expect_err("a non-2xx must be an error");
        assert_eq!(kind(&err), want, "{status}: {err}");
        let text = err.to_string();
        assert!(
            text.contains(&format!("embedding endpoint returned {status}"))
                && text.contains("refused"),
            "{status}: {text}"
        );
    }
}

#[tokio::test]
async fn a_closed_port_is_unreachable() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let uri = format!("http://127.0.0.1:{port}");
    let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap()
        .expect_err("nothing listens on a closed port");
    assert_eq!(kind(&err), "unreachable", "{err}");
    assert!(
        err.to_string().contains("embedding HTTP request failed"),
        "{err}"
    );
}

#[tokio::test]
async fn an_answer_slower_than_the_timeout_is_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": [{ "embedding": [0.1, 0.2, 0.3, 0.4] }] }))
                .set_delay(std::time::Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    let err = tokio::task::spawn_blocking(move || {
        HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "tei".into(),
            None,
            4,
            1,
        )
        .unwrap()
        .embed("hello")
    })
    .await
    .unwrap()
    .expect_err("an answer after the timeout is no answer");
    assert_eq!(kind(&err), "failed", "{err}");
    assert!(
        err.to_string().contains("embedding HTTP request failed"),
        "{err}"
    );
}

#[tokio::test]
async fn embed_direct_format_roundtrip() {
    // `{"embedding": [...]}`, the shape some servers answer with.
    let server = MockServer::start().await;
    mount_json(&server, json!({ "embedding": [0.1, 0.2, 0.3, 0.4] })).await;
    let uri = server.uri();
    let emb = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("direct"))
        .await
        .unwrap()
        .expect("a direct-format response should parse");
    assert_eq!(emb.len(), 4);
}

#[tokio::test]
async fn embed_sends_the_bearer_api_key_when_configured() {
    // The only request the mock answers carries the configured key.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer test-key-123",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": [{ "embedding": [0.1, 0.2, 0.3, 0.4] }] })),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    let emb = tokio::task::spawn_blocking(move || {
        HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "tei".into(),
            Some("test-key-123".into()),
            4,
            5,
        )
        .unwrap()
        .embed("auth")
    })
    .await
    .unwrap()
    .expect("the keyed request should match the mock");
    assert_eq!(emb.len(), 4);
}
