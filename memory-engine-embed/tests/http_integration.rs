//! wiremock-based end-to-end tests for `HttpEmbeddingProvider`'s public
//! `EmbeddingProvider` contract (single + batch happy paths, HTTP error
//! propagation, and the response-body size cap).
//!
//! `HttpEmbeddingProvider` uses `reqwest::blocking::Client` (internal tokio
//! runtime), so all provider work runs inside `spawn_blocking` to avoid the
//! nested-runtime panic — mirroring `asymmetric_embedding.rs`.

use memory_engine::error::MemoryError;
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
async fn embed_http_500_propagates_as_internal_error() {
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
        matches!(&err, MemoryError::Internal(m)
            if m.contains("500") && m.contains("upstream model crashed")),
        "expected the 500 status and body in the error, got: {err}"
    );
}

#[tokio::test]
async fn embed_batch_http_500_propagates_as_internal_error() {
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
        matches!(&err, MemoryError::Internal(m) if m.contains("500") && m.contains("boom")),
        "expected the 500 status and body in the error, got: {err}"
    );
}
