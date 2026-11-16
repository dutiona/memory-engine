//! A stop drains the tool calls already started, then flushes the sidecar snapshot.

mod common;

use std::time::{Duration, Instant};

use common::{DIM, TestServer};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

async fn slow_embedder(server: &TestServer, delay: Duration) {
    server.embedder.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": [{ "embedding": vec![0.25_f32; DIM] }] }))
                .set_delay(delay),
        )
        .mount(&server.embedder)
        .await;
}

async fn wait_for_embed_request(server: &TestServer) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while server
        .embedder
        .received_requests()
        .await
        .unwrap()
        .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "the embedder never received the call"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn add_fact(server: &TestServer) -> reqwest::RequestBuilder {
    server.rpc(
        Some("laptop"),
        "tools/call",
        &json!({ "name": "memory_add_fact", "arguments": { "content": "in flight" } }),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_lets_an_in_flight_call_finish() {
    let mut server = TestServer::start().await;
    slow_embedder(&server, Duration::from_millis(1500)).await;
    let call = tokio::spawn(add_fact(&server).send());
    wait_for_embed_request(&server).await;

    server.stop().await;

    let response = call.await.unwrap().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(server.fact_count().await, 1);
    assert!(server.snapshot_path().is_file());
    assert!(
        reqwest::Client::new()
            .get(format!("{}/mcp", server.url))
            .send()
            .await
            .is_err(),
        "the listener still accepts connections after a stop"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_waits_for_an_abandoned_call() {
    let mut server = TestServer::start().await;
    slow_embedder(&server, Duration::from_secs(2)).await;
    let call = tokio::spawn(add_fact(&server).send());
    wait_for_embed_request(&server).await;
    // The client hangs up mid-call; rmcp keeps running the call in its own task.
    call.abort();
    let _ = call.await;

    server.stop().await;

    assert_eq!(
        server.fact_count().await,
        1,
        "the flush ran before the call committed"
    );
    assert!(server.snapshot_path().is_file());
}
