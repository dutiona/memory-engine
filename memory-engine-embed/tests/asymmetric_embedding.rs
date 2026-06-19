//! wiremock-based end-to-end tests for #617: asymmetric query embedding
//! (`embed_query*` instruction prefix) and Matryoshka (MRL) truncation.
//!
//! `HttpEmbeddingProvider` uses `reqwest::blocking::Client` (internal tokio
//! runtime), so all provider work runs inside `spawn_blocking` to avoid the
//! nested-runtime panic — mirroring `proposer_integration.rs`. The request body
//! the provider actually sent is inspected afterwards via `received_requests()`.

use memory_engine::traits::EmbeddingProvider;
use memory_engine_embed::HttpEmbeddingProvider;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount a `/v1/embeddings` mock returning the given OpenAI-format embeddings
/// (one `data` item per row, in order).
async fn mount_embeddings(server: &MockServer, rows: Vec<Vec<f32>>) {
    let data: Vec<_> = rows
        .into_iter()
        .enumerate()
        .map(|(i, emb)| json!({ "index": i, "embedding": emb }))
        .collect();
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": data })))
        .mount(server)
        .await;
}

/// The JSON `input` field of the single request the provider sent.
async fn sole_request_input(server: &MockServer) -> serde_json::Value {
    let reqs = server.received_requests().await.expect("recording enabled");
    assert_eq!(reqs.len(), 1, "expected exactly one HTTP request");
    let body: serde_json::Value =
        serde_json::from_slice(&reqs[0].body).expect("request body is JSON");
    body.get("input")
        .cloned()
        .expect("request has an 'input' field")
}

#[tokio::test]
async fn embed_query_prepends_instruction() {
    let server = MockServer::start().await;
    mount_embeddings(&server, vec![vec![0.1, 0.2, 0.3, 0.4]]).await;
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let p = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "tei".into(),
            None,
            4,
            5,
        )
        .unwrap()
        .with_query_instruction("PREFIX::");
        p.embed_query("hello").unwrap();
    })
    .await
    .unwrap();

    // The query path must send the prefixed text.
    assert_eq!(sole_request_input(&server).await, json!("PREFIX::hello"));
}

#[tokio::test]
async fn embed_document_has_no_prefix() {
    let server = MockServer::start().await;
    mount_embeddings(&server, vec![vec![0.1, 0.2, 0.3, 0.4]]).await;
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let p = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "tei".into(),
            None,
            4,
            5,
        )
        .unwrap()
        .with_query_instruction("PREFIX::");
        // Documents go through `embed`, which must NOT prepend the query prefix.
        p.embed("hello").unwrap();
    })
    .await
    .unwrap();

    assert_eq!(sole_request_input(&server).await, json!("hello"));
}

#[tokio::test]
async fn embed_query_batch_prepends_to_each() {
    let server = MockServer::start().await;
    mount_embeddings(
        &server,
        vec![vec![0.1, 0.2, 0.3, 0.4], vec![0.5, 0.6, 0.7, 0.8]],
    )
    .await;
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let p = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "tei".into(),
            None,
            4,
            5,
        )
        .unwrap()
        .with_query_instruction("Q: ");
        p.embed_query_batch(&["a", "b"]).unwrap();
    })
    .await
    .unwrap();

    assert_eq!(sole_request_input(&server).await, json!(["Q: a", "Q: b"]));
}

#[tokio::test]
async fn mrl_truncation_end_to_end() {
    let server = MockServer::start().await;
    // Server returns a native 4-dim vector; provider truncates to 2 and renormalizes.
    mount_embeddings(&server, vec![vec![3.0, 4.0, 99.0, 99.0]]).await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || {
        let p = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "Qwen/Qwen3-Embedding-0.6B".into(),
            "tei".into(),
            None,
            4, // native dim validated against the raw response
            5,
        )
        .unwrap()
        .with_mrl_dim(2)
        .unwrap();
        p.embed("doc").unwrap()
    })
    .await
    .unwrap();

    // [3, 4] L2-normalized -> [0.6, 0.8]; the 99s are truncated away.
    assert_eq!(out.len(), 2);
    assert!((out[0] - 0.6).abs() < 1e-6, "got {}", out[0]);
    assert!((out[1] - 0.8).abs() < 1e-6, "got {}", out[1]);
}

#[tokio::test]
async fn embed_query_applies_both_prefix_and_mrl() {
    let server = MockServer::start().await;
    mount_embeddings(&server, vec![vec![3.0, 4.0, 0.0, 0.0]]).await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || {
        let p = HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "Qwen/Qwen3-Embedding-0.6B".into(),
            "tei".into(),
            None,
            4,
            5,
        )
        .unwrap()
        .with_query_instruction("Instruct: ")
        .with_mrl_dim(2)
        .unwrap();
        p.embed_query("find this").unwrap()
    })
    .await
    .unwrap();

    // Prefix went out on the wire...
    assert_eq!(
        sole_request_input(&server).await,
        json!("Instruct: find this")
    );
    // ...and the response was MRL-truncated + renormalized ([3,4] -> [0.6, 0.8]).
    assert_eq!(out.len(), 2);
    assert!((out[0] - 0.6).abs() < 1e-6, "got {}", out[0]);
    assert!((out[1] - 0.8).abs() < 1e-6, "got {}", out[1]);
}
