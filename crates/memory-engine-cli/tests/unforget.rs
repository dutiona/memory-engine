//! `unforget`: the maintenance command that reverses a forget (#1023, ADR-0020 §8).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use memory_engine::StoreLock;
use memory_engine::types::{AddFactRequest, EventType, FactType, NewEvent};
use predicates::prelude::*;
use tempfile::TempDir;

fn record() -> NewEvent {
    NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: EventType::MemoryOp,
        payload: serde_json::json!({ "tool": "forget" }),
        source: "test".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "test".into(),
        sequence_id: 0,
        created_at: None,
    }
}

/// A store with a laptop-bucket fact (id 1) forgotten with the reason `it moved`, and a live
/// one (id 2); `expired` forgets an already expired fact instead of a live one, and `copy`
/// adds an identical copy of fact 1 (id 3), which the forget takes too.
async fn store(expired: bool, copy: bool) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.db");
    let mut engine = memory_engine::MemoryEngine::builder(4)
        .path(db.clone())
        .build()
        .unwrap();
    let fingerprint = memory_engine::EmbeddingFingerprint::new("mock", "test", 4);
    let contents: &[&str] = if copy {
        &["the kestrel nests", "the falcon hunts", "the kestrel nests"]
    } else {
        &["the kestrel nests", "the falcon hunts"]
    };
    for content in contents {
        engine
            .add_fact_precomputed(
                &AddFactRequest {
                    content: (*content).into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: Some("unscoped:laptop".into()),
                    opts: None,
                },
                vec![0.1, 0.2, 0.3, 0.4],
                &fingerprint,
                None,
            )
            .await
            .unwrap();
    }
    engine.close().await.unwrap();
    if expired {
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("UPDATE facts SET t_expired = '2026-01-01T00:00:00+00:00' WHERE id = 1")
            .unwrap();
    }
    let mut engine = memory_engine::MemoryEngine::builder(4)
        .path(db.clone())
        .build()
        .unwrap();
    engine
        .forget_fact(
            1,
            &["unscoped:laptop".into()],
            "it moved",
            "unscoped:laptop",
            &record(),
        )
        .await
        .unwrap();
    engine.close().await.unwrap();
    (dir, db)
}

fn cli(db: &Path) -> Command {
    let mut cmd = Command::cargo_bin("memory-engine-cli").unwrap();
    cmd.arg("--db").arg(db);
    cmd
}

fn kestrel_found(db: &Path) -> bool {
    let out = cli(db)
        .args(["--format", "json", "query", "kestrel"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out)
        .unwrap()
        .contains("the kestrel nests")
}

#[tokio::test]
async fn unforget_returns_a_forgotten_fact_to_recall() {
    let (_dir, db) = store(false, false).await;
    assert!(!kestrel_found(&db));
    cli(&db)
        .args(["unforget", "1"])
        .assert()
        .success()
        .stdout("unforgot fact 1: back in recall\n");
    assert!(kestrel_found(&db));
}

#[tokio::test]
async fn unforget_says_when_the_fact_stays_out_of_recall() {
    let (_dir, db) = store(true, false).await;
    cli(&db)
        .args(["--format", "json", "unforget", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""active": false"#));
    assert!(!kestrel_found(&db));
}

#[tokio::test]
async fn unforget_lists_every_copy_it_restores() {
    let (_dir, db) = store(false, true).await;
    cli(&db)
        .args(["--format", "plain", "unforget", "1"])
        .assert()
        .success()
        .stdout("fact_id=1 active=true\nfact_id=3 active=true\n");
}

#[tokio::test]
async fn unforget_refuses_while_the_store_is_held() {
    let (_dir, db) = store(false, false).await;
    {
        let _held = StoreLock::acquire(&db).unwrap();
        cli(&db)
            .args(["unforget", "1"])
            .assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("memory.db.store-lock"))
            .stderr(predicate::str::contains(
                "stop the memory-engine server first",
            ));
        assert!(!kestrel_found(&db), "still forgotten");
    }
    cli(&db).args(["unforget", "1"]).assert().success();
}

#[tokio::test]
async fn unforget_of_a_fact_that_is_not_forgotten_or_missing_fails() {
    let (_dir, db) = store(false, false).await;
    cli(&db)
        .args(["unforget", "2"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("fact 2 is not forgotten"));
    cli(&db)
        .args(["unforget", "99"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("fact 99 not found"));
}

#[tokio::test]
async fn explain_names_a_forgotten_fact_and_why() {
    let (_dir, db) = store(false, false).await;
    cli(&db)
        .args(["explain", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Forgotten"))
        .stdout(predicate::str::contains("it moved"));
}
