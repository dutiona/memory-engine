//! An embedder that is down, slow or refuses the key never fails a `recall`: it answers from
//! full-text search, flagged with ADR-0020 §10's cause. `remember` stores at once whatever
//! the embedder's state, and the drain gives each fact its vector once the embedder answers
//! (#1021).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{DIM, TestServer, an_embedding};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, Request, Respond, ResponseTemplate};

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

// --- remember stores at once; the drain embeds the queue (#1021) ---

async fn remember(server: &TestServer, content: &str) -> i64 {
    let reply = server
        .tool("laptop", "remember", json!({ "content": content }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    reply
        .body
        .strip_prefix("stored ")
        .and_then(|rest| rest.split_once(' '))
        .unwrap_or_else(|| panic!("not a receipt: {reply:?}"))
        .0
        .parse()
        .unwrap()
}

/// Wait, at most `limit`, until every fact in `ids` has its vector.
async fn wait_for_vectors(server: &TestServer, ids: &[i64], limit: Duration) {
    let deadline = Instant::now() + limit;
    for &id in ids {
        while !server.has_vector(id).await {
            assert!(Instant::now() < deadline, "fact {id} got no vector");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Answer an embed request whose body contains `marker` with `answer`, before every other mock.
async fn answer_for(server: &TestServer, marker: &str, answer: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_string_contains(marker))
        .respond_with(answer)
        .with_priority(1)
        .mount(&server.embedder)
        .await;
}

/// A responder whose answer the test switches while the drain runs: unlike a reset and a
/// remount, no request ever finds no mock.
#[derive(Clone)]
struct Switch(Arc<Mutex<ResponseTemplate>>);

impl Switch {
    fn new(answer: ResponseTemplate) -> Self {
        Self(Arc::new(Mutex::new(answer)))
    }

    fn set(&self, answer: ResponseTemplate) {
        *self.0.lock().unwrap() = answer;
    }
}

impl Respond for Switch {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0.lock().unwrap().clone()
    }
}

/// The embedding queue's status, read as `memory-engine-cli status` reads it.
fn status(server: &TestServer) -> memory_engine::EmbedQueueStatus {
    memory_engine::read_embed_queue_status(&server.db()).unwrap()
}

/// Wait, at most 10 s, until the queue's status satisfies `done`.
async fn wait_for_status(
    server: &TestServer,
    done: impl Fn(&memory_engine::EmbedQueueStatus) -> bool,
) -> memory_engine::EmbedQueueStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let now = status(server);
        if done(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "status never reached: {now:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_fact_remembered_while_the_embedder_is_down_is_stored_and_found() {
    let server = TestServer::start_unreachable().await;
    let id = remember(&server, "the kestrel nests on the cliff").await;
    let (attempts, _, failed_at, _) = server.queue(id).expect("queued");
    assert_eq!((attempts, failed_at), (0, None));
    assert!(!server.has_vector(id).await);

    let answer = recall(&server, "kestrel").await;
    assert_eq!(answer["search"], "full-text only", "{answer}");
    assert_eq!(answer["degraded"], "embedder down", "{answer}");
    assert_eq!(ids(&answer), [id]);
}

#[tokio::test]
async fn a_pending_fact_never_breaks_a_hybrid_recall() {
    let server = TestServer::start().await;
    server
        .mount_embedder(an_embedding(), ResponseTemplate::new(503))
        .await;
    let id = remember(&server, "the kestrel nests on the cliff").await;
    let answer = recall(&server, "kestrel").await;
    assert_eq!(answer["search"], "hybrid", "{answer}");
    assert!(answer.get("degraded").is_none(), "{answer}");
    assert_eq!(ids(&answer), [id]);
    assert!(!server.has_vector(id).await, "found through full text");
}

/// Answers each document embed after 100 ms, recording when each arrived.
#[derive(Clone, Default)]
struct Arrivals(Arc<Mutex<Vec<Instant>>>);

const DOCUMENT_DELAY: Duration = Duration::from_millis(100);

impl Respond for Arrivals {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0.lock().unwrap().push(Instant::now());
        an_embedding().set_delay(DOCUMENT_DELAY)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backlog_drains_one_input_at_a_time_and_recall_stays_hybrid() {
    let server = TestServer::start().await;
    let arrivals = Arrivals::default();
    server
        .mount_embedder(an_embedding(), arrivals.clone())
        .await;
    let mut facts = Vec::new();
    for i in 0..20 {
        facts.push(remember(&server, &format!("kestrel backlog {i}")).await);
    }
    let mut recalls = 0;
    while server.engine.next_embedding().await.unwrap() != memory_engine::NextEmbedding::Idle {
        let answer = recall(&server, "kestrel").await;
        assert_eq!(answer["search"], "hybrid", "{answer}");
        assert!(answer.get("degraded").is_none(), "{answer}");
        recalls += 1;
    }
    assert!(recalls > 0);
    for &id in &facts {
        assert!(server.has_vector(id).await, "{id}");
    }
    let arrivals = arrivals.0.lock().unwrap().clone();
    assert_eq!(arrivals.len(), facts.len());
    // A response takes at least the delay, so a request sent only once the previous one was
    // answered arrives at least the delay after it.
    for pair in arrivals.windows(2) {
        assert!(
            pair[1] - pair[0] >= DOCUMENT_DELAY,
            "two document requests overlapped: {:?} apart",
            pair[1] - pair[0]
        );
    }
}

#[tokio::test]
async fn an_outage_longer_than_the_cap_ends_with_every_vector() {
    let mut server = TestServer::start_unreachable().await;
    let mut facts = Vec::new();
    for content in ["the kestrel", "the falcon", "the merlin"] {
        facts.push(remember(&server, content).await);
    }
    let rows: Vec<_> = facts.iter().map(|&id| server.queue(id)).collect();
    tokio::time::sleep(common::BACKOFF.cap * 4).await;
    let after: Vec<_> = facts.iter().map(|&id| server.queue(id)).collect();
    assert_eq!(after, rows, "a refused connection touches no item");

    server.bring_embedder_up().await;
    server.wait_drained().await;
    for &id in &facts {
        assert!(server.has_vector(id).await, "{id}");
        assert_eq!(server.queue(id), None);
    }
    assert_eq!(status(&server).failed, 0);
}

#[tokio::test]
async fn a_rejected_key_pauses_the_queue_and_touches_no_item() {
    let server = TestServer::start().await;
    let documents = Switch::new(ResponseTemplate::new(401));
    server
        .mount_embedder(an_embedding(), documents.clone())
        .await;
    let facts = [
        remember(&server, "the kestrel").await,
        remember(&server, "the falcon").await,
    ];
    let untouched = |server: &TestServer| {
        for id in facts {
            let (attempts, _, failed_at, last_error) = server.queue(id).expect("queued");
            assert_eq!((attempts, failed_at, last_error), (0, None, None), "{id}");
        }
    };

    let paused = wait_for_status(&server, |s| {
        s.pause_error.as_deref().is_some_and(|e| e.contains("401"))
    })
    .await;
    assert!(paused.paused_since.is_some());
    untouched(&server);

    documents.set(ResponseTemplate::new(403));
    let still = wait_for_status(&server, |s| {
        s.pause_error.as_deref().is_some_and(|e| e.contains("403"))
    })
    .await;
    assert_eq!(
        still.paused_since, paused.paused_since,
        "paused since the 401"
    );
    untouched(&server);

    documents.set(an_embedding());
    server.wait_drained().await;
    let cleared = status(&server);
    assert_eq!((cleared.paused_since, cleared.pause_error), (None, None));
    for id in facts {
        assert!(server.has_vector(id).await, "{id}");
    }
}

#[tokio::test]
async fn a_rejected_input_fails_only_that_item() {
    for code in [400, 413, 422] {
        let server = TestServer::start().await;
        answer_for(&server, "poison", ResponseTemplate::new(code)).await;
        let poison = remember(&server, "a poison input").await;
        let others = [
            remember(&server, "the kestrel").await,
            remember(&server, "the falcon").await,
        ];
        server.wait_drained().await;
        for id in others {
            assert!(server.has_vector(id).await, "{code}: {id}");
        }
        let (attempts, _, failed_at, last_error) = server.queue(poison).expect("kept");
        assert_eq!(attempts, 1, "{code}");
        assert!(failed_at.is_some(), "{code}");
        let last_error = last_error.unwrap();
        assert!(
            last_error.contains(&code.to_string()),
            "{code}: {last_error}"
        );
        assert!(!server.has_vector(poison).await, "{code}");
    }
}

#[tokio::test]
async fn a_recurring_5xx_for_one_item_never_holds_up_the_others() {
    let server = TestServer::start().await;
    answer_for(&server, "poison", ResponseTemplate::new(503)).await;
    let poison = remember(&server, "a poison input").await;
    let others = [
        remember(&server, "the kestrel").await,
        remember(&server, "the falcon").await,
        remember(&server, "the merlin").await,
    ];
    wait_for_vectors(&server, &others, Duration::from_secs(10)).await;
    let (attempts, _, failed_at, _) = server.queue(poison).expect("still waiting");
    assert!(attempts >= 1, "{attempts}");
    assert_eq!(failed_at, None);
}

#[tokio::test]
async fn a_poison_item_that_times_out_never_holds_up_the_others() {
    let server = TestServer::start_with_timeout(1).await;
    answer_for(
        &server,
        "poison",
        an_embedding().set_delay(Duration::from_secs(3)),
    )
    .await;
    let poison = remember(&server, "a poison input").await;
    let others = [
        remember(&server, "the kestrel").await,
        remember(&server, "the falcon").await,
    ];
    wait_for_vectors(&server, &others, Duration::from_secs(15)).await;
    let (attempts, _, failed_at, _) = server.queue(poison).expect("still waiting");
    assert!(attempts >= 1, "{attempts}");
    assert_eq!(failed_at, None);
}

#[tokio::test]
async fn a_failing_embedder_is_not_stormed() {
    let server = TestServer::start().await;
    server
        .mount_embedder(ResponseTemplate::new(503), ResponseTemplate::new(503))
        .await;
    let start = Instant::now();
    for i in 0..10 {
        remember(&server, &format!("fact {i}")).await;
    }
    tokio::time::sleep(Duration::from_secs(1).saturating_sub(start.elapsed())).await;
    let requests = server.embedder.received_requests().await.unwrap().len();
    assert!(requests <= 25, "{requests} requests in 1 s");
}

#[tokio::test]
async fn an_over_length_memory_is_chunked_and_gets_one_vector() {
    const LIMIT: usize = 64;
    let server = TestServer::start_with_input_limit(LIMIT).await;
    let content = "the kestrel nests on the cliff. ".repeat(10)[..300].to_owned();
    let id = remember(&server, &content).await;
    server.wait_drained().await;

    let fact = server.engine.get_fact(id).await.unwrap();
    assert_eq!(fact.embedding.len(), DIM);
    assert_eq!(fact.content, content, "the stored content is never cut");
    let embeds: Vec<String> = server
        .embedder
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/embeddings")
        .map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["input"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert!(embeds.len() >= 2, "{embeds:?}");
    for input in &embeds {
        assert!(
            input.len() / 2 <= LIMIT,
            "a piece over the limit: {input:?}"
        );
    }
    assert_eq!(embeds.concat(), content, "the pieces rejoin exactly");
}

#[tokio::test]
async fn the_queue_survives_a_restart() {
    let mut server = TestServer::start_unreachable().await;
    let id = remember(&server, "the kestrel nests on the cliff").await;
    server.restart().await;
    assert!(server.queue(id).is_some(), "still queued");
    assert!(!server.has_vector(id).await);

    server.bring_embedder_up().await;
    server.wait_drained().await;
    assert!(server.has_vector(id).await);
}
