//! `status` reports the deferred-embedding queue through a read-only connection, without
//! the engine (#1021): it takes no lock, never migrates, and changes nothing.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use memory_engine::{
    AddFactRequest, Backoff, EmbedderError, EmbeddingFingerprint, FactType, MemoryEngine,
    MemoryError, NextEmbedding, Settled, StoreLock,
};
use predicates::prelude::*;
use tempfile::TempDir;

fn cli(db: &Path) -> Command {
    let mut cmd = Command::cargo_bin("memory-engine-cli").unwrap();
    cmd.arg("--db").arg(db);
    cmd
}

/// Take the due item and settle `answer` for it.
async fn settle(engine: &MemoryEngine, answer: memory_engine::Result<Vec<f32>>) -> Settled {
    let NextEmbedding::Due(item) = engine.next_embedding().await.unwrap() else {
        panic!("an item is due");
    };
    engine
        .settle_embedding(
            &item,
            answer,
            &EmbeddingFingerprint::new("mock", "test", 4),
            Backoff::DEFAULT,
        )
        .await
        .unwrap()
}

/// A store with `contents` remembered and waiting for their vectors.
async fn store(contents: &[&str]) -> (TempDir, PathBuf, MemoryEngine) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.db");
    let engine = MemoryEngine::builder(4).path(db.clone()).build().unwrap();
    for content in contents {
        engine
            .add_fact_deferred(&AddFactRequest {
                content: (*content).into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            })
            .await
            .unwrap();
    }
    (dir, db, engine)
}

fn stdout_json(cmd: &mut Command) -> serde_json::Value {
    let out = cmd.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&out).unwrap()
}

#[tokio::test]
async fn status_reports_the_queue() {
    let (_dir, db, mut engine) = store(&["embedded", "rejected", "waiting"]).await;
    assert_eq!(
        settle(&engine, Ok(vec![1.0, 0.0, 0.0, 0.0])).await,
        Settled::Embedded
    );
    let rejected = EmbedderError::InputRejected("400".into());
    assert_eq!(
        settle(&engine, Err(MemoryError::from(rejected))).await,
        Settled::Rejected
    );
    let key = EmbedderError::KeyRejected("401 \u{1b}[2J".into());
    assert_eq!(
        settle(&engine, Err(MemoryError::from(key))).await,
        Settled::KeyRejected
    );
    engine.close().await.unwrap();

    cli(&db)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "embedding queue: 1 waiting, oldest 0m; 1 failed",
        ))
        .stdout(
            predicate::str::is_match(r"paused: since \S+: embedder: 401 \\u\{1b\}\[2J").unwrap(),
        )
        .stdout(predicate::str::is_match(r"last embed success: \d{4}-\S+\n").unwrap())
        .stdout(
            predicate::str::is_match(r"last embed error: \S+: embedder: 401 \\u\{1b\}\[2J")
                .unwrap(),
        )
        .stdout(predicate::str::contains('\u{1b}').not());

    let json = stdout_json(cli(&db).args(["--format", "json", "status"]));
    assert_eq!(json["waiting"], 1);
    assert_eq!(json["failed"], 1);
    assert!(json["oldest_age_secs"].as_i64().unwrap() >= 0);
    assert!(json["paused_since"].is_string());
    assert_eq!(json["pause_error"], "embedder: 401 \u{1b}[2J");
    assert!(json["last_success_at"].is_string());
    assert!(json["last_error_at"].is_string());
    assert_eq!(json["last_error"], "embedder: 401 \u{1b}[2J");
    assert_eq!(
        json["skipped"],
        serde_json::json!({}),
        "no skip yet (#1171)"
    );

    cli(&db)
        .args(["--format", "plain", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("waiting=1\n"))
        .stdout(predicate::str::contains("failed=1\n"));
}

#[tokio::test]
async fn status_of_an_idle_queue_says_never() {
    let (_dir, db, mut engine) = store(&[]).await;
    engine.close().await.unwrap();
    cli(&db)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "embedding queue: 0 waiting; 0 failed\n",
        ))
        .stdout(predicate::str::contains("paused: no\n"))
        .stdout(predicate::str::contains("last embed success: never\n"))
        .stdout(predicate::str::contains("last embed error: never\n"))
        .stdout(predicate::str::contains(
            "redaction: 0 redacted, 0 stubbed\n",
        ));
    let json = stdout_json(cli(&db).args(["--format", "json", "status"]));
    for key in [
        "oldest_age_secs",
        "paused_since",
        "pause_error",
        "last_success_at",
        "last_error_at",
        "last_error",
    ] {
        assert!(json[key].is_null(), "{key} is null");
    }
}

/// #1024: the store-wide totals of redacted secrets and stubbed activities.
#[tokio::test]
async fn status_reports_the_counters() {
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let ghp = format!("ghp_{}", "0123456789abcdefghij".repeat(2));
    let (_dir, db, mut engine) = store(&[&format!("key {aws}"), &format!("token {ghp}")]).await;
    let dotenv = format!("app/.{}", "env");
    let activity = memory_engine::RecordActivityRequest {
        tool_name: "Read".into(),
        args: serde_json::json!({ "file_path": dotenv }),
        result: None,
        session_id: "s1".into(),
        timestamp: chrono::Utc::now(),
        scope_path: None,
        outcome_class: None,
        touched_paths: vec![dotenv.clone()],
        cwd: None,
    };
    engine
        .record_activity(
            &activity,
            None,
            &memory_engine::ActivityFilterConfig::default(),
        )
        .await
        .unwrap();
    engine.close().await.unwrap();

    cli(&db)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "redaction: 2 redacted, 1 stubbed\n",
        ));
    let json = stdout_json(cli(&db).args(["--format", "json", "status"]));
    assert_eq!(json["redacted"], 2);
    assert_eq!(json["stubbed"], 1);
    cli(&db)
        .args(["--format", "plain", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("redacted=2\n"))
        .stdout(predicate::str::contains("stubbed=1\n"));
}

#[tokio::test]
async fn status_runs_beside_a_writer_and_changes_nothing() {
    let (_dir, db, mut engine) = store(&["waiting"]).await;
    engine.close().await.unwrap();
    drop(engine);

    let _held = StoreLock::acquire(&db).unwrap();
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let with = |suffix: &str| {
        let mut path = db.clone().into_os_string();
        path.push(suffix);
        PathBuf::from(path)
    };
    // A write lands in the store or its WAL. A WAL reader may itself write a read mark
    // into the shared-memory index, so that file only has to stay.
    let files = || ["", "-wal"].map(|suffix| std::fs::read(with(suffix)).ok());
    let before = files();
    assert!(before.iter().all(Option::is_some), "the WAL exists");
    cli(&db)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 waiting"));
    assert_eq!(files(), before);
    assert!(with("-shm").is_file());
    writer.execute_batch("ROLLBACK").unwrap();
}

#[tokio::test]
async fn status_never_migrates_an_older_store() {
    let (_dir, db, mut engine) = store(&[]).await;
    engine.close().await.unwrap();
    drop(engine);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE lifecycle_receipts;
             DROP TABLE lifecycle_segments;
             DROP INDEX idx_events_mcp_call;
             UPDATE config SET value = '17' WHERE key = 'schema_version';",
        )
        .unwrap();
    }
    let before = std::fs::read(&db).unwrap();

    cli(&db)
        .arg("status")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("17").and(predicate::str::contains("18")));

    assert_eq!(std::fs::read(&db).unwrap(), before);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, "17");
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'lifecycle_receipts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "never migrated");
}

#[tokio::test]
async fn status_counts_quarantined_events() {
    use memory_engine::types::lifecycle::{BodyFault, Delivery, ParseClass, ParseFailure};

    let (_dir, db, mut engine) = store(&[]).await;
    let now = chrono::Utc::now();
    for id in ["q1", "q2"] {
        let delivery = Delivery {
            machine: "laptop".into(),
            event_id: id.into(),
            time: now,
            seq: 1,
            harness: "claude-code".into(),
            client_version: "1.0.0".into(),
            body_sha256: [1; 32],
            body_size: 3,
            received_at: now,
            home: "unscoped:laptop".into(),
            provenance: serde_json::json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
        };
        let fault = BodyFault::Parse(ParseFailure {
            class: ParseClass::NotJson,
            line: 1,
            column: 1,
        });
        engine
            .record_lifecycle_event(&delivery, Err(fault), &[])
            .await
            .unwrap();
    }
    engine.close().await.unwrap();

    cli(&db)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("lifecycle: 2 quarantined\n"));
    assert_eq!(
        stdout_json(cli(&db).args(["--format", "json", "status"]))["quarantined"],
        2
    );
    cli(&db)
        .args(["--format", "plain", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("quarantined=2\n"));
}

/// The skip totals are read by reason and reach every rendering (#1171): the store's `skip.*` keys,
/// the reader beside a live server, and the CLI's three formats.
#[tokio::test]
async fn status_reports_capture_skips_by_reason() {
    use memory_engine::types::lifecycle::{
        Actor, Content, Delivery, NeutralRecord, Recorded, TASK_NOTIFICATION,
    };

    let (_dir, db, mut engine) = store(&[]).await;
    let now = chrono::Utc::now();
    let delivery = Delivery {
        machine: "laptop".into(),
        event_id: "n1".into(),
        time: now,
        seq: 1,
        harness: "claude-code".into(),
        client_version: "1.0.0".into(),
        body_sha256: [1; 32],
        body_size: 40,
        received_at: now,
        home: "unscoped:laptop".into(),
        provenance: serde_json::json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
    };
    let notification = NeutralRecord {
        session_id: "s1".into(),
        actor: Actor::Main,
        cwd: None,
        harness_version: None,
        content: Content::Prompt {
            text: Some("<task-notification>\n<task-id>t1</task-id>\n".into()),
        },
    };
    assert!(matches!(
        engine
            .record_lifecycle_event(&delivery, Ok(notification), &[])
            .await
            .unwrap(),
        Recorded::Skipped(TASK_NOTIFICATION)
    ));
    engine.close().await.unwrap();

    let json = stdout_json(cli(&db).args(["--format", "json", "status"]));
    assert_eq!(json["skipped"][TASK_NOTIFICATION], 1, "{json}");
    assert_eq!(json["quarantined"], 0, "a skip is not a quarantine: {json}");
    cli(&db)
        .args(["--format", "plain", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped.task-notification=1\n"));
    cli(&db)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped: task-notification 1"));
}
