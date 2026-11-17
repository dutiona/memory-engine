//! The model's surface over HTTP (ADR-0020 §8, ADR-0021 §5-§6): `recall` and `remember`, each
//! answer headed by its call ID. Until #1022 every session is keyless: it files into its
//! machine's bucket and reads that bucket and `user:alice`.

mod common;

use common::TestServer;
use serde_json::{Value, json};

const PROJECT_KEY: &str = "github.com/example/project";
const PROJECT_HOME: &str = "user:alice/project:github.com%2Fexample%2Fproject";

fn ids(reply: &common::Reply) -> Vec<i64> {
    let mut ids: Vec<i64> = reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn every_answer_starts_with_its_call_id() {
    let server = TestServer::start().await;
    let mut calls = Vec::new();
    for (name, args, failed) in [
        (
            "remember",
            json!({ "content": "the lock file sits beside the store" }),
            false,
        ),
        ("recall", json!({ "query": "lock" }), false),
        ("recall", json!({}), true),
        ("recall", json!({ "query": "   " }), true),
        ("recall", json!({ "query": 5 }), true),
        (
            "recall",
            json!({ "query": "lock", "project": "nowhere" }),
            true,
        ),
        ("remember", json!({}), true),
        ("remember", json!({ "content": "" }), true),
    ] {
        let reply = server.tool("laptop", name, args.clone()).await;
        assert_eq!(reply.is_error, failed, "{name} {args}: {reply:?}");
        calls.push(reply.call);
    }
    let without = TestServer::start_without_embedder().await;
    let reply = without
        .tool("laptop", "remember", json!({ "content": "x" }))
        .await;
    assert!(reply.is_error);
    assert!(reply.body.contains("no embedder"), "{reply:?}");
    calls.push(reply.call);
    let distinct: std::collections::HashSet<_> = calls.iter().collect();
    assert_eq!(distinct.len(), calls.len(), "call IDs repeat: {calls:?}");
}

#[tokio::test]
async fn unknown_arguments_are_refused() {
    let server = TestServer::start().await;
    for (name, key) in [
        ("recall", "limit"),
        ("recall", "mode"),
        ("recall", "scope"),
        ("recall", "depth"),
        ("remember", "level"),
        ("remember", "metadata"),
        ("remember", "embedding"),
    ] {
        let mut args = json!({ "query": "lock", "content": "c" });
        let args = args.as_object_mut().unwrap();
        args.remove(if name == "recall" { "content" } else { "query" });
        args.insert(key.to_owned(), json!(1));
        let reply = server
            .tool("laptop", name, Value::Object(args.clone()))
            .await;
        assert!(reply.is_error, "{name} {key}: {reply:?}");
        assert!(reply.body.contains(key), "{name} {key}: {reply:?}");
    }
    assert_eq!(server.fact_count().await, 0);
    let requests = server.embedder.received_requests().await.unwrap();
    assert!(requests.is_empty(), "the embedder was called");
}

#[tokio::test]
async fn remember_files_into_the_calling_machines_bucket() {
    let server = TestServer::start().await;
    let reply = server
        .tool(
            "laptop",
            "remember",
            json!({ "content": "the lock file sits beside the store" }),
        )
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let id: i64 = reply
        .body
        .strip_prefix("stored ")
        .and_then(|rest| rest.split_once(' '))
        .unwrap()
        .0
        .parse()
        .unwrap();
    assert_eq!(
        reply.body,
        format!("stored {id} in unscoped on laptop, visible to all agents")
    );
    let explained = server.engine.explain_fact(id).await.unwrap();
    assert_eq!(explained.scope_path, "unscoped:laptop");
    let fact = server.engine.get_fact(id).await.unwrap();
    assert_eq!(fact.fact_type, memory_engine::types::FactType::Semantic);
    assert_eq!(fact.metadata, json!({ "machine": "laptop" }));
}

/// C2, C3, C7 and C16 (ADR-0021) over HTTP.
#[tokio::test]
async fn recall_reads_exactly_the_bucket_and_the_user_level() {
    let server = TestServer::start().await;
    for i in 0..20 {
        let strong = format!("kestrel kestrel kestrel strong {i}");
        server
            .seed(Some("unscoped:desktop"), &strong, json!({}))
            .await;
        server.seed(Some(PROJECT_HOME), &strong, json!({})).await;
    }
    for i in 0..5 {
        server
            .seed(
                None,
                &format!("kestrel kestrel kestrel root {i}"),
                json!({}),
            )
            .await;
    }
    let mut user = Vec::new();
    for i in 0..2 {
        user.push(
            server
                .seed(Some("user:alice"), &format!("kestrel user {i}"), json!({}))
                .await,
        );
    }
    let mut laptop = user.clone();
    for i in 0..3 {
        laptop.push(
            server
                .seed(
                    Some("unscoped:laptop"),
                    &format!("kestrel laptop {i}"),
                    json!({ "machine": "laptop" }),
                )
                .await,
        );
    }
    laptop.sort_unstable();

    let reply = server
        .tool("laptop", "recall", json!({ "query": "kestrel" }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    assert_eq!(ids(&reply), laptop, "not starved, and nothing hidden");
    let reply = server
        .tool("desktop", "recall", json!({ "query": "kestrel user" }))
        .await;
    let desktop_reads = ids(&reply);
    assert!(
        desktop_reads.iter().all(|id| !laptop[2..].contains(id)),
        "{desktop_reads:?}"
    );

    for i in 2..15 {
        server
            .seed(Some("user:alice"), &format!("kestrel user {i}"), json!({}))
            .await;
    }
    let reply = server
        .tool("laptop", "recall", json!({ "query": "kestrel" }))
        .await;
    assert_eq!(reply.json()["results"].as_array().unwrap().len(), 10);
}

/// C6: `project` reads that project and the user level, never the bucket.
#[tokio::test]
async fn recall_project_swaps_the_context_home() {
    let server = TestServer::start().await;
    let in_project = server
        .seed(Some(PROJECT_HOME), "kestrel in the project", json!({}))
        .await;
    let at_user = server
        .seed(Some("user:alice"), "kestrel at the user level", json!({}))
        .await;
    server
        .seed(Some("unscoped:laptop"), "kestrel in the bucket", json!({}))
        .await;
    let reply = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": "kestrel", "project": PROJECT_KEY }),
        )
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let mut want = vec![in_project, at_user];
    want.sort_unstable();
    assert_eq!(ids(&reply), want);
    let scopes: Vec<String> = reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["scope"].as_str().unwrap().to_owned())
        .collect();
    assert!(scopes.contains(&PROJECT_KEY.to_owned()), "{scopes:?}");
    assert!(scopes.contains(&"user".to_owned()), "{scopes:?}");
}

/// C5: an unknown key is an error naming it, and creates nothing.
#[tokio::test]
async fn an_unknown_project_is_an_error_that_names_it() {
    let server = TestServer::start().await;
    server
        .seed(Some("user:alice/project:laptop%3Ax"), "kestrel", json!({}))
        .await;
    let long = "k".repeat(300);
    for key in [
        "github.com/nowhere/at-all",
        "x",
        "unscoped:laptop",
        "",
        long.as_str(),
    ] {
        let reply = server
            .tool(
                "laptop",
                "recall",
                json!({ "query": "kestrel", "project": key }),
            )
            .await;
        assert!(reply.is_error, "{key:?}: {reply:?}");
        assert!(
            reply.body.contains(&format!("{key:?}")),
            "{key:?}: {reply:?}"
        );
    }
    assert_eq!(
        server.engine.resolve_scope_path("user:alice/project:x"),
        None
    );
    assert_eq!(
        server
            .engine
            .resolve_scope_path("user:alice/project:github.com%2Fnowhere%2Fat-all"),
        None
    );

    // No `project`, no bucket yet: the read is the user level only, and creates no project.
    let at_user = server
        .seed(Some("user:alice"), "kestrel for alice", json!({}))
        .await;
    let reply = server
        .tool("desktop", "recall", json!({ "query": "kestrel" }))
        .await;
    assert_eq!(ids(&reply), vec![at_user]);
}

#[tokio::test]
async fn results_carry_kind_scope_date_and_machine() {
    let server = TestServer::start().await;
    let seeded = server
        .seed(Some("user:alice"), "kestrel seeded", json!({}))
        .await;
    let reply = server
        .tool(
            "laptop",
            "remember",
            json!({ "content": "kestrel remembered" }),
        )
        .await;
    assert!(!reply.is_error);
    let reply = server
        .tool("laptop", "recall", json!({ "query": "kestrel" }))
        .await;
    let body = reply.json();
    assert_eq!(body["search"], "hybrid");
    for result in body["results"].as_array().unwrap() {
        assert_eq!(result["kind"], "remembered");
        let date = result["date"].as_str().unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(date).is_ok() && date.ends_with('Z'),
            "{date}"
        );
        assert_eq!(date.len(), "2026-11-17T00:00:00Z".len(), "{date}");
        if result["id"] == seeded {
            assert_eq!(result["scope"], "user");
            assert!(result["machine"].is_null());
        } else {
            assert_eq!(result["scope"], "unscoped");
            assert_eq!(result["machine"], "laptop");
            assert_eq!(result["content"], "kestrel remembered");
        }
    }
}

#[tokio::test]
async fn a_full_text_only_answer_says_so() {
    let server = TestServer::start_without_embedder().await;
    let id = server
        .seed(
            Some("unscoped:laptop"),
            "kestrel without vectors",
            json!({}),
        )
        .await;
    let reply = server
        .tool("laptop", "recall", json!({ "query": "kestrel" }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    assert_eq!(reply.json()["search"], "full-text only");
    assert_eq!(ids(&reply), vec![id]);
}

#[tokio::test]
async fn an_oversized_query_is_refused() {
    let server = TestServer::start().await;
    let reply = server
        .tool("laptop", "recall", json!({ "query": "a".repeat(1025) }))
        .await;
    assert!(reply.is_error);
    assert!(
        reply.body.contains("1025") && reply.body.contains("1024"),
        "{reply:?}"
    );
    assert!(
        server
            .embedder
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    let reply = server
        .tool("laptop", "recall", json!({ "query": "a".repeat(1024) }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
}

#[tokio::test]
async fn an_oversized_unknown_project_is_named_in_full() {
    let server = TestServer::start().await;
    let key = "p".repeat(5000);
    let reply = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": "kestrel", "project": key }),
        )
        .await;
    assert!(reply.is_error);
    assert!(reply.body.contains(&key));
}

async fn embedded_inputs(server: &TestServer) -> Vec<Value> {
    server
        .embedder
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["input"].clone())
        .collect()
}

/// ADR-0015 §6: queries carry the instruction, documents never do.
#[tokio::test]
async fn recall_embeds_with_the_query_instruction_and_remember_without() {
    let server = TestServer::start_with_query_instruction("Q-PREFIX: ").await;
    server
        .tool("laptop", "recall", json!({ "query": "search terms" }))
        .await;
    server
        .tool("laptop", "remember", json!({ "content": "document text" }))
        .await;
    assert_eq!(
        embedded_inputs(&server).await,
        vec![json!("Q-PREFIX: search terms"), json!("document text")]
    );

    // With none configured, ME's pinned instruction applies to queries only.
    let server = TestServer::start().await;
    server
        .tool("laptop", "recall", json!({ "query": "search terms" }))
        .await;
    server
        .tool("laptop", "remember", json!({ "content": "document text" }))
        .await;
    let pinned = memory_engine_mcp::embedding::HttpEmbeddingProvider::DEFAULT_QUERY_INSTRUCTION;
    assert_eq!(
        embedded_inputs(&server).await,
        vec![
            json!(format!("{pinned}search terms")),
            json!("document text")
        ]
    );
}

// --- Call records (ADR-0020 §8, ADR-0021 §12): one event-log record per model-tool call ---

/// The `MemoryOp` records, oldest first: (id, payload, scope path).
fn records(server: &TestServer) -> Vec<(i64, Value, String)> {
    let conn = rusqlite::Connection::open(server.dir.path().join("memory.db")).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT e.id, e.payload, s.label FROM events e JOIN scopes s ON s.id = e.scope_id \
             WHERE e.event_type = 'MemoryOp' ORDER BY e.id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        let payload: String = r.get(1)?;
        Ok((
            r.get(0)?,
            serde_json::from_str(&payload).unwrap(),
            r.get(2)?,
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn remembered_id(reply: &common::Reply) -> i64 {
    reply.body.split(' ').nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn each_call_is_recorded_in_the_event_log() {
    let server = TestServer::start().await;
    let remember = server
        .tool(
            "laptop",
            "remember",
            json!({ "content": "kestrel recorded" }),
        )
        .await;
    let fact = remembered_id(&remember);
    let recall = server
        .tool("laptop", "recall", json!({ "query": "kestrel" }))
        .await;
    let unknown = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": "kestrel", "project": "nowhere" }),
        )
        .await;
    let refused = server
        .tool(
            "laptop",
            "remember",
            json!({ "content": "c", "level": "user" }),
        )
        .await;
    let _ = server.call("laptop", "memory_forget", json!({})).await;

    let records = records(&server);
    assert_eq!(records.len(), 4, "{records:?}");
    for ((_, payload, scope), (reply, tool)) in records.iter().zip([
        (&remember, "remember"),
        (&recall, "recall"),
        (&unknown, "recall"),
        (&refused, "remember"),
    ]) {
        assert_eq!(payload["call"], reply.call, "{payload}");
        assert_eq!(payload["tool"], tool, "{payload}");
        assert_eq!(payload["machine"], "laptop", "{payload}");
        assert_eq!(
            scope, "unscoped:laptop",
            "under the session's own Home, even under the swap"
        );
    }
    assert_eq!(records[1].1["fact_ids"], json!([fact]));
    assert!(records[2].1["error"].as_str().unwrap().contains("nowhere"));
    assert!(records[3].1["error"].as_str().unwrap().contains("level"));
    assert!(records[0].1.get("fact_ids").is_none() && records[0].1.get("error").is_none());
    let stored = server.engine.get_fact(fact).await.unwrap();
    assert_eq!(stored.source_event_id, Some(records[0].0));
}

#[tokio::test]
async fn a_keyless_recall_on_a_fresh_store_creates_the_bucket_and_no_project() {
    let server = TestServer::start().await;
    let before = server.engine.statistics().await.unwrap().scopes.total;
    server
        .tool("desktop", "recall", json!({ "query": "kestrel" }))
        .await;
    assert!(
        server
            .engine
            .resolve_scope_path("unscoped:desktop")
            .is_some()
    );
    assert_eq!(
        server.engine.statistics().await.unwrap().scopes.total,
        before + 1
    );
}

#[tokio::test]
async fn an_oversized_value_cannot_make_its_record_fail() {
    let server = TestServer::start().await;
    let key = "p".repeat(5000);
    let reply = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": "kestrel", "project": key }),
        )
        .await;
    assert!(
        reply.is_error && reply.body.contains(&key),
        "the reply names the value in full"
    );
    let records = records(&server);
    let error = records[0].1["error"].as_str().unwrap();
    assert!(error.len() <= 512, "{}", error.len());
}

fn refuse_event_writes(server: &TestServer) {
    rusqlite::Connection::open(server.dir.path().join("memory.db"))
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_events BEFORE INSERT ON events \
             BEGIN SELECT RAISE(ABORT, 'event writes refused by the test'); END;",
        )
        .unwrap();
}

#[tokio::test]
async fn a_remember_whose_record_fails_stores_nothing() {
    let server = TestServer::start().await;
    server
        .engine
        .ensure_scope_path("unscoped:laptop")
        .await
        .unwrap();
    refuse_event_writes(&server);
    let reply = server
        .tool("laptop", "remember", json!({ "content": "kestrel" }))
        .await;
    assert!(reply.is_error, "{reply:?}");
    assert!(reply.body.contains("record"), "{reply:?}");
    assert_eq!(server.fact_count().await, 0);
}

#[tokio::test]
async fn a_recall_whose_record_fails_is_an_error() {
    let server = TestServer::start().await;
    server
        .seed(Some("unscoped:laptop"), "kestrel", json!({}))
        .await;
    refuse_event_writes(&server);
    let reply = server
        .tool("laptop", "recall", json!({ "query": "kestrel" }))
        .await;
    assert!(reply.is_error, "{reply:?}");
    assert!(reply.body.contains("record"), "{reply:?}");
}

#[tokio::test]
async fn a_remember_whose_fact_write_fails_leaves_an_unnamed_record() {
    let server = TestServer::start().await;
    server.embedder.reset().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .mount(&server.embedder)
        .await;
    let reply = server
        .tool("laptop", "remember", json!({ "content": "kestrel" }))
        .await;
    assert!(reply.is_error, "{reply:?}");
    let records = records(&server);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].1["call"], reply.call);
    assert_eq!(server.fact_count().await, 0, "no fact names the record");
}

// --- Natural-language full text (D12): words, never FTS5 syntax ---

#[tokio::test]
async fn a_question_with_punctuation_still_finds_full_text() {
    let server = TestServer::start_without_embedder().await;
    let id = server
        .seed(
            Some("unscoped:laptop"),
            "the lock file sits beside the store",
            json!({}),
        )
        .await;
    let reply = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": "what's the lock file?" }),
        )
        .await;
    assert!(!reply.is_error, "{reply:?}");
    assert_eq!(ids(&reply), vec![id]);
}

#[tokio::test]
async fn a_query_without_words_lists_nothing() {
    let server = TestServer::start_without_embedder().await;
    server
        .seed(Some("unscoped:laptop"), "an important fact", json!({}))
        .await;
    let reply = server
        .tool("laptop", "recall", json!({ "query": "?!" }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    assert_eq!(
        reply.json(),
        json!({ "search": "full-text only", "results": [] })
    );

    let server = TestServer::start().await;
    server
        .seed(Some("unscoped:laptop"), "an important fact", json!({}))
        .await;
    let reply = server
        .tool("laptop", "recall", json!({ "query": "?!" }))
        .await;
    assert_eq!(reply.json()["search"], "vector only");
}

#[tokio::test]
async fn worst_case_queries_at_the_cap_are_answered() {
    let server = TestServer::start_without_embedder().await;
    let id = server
        .seed(Some("unscoped:laptop"), "lock file", json!({}))
        .await;
    // Each is answered within the 1 KiB cap. U+0345 is alphanumeric to Rust and a separator to
    // unicode61, so its run is one long phrase that matches nothing: bounded, never an error.
    for (query, found) in [
        ("lock_".repeat(204), true),
        ("lock ".repeat(204), true),
        ("lock\u{345}".repeat(170), false),
    ] {
        assert!(query.len() <= 1024, "{}", query.len());
        let reply = server
            .tool("laptop", "recall", json!({ "query": query }))
            .await;
        assert!(!reply.is_error, "{reply:?}");
        assert_eq!(ids(&reply) == vec![id], found, "{reply:?}");
    }
}

#[tokio::test]
async fn a_query_whose_last_word_matches_is_found() {
    let server = TestServer::start_without_embedder().await;
    let id = server
        .seed(Some("unscoped:laptop"), "kestrel", json!({}))
        .await;
    let mut query = (0..150)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    query.push(' ');
    query.push_str("kestrel");
    assert!(query.len() <= 1024);
    let reply = server
        .tool("laptop", "recall", json!({ "query": query }))
        .await;
    assert_eq!(ids(&reply), vec![id]);
}
