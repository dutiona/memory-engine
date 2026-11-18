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
        .stdout(predicate::str::contains("last embed error: never\n"));
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
            "DROP INDEX idx_embed_queue_due;
             DROP TABLE embed_queue;
             UPDATE config SET value = '16' WHERE key = 'schema_version';",
        )
        .unwrap();
    }
    let before = std::fs::read(&db).unwrap();

    cli(&db)
        .arg("status")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("16").and(predicate::str::contains("17")));

    assert_eq!(std::fs::read(&db).unwrap(), before);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, "16");
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'embed_queue'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "never migrated");
}
