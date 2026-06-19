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
