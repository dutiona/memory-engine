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
