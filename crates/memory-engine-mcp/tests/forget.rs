//! `forget(id, reason)` over HTTP (ADR-0020 §8, ADR-0021 §5, #1023): a retraction behind a
//! tombstone, never a deletion. These calls send no project facts, so every session is
//! keyless: its Homes are its machine's bucket and `user:alice`.

mod common;

use common::TestServer;
use serde_json::{Value, json};

const PROJECT_KEY: &str = "github.com/example/project";
const PROJECT_HOME: &str = "user:alice/project:github.com%2Fexample%2Fproject";

fn remembered_id(reply: &common::Reply) -> i64 {
    reply.body.split(' ').nth(1).unwrap().parse().unwrap()
}

async fn remember(server: &TestServer, content: &str) -> i64 {
    let reply = server
        .tool("laptop", "remember", json!({ "content": content }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    remembered_id(&reply)
}

async fn recall_ids(server: &TestServer, query: &str) -> Vec<i64> {
    let reply = server
        .tool("laptop", "recall", json!({ "query": query }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

async fn forget(server: &TestServer, machine: &str, id: i64) -> common::Reply {
    server
        .tool(machine, "forget", json!({ "id": id, "reason": "it moved" }))
        .await
}

#[tokio::test]
async fn forget_retracts_a_bucket_memory() {
    let server = TestServer::start().await;
    let id = remember(&server, "the kestrel nests on the north ledge").await;
    assert_eq!(recall_ids(&server, "kestrel").await, vec![id]);
    let reply = forget(&server, "laptop", id).await;
    assert!(!reply.is_error, "{reply:?}");
    assert_eq!(reply.body, "forgotten");
    assert_eq!(recall_ids(&server, "kestrel").await, Vec::<i64>::new());
    let fact = server.engine.get_fact(id).await.expect("the row stays");
    assert!(
        fact.t_expired.is_some() && fact.t_invalid.is_some(),
        "{fact:?}"
    );
    let source = fact.source_event_id.expect("the remember record");
    assert!(
        server
            .records()
            .iter()
            .any(|(record, ..)| *record == source),
        "the remember record stays"
    );
}

#[tokio::test]
async fn forget_retracts_an_identical_copy_too() {
    let server = TestServer::start().await;
    let first = remember(&server, "the kestrel nests on the north ledge").await;
    let second = remember(&server, "the kestrel nests on the north ledge").await;
    assert_ne!(first, second);
    assert_eq!(forget(&server, "laptop", first).await.body, "forgotten");
    assert_eq!(recall_ids(&server, "kestrel").await, Vec::<i64>::new());
}

#[tokio::test]
async fn the_call_is_recorded_with_the_retraction() {
    let server = TestServer::start().await;
    let id = remember(&server, "the kestrel nests").await;
    let reply = forget(&server, "laptop", id).await;
    let records = server.records();
    let (record, payload, scope) = records
        .iter()
        .find(|(_, p, _)| p["tool"] == "forget")
        .expect("a forget record");
    assert_eq!(payload["call"], reply.call.as_str());
    assert_eq!(payload["machine"], "laptop");
    assert_eq!(payload["fact_ids"], json!([id]));
    assert!(
        payload.get("reason").is_none(),
        "the reason stays out of the log"
    );
    assert_eq!(scope, "unscoped:laptop");
    let stone = server.engine.explain_fact(id).await.unwrap().state;
    let memory_engine::inspect::FactState::Expired {
        reason:
            memory_engine::inspect::ExpiredReason::Forgotten {
                reason,
                record_event_id,
                ..
            },
    } = stone
    else {
        panic!("explained as forgotten: {stone:?}");
    };
    assert_eq!(record_event_id, Some(*record));
    assert_eq!(reason.as_deref(), Some("it moved"));
}

#[tokio::test]
async fn a_second_forget_answers_already_forgotten() {
    let server = TestServer::start().await;
    let id = remember(&server, "the kestrel nests").await;
    forget(&server, "laptop", id).await;
    let reply = forget(&server, "laptop", id).await;
    assert!(!reply.is_error, "{reply:?}");
    assert_eq!(reply.body, "already forgotten");
}

#[tokio::test]
async fn forget_acts_at_the_user_level_from_any_machine() {
    let server = TestServer::start().await;
    let id = server
        .seed(Some("user:alice"), "the kestrel nests", json!({}))
        .await;
    assert_eq!(forget(&server, "desktop", id).await.body, "forgotten");
}

/// C7 and C17 (ADR-0021): a missing id, a negative id, a root fact, another machine's bucket
/// and a project the session reached only through `recall(project:)` answer the same.
#[tokio::test]
async fn forget_and_set_level_answer_not_found_alike() {
    let server = TestServer::start().await;
    let at_root = server.seed(None, "kestrel at the root", json!({})).await;
    let desktops = server
        .seed(Some("unscoped:desktop"), "kestrel on desktop", json!({}))
        .await;
    let in_project = server
        .seed(Some(PROJECT_HOME), "kestrel in the project", json!({}))
        .await;
    let swapped = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": "kestrel", "project": PROJECT_KEY }),
        )
        .await;
    assert!(
        swapped.body.contains(&in_project.to_string()),
        "{swapped:?}"
    );
    let ids = [999_999, -1, at_root, desktops, in_project];
    let mut bodies = Vec::new();
    for id in ids {
        let reply = forget(&server, "laptop", id).await;
        assert!(!reply.is_error, "forget {id}: {reply:?}");
        bodies.push(reply.body);
        let reply = server
            .tool("laptop", "set_level", json!({ "id": id, "level": "user" }))
            .await;
        assert!(!reply.is_error, "set_level {id}: {reply:?}");
        bodies.push(reply.body);
    }
    assert!(bodies.iter().all(|b| b == "not found"), "{bodies:?}");
    let moves: Vec<Value> = server
        .records()
        .into_iter()
        .map(|(_, payload, _)| payload)
        .filter(|p| p["tool"] == "set_level")
        .collect();
    assert_eq!(moves.len(), ids.len(), "{moves:?}");
    for (payload, id) in moves.iter().zip(ids) {
        assert_eq!(payload["fact_ids"], json!([id]), "{payload}");
        assert!(
            payload.get("error").is_none() && payload.get("from").is_none(),
            "not found is not a failure and not a move: {payload}"
        );
    }
    for id in [at_root, desktops, in_project] {
        let fact = server.engine.get_fact(id).await.unwrap();
        assert!(fact.t_expired.is_none(), "{id} untouched");
        assert!(
            matches!(
                server.engine.explain_fact(id).await.unwrap().state,
                memory_engine::inspect::FactState::Active
            ),
            "{id}: no tombstone"
        );
    }
}

/// The acceptance: re-running the ADR-0009 `--memory-dir` importer on a forgotten memory's
/// source leaves it absent.
#[tokio::test]
async fn a_forgotten_import_stays_out_when_the_importer_reruns() {
    let server = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("kestrel.md"),
        "---\nmetadata:\n  type: project\n---\nThe kestrel nests on the north ledge.\n",
    )
    .unwrap();
    let embedder: std::sync::Arc<dyn memory_engine::EmbeddingProvider> =
        server.provider.clone().expect("the test embedder");
    let config = memory_engine::BootstrapConfig {
        scope: Some("unscoped:laptop".into()),
        ..Default::default()
    };
    let import = || {
        server
            .engine
            .bootstrap_memory_directory(dir.path(), embedder.clone(), &config, None)
    };
    assert_eq!(import().await.unwrap().facts_created, 1);
    let id = recall_ids(&server, "kestrel").await[0];
    assert_eq!(forget(&server, "laptop", id).await.body, "forgotten");
    let rerun = import().await.unwrap();
    assert_eq!((rerun.facts_created, rerun.facts_blocked), (0, 1));
    assert_eq!(recall_ids(&server, "kestrel").await, Vec::<i64>::new());
}

#[tokio::test]
async fn remember_after_forget_stores_a_new_memory() {
    let server = TestServer::start().await;
    let id = remember(&server, "the kestrel nests").await;
    forget(&server, "laptop", id).await;
    let again = remember(&server, "the kestrel nests").await;
    assert_ne!(again, id);
    assert_eq!(recall_ids(&server, "kestrel").await, vec![again]);
}

#[tokio::test]
async fn forget_refuses_bad_arguments_and_records_why() {
    let server = TestServer::start().await;
    for (args, needle) in [
        (json!({}), "missing field"),
        (json!({ "id": 1 }), "reason"),
        (json!({ "id": 1, "reason": "  " }), "reason"),
        (json!({ "id": "1", "reason": "r" }), "invalid type"),
        (json!({ "id": 1, "reason": "r", "level": "user" }), "level"),
    ] {
        let reply = server.tool("laptop", "forget", args.clone()).await;
        assert!(reply.is_error, "{args}: {reply:?}");
        assert!(reply.body.contains(needle), "{args}: {reply:?}");
    }
    let refusals: Vec<Value> = server
        .records()
        .into_iter()
        .filter(|(_, p, _)| p["tool"] == "forget")
        .map(|(_, p, _)| p)
        .collect();
    assert_eq!(refusals.len(), 5);
    assert!(refusals.iter().all(|p| p.get("error").is_some()));
}

#[tokio::test]
async fn a_forget_whose_record_fails_changes_nothing() {
    let server = TestServer::start().await;
    let id = remember(&server, "the kestrel nests").await;
    server.refuse_event_writes();
    let reply = forget(&server, "laptop", id).await;
    assert!(reply.is_error, "{reply:?}");
    assert!(
        server
            .engine
            .get_fact(id)
            .await
            .unwrap()
            .t_expired
            .is_none()
    );
    assert!(matches!(
        server.engine.explain_fact(id).await.unwrap().state,
        memory_engine::inspect::FactState::Active
    ));
}
