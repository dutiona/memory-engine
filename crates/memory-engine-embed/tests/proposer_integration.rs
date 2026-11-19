//! End-to-end tests for [`HttpDeltaProposer`] against a wiremock Ollama
//! `/api/generate` endpoint.
//!
//! `HttpDeltaProposer` uses `reqwest::blocking::Client`, which creates its own
//! runtime; calling it from inside the test's tokio runtime would panic. So all
//! provider work runs inside a single `spawn_blocking` block (mirroring the
//! `HttpEmbeddingProvider` integration tests).

use memory_engine::traits::DeltaProposer;
use memory_engine_embed::HttpDeltaProposer;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn propose_round_trip_returns_proposal_and_captures_token_stats() {
    let server = MockServer::start().await;
    let response_body = serde_json::json!({
        "model": "gemma4:26b",
        "response": r#"{"merges":[{"source_ids":[1,2],"summary":"merged a+b"}]}"#,
        "done": true,
        "eval_count": 30,
        "prompt_eval_count": 12,
    });
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(1)
        .mount(&server)
        .await;
    let url = format!("{}/api/generate", server.uri());

    let (proposal, stats) = tokio::task::spawn_blocking(move || {
        let proposer =
            HttpDeltaProposer::new(url, "gemma4:26b".to_string(), None, 10).expect("client build");
        let proposal = proposer.propose(&[], &[]).expect("propose");
        (proposal, proposer.stats())
    })
    .await
    .expect("spawn_blocking join");

    assert_eq!(proposal.merges.len(), 1);
    assert_eq!(proposal.merges[0].source_ids, vec![1, 2]);
    assert_eq!(proposal.merges[0].summary, "merged a+b");
    assert_eq!(stats.llm_calls, 1);
    assert_eq!(stats.eval_count, 30);
    assert_eq!(stats.prompt_eval_count, 12);
}

#[tokio::test]
async fn propose_records_call_and_tokens_even_when_merge_json_is_malformed() {
    // A 200 response whose inner merge JSON is broken still cost a real LLM call and
    // tokens — the benchmark must see them, so stats are recorded before the parse.
    let server = MockServer::start().await;
    let response_body = serde_json::json!({
        "model": "gemma4:26b",
        "response": "not valid json {{{",
        "done": true,
        "eval_count": 25,
        "prompt_eval_count": 8,
    });
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .mount(&server)
        .await;
    let url = format!("{}/api/generate", server.uri());

    let stats = tokio::task::spawn_blocking(move || {
        let proposer =
            HttpDeltaProposer::new(url, "gemma4:26b".to_string(), None, 10).expect("client build");
        let err = proposer.propose(&[], &[]);
        assert!(
            err.is_err(),
            "malformed merge JSON must surface as an error"
        );
        proposer.stats()
    })
    .await
    .expect("join");

    assert_eq!(
        stats.llm_calls, 1,
        "the call is counted despite the parse failure"
    );
    assert_eq!(stats.eval_count, 25);
    assert_eq!(stats.prompt_eval_count, 8);
}

#[tokio::test]
async fn propose_surfaces_http_error_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(500).set_body_string("model not loaded"))
        .mount(&server)
        .await;
    let url = format!("{}/api/generate", server.uri());

    let err = tokio::task::spawn_blocking(move || {
        let proposer =
            HttpDeltaProposer::new(url, "gemma4:26b".to_string(), None, 10).expect("client build");
        proposer.propose(&[], &[])
    })
    .await
    .expect("spawn_blocking join")
    .expect_err("a 500 must surface as an error");

    assert!(
        err.to_string().contains("500"),
        "error should carry the status: {err}"
    );
}

/// One byte over the crate's 32 MiB response cap.
const OVER_CAP: usize = 32 * 1024 * 1024 + 1;

async fn propose_against(status: u16) -> memory_engine::MemoryError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(status).set_body_string("x".repeat(OVER_CAP)))
        .mount(&server)
        .await;
    let url = format!("{}/api/generate", server.uri());
    tokio::task::spawn_blocking(move || {
        let proposer =
            HttpDeltaProposer::new(url, "gemma4:26b".to_string(), None, 30).expect("client build");
        proposer
            .propose(&[], &[])
            .expect_err("an over-cap response is refused")
    })
    .await
    .expect("spawn_blocking join")
}

#[tokio::test]
async fn an_over_cap_success_response_is_refused_unread() {
    let err = propose_against(200).await;
    assert!(err.to_string().contains("too large"), "{err}");
}

#[tokio::test]
async fn an_over_cap_error_response_is_refused_unread() {
    let err = propose_against(500).await;
    let message = err.to_string();
    assert!(
        message.contains("too large"),
        "{}",
        &message[..message.len().min(200)]
    );
    assert!(
        message.len() < 1_000,
        "the error does not carry the body: {} bytes",
        message.len()
    );
}
