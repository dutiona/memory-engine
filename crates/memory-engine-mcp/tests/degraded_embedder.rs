//! An embedder that is down, slow or refuses the key never fails a `recall`: it answers from
//! full-text search, flagged with ADR-0020 §10's cause (#1021).

mod common;

use std::time::{Duration, Instant};

use common::{TestServer, an_embedding};
use serde_json::{Value, json};
use wiremock::ResponseTemplate;

/// A server whose query embeds get `query`, and a fact it can find by full text.
async fn with_query_answer(query: ResponseTemplate) -> (TestServer, i64) {
    let server = TestServer::start().await;
    server.mount_embedder(query, an_embedding()).await;
    let id = server
        .seed(
            Some("unscoped:laptop"),
            "the kestrel nests on the cliff",
            json!({}),
        )
        .await;
    (server, id)
}

async fn recall(server: &TestServer, query: &str) -> Value {
    let reply = server
        .tool("laptop", "recall", json!({ "query": query }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    reply.json()
}

fn ids(answer: &Value) -> Vec<i64> {
    answer["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn recall_answers_from_full_text_when_the_embedder_is_down() {
    let (server, id) = with_query_answer(ResponseTemplate::new(503)).await;
    let answer = recall(&server, "kestrel").await;
    assert_eq!(answer["search"], "full-text only", "{answer}");
    assert_eq!(answer["degraded"], "embedder down", "{answer}");
    assert_eq!(ids(&answer), [id]);
}

#[tokio::test]
async fn recall_answers_within_the_limit_when_the_embedder_hangs() {
    let (server, id) = with_query_answer(an_embedding().set_delay(Duration::from_secs(3))).await;
    let start = Instant::now();
    let answer = recall(&server, "kestrel").await;
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(answer["search"], "full-text only", "{answer}");
    assert_eq!(answer["degraded"], "embedder slow", "{answer}");
    assert_eq!(ids(&answer), [id]);
}

#[tokio::test]
async fn recall_names_a_rejected_key() {
    for status in [401, 403] {
        let (server, id) = with_query_answer(ResponseTemplate::new(status)).await;
        let answer = recall(&server, "kestrel").await;
        assert_eq!(answer["search"], "full-text only", "{status}: {answer}");
        assert_eq!(
            answer["degraded"], "embedder key rejected",
            "{status}: {answer}"
        );
        assert_eq!(ids(&answer), [id], "{status}");
    }
}

#[tokio::test]
async fn a_wordless_recall_with_the_embedder_down_lists_nothing() {
    let (server, _) = with_query_answer(ResponseTemplate::new(503)).await;
    assert_eq!(
        recall(&server, "?!").await,
        json!({ "search": "full-text only", "degraded": "embedder down", "results": [] })
    );
}

#[tokio::test]
async fn a_healthy_embedder_is_never_flagged() {
    let (server, id) = with_query_answer(an_embedding()).await;
    let answer = recall(&server, "kestrel").await;
    assert_eq!(answer["search"], "hybrid", "{answer}");
    assert!(answer.get("degraded").is_none(), "{answer}");
    assert_eq!(ids(&answer), [id]);
    let answer = recall(&server, "?!").await;
    assert_eq!(answer["search"], "vector only", "{answer}");
    assert!(answer.get("degraded").is_none(), "{answer}");

    // No embedder is configuration, not a fallback.
    let server = TestServer::start_without_embedder().await;
    let answer = recall(&server, "kestrel").await;
    assert_eq!(answer["search"], "full-text only", "{answer}");
    assert!(answer.get("degraded").is_none(), "{answer}");
}
