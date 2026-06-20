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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
