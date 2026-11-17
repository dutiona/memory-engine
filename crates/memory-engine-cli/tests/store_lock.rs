//! Every writable command takes the store lock or refuses; read-only commands never take it
//! (ADR-0020 §8). The test process holds the lock exactly as a running server would.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use memory_engine::StoreLock;
use predicates::prelude::*;
use tempfile::TempDir;

const EMBEDDING: &str = "[0.1, 0.2, 0.3, 0.4]";

/// A store holding one embedded fact (id 1).
async fn store() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.db");
    let engine = memory_engine::MemoryEngine::builder(4)
        .path(db.clone())
        .build()
        .unwrap();
    engine
        .add_fact_precomputed(
            &memory_engine::AddFactRequest {
                content: "The sky is blue".into(),
                fact_type: memory_engine::FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            vec![0.1, 0.2, 0.3, 0.4],
            &memory_engine::EmbeddingFingerprint::new("mock", "test", 4),
            None,
        )
        .await
        .unwrap();
    drop(engine);
    (dir, db)
}

fn cli(db: &Path) -> Command {
    let mut cmd = Command::cargo_bin("memory-engine-cli").unwrap();
    cmd.arg("--db").arg(db);
    cmd
}

fn sql_i64(db: &Path, sql: &str) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn fact_and_event_counts(db: &Path) -> (i64, i64) {
    (
        sql_i64(db, "SELECT COUNT(*) FROM facts"),
        sql_i64(db, "SELECT COUNT(*) FROM events"),
    )
}

fn refuses_as_held(cmd: &mut Command) {
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("memory.db.store-lock"))
        .stderr(predicate::str::contains(
            "stop the memory-engine server first",
        ));
}

#[tokio::test]
async fn every_writable_command_refuses_while_the_store_is_held() {
    let (dir, db) = store().await;
    let jsonl = dir.path().join("facts.jsonl");
    std::fs::write(&jsonl, "{\"content\": \"x\"}\n").unwrap();
    let memory_dir = dir.path().join("memories");
    std::fs::create_dir(&memory_dir).unwrap();
    let embed = [
        "--embed-url",
        "http://127.0.0.1:9/v1/embeddings",
        "--embed-model",
        "m",
    ];

    let _held = StoreLock::acquire(&db).unwrap();
    let before = fact_and_event_counts(&db);
    let schema = sql_i64(
        &db,
        "SELECT CAST(value AS INTEGER) FROM config WHERE key = 'schema_version'",
    );

    refuses_as_held(cli(&db).args([
        "add-fact",
        "--content",
        "c",
        "--fact-type",
        "semantic",
        "--embedding",
        EMBEDDING,
    ]));
    refuses_as_held(cli(&db).args(["record-outcome", "--fact-id", "1", "--outcome", "positive"]));
    refuses_as_held(cli(&db).arg("consolidate"));
    refuses_as_held(cli(&db).args(["prune", "--min-importance", "1"]));
    refuses_as_held(
        cli(&db)
            .args(["batch-ingest", "--file"])
            .arg(&jsonl)
            .args(embed),
    );
    refuses_as_held(
        cli(&db)
            .args(["bootstrap", "--memory-dir"])
            .arg(&memory_dir)
            .args(embed),
    );

    assert_eq!(fact_and_event_counts(&db), before);
    assert_eq!(
        sql_i64(
            &db,
            "SELECT CAST(value AS INTEGER) FROM config WHERE key = 'schema_version'"
        ),
        schema
    );
}

#[tokio::test]
async fn migrate_refuses_while_the_store_is_held() {
    let (_dir, db) = store().await;
    let behind = i64::from(memory_engine::CURRENT_SCHEMA_VERSION) - 1;
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE config SET value = ?1 WHERE key = 'schema_version'",
            [behind.to_string()],
        )
        .unwrap();
    let _held = StoreLock::acquire(&db).unwrap();
    refuses_as_held(cli(&db).arg("migrate"));
    assert_eq!(
        sql_i64(
            &db,
            "SELECT CAST(value AS INTEGER) FROM config WHERE key = 'schema_version'"
        ),
        behind
    );
    // The dry run reads only, so it still answers.
    cli(&db)
        .args(["migrate", "--check"])
        .assert()
        .stderr(predicate::str::contains("store-lock").not());
}

#[tokio::test]
async fn import_refuses_while_its_target_is_held() {
    let (dir, db) = store().await;
    let snapshot = dir.path().join("snapshot.json");
    cli(&db)
        .arg("export")
        .arg("-o")
        .arg(&snapshot)
        .assert()
        .success();
    let target = dir.path().join("restored.db");
    let _held = StoreLock::acquire(&target).unwrap();
    cli(&target)
        .arg("import")
        .arg(&snapshot)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("restored.db.store-lock"));
    assert!(!target.exists(), "a refused import must create no database");
}

#[tokio::test]
async fn a_held_store_is_refused_before_it_is_read() {
    // A garbage store under a held lock is refused as held, not as unreadable: the lock
    // comes before the dimension peek.
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.db");
    std::fs::write(&db, b"not a database").unwrap();
    let _held = StoreLock::acquire(&db).unwrap();
    refuses_as_held(cli(&db).args(["record-outcome", "--fact-id", "1", "--outcome", "positive"]));
}

#[tokio::test]
async fn a_mistyped_path_leaves_no_lock_file() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("typo.db");
    cli(&db)
        .args(["record-outcome", "--fact-id", "1", "--outcome", "positive"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("database not found"));
    assert!(!dir.path().join("typo.db.store-lock").exists());
}

#[tokio::test]
async fn read_only_commands_run_while_the_store_is_held() {
    let (dir, db) = store().await;
    let _held = StoreLock::acquire(&db).unwrap();
    for args in [
        vec!["stats"],
        vec!["inspect", "1"],
        vec!["explain", "1"],
        vec!["query", "sky"],
        vec!["dump", "facts"],
        vec!["outcome-counts", "--fact-id", "1"],
        vec!["schema"],
    ] {
        cli(&db).args(&args).assert().success();
    }
    for (format, name) in [
        ("json", "out.json"),
        ("sqlite", "out.db"),
        ("json-gz", "out.json.gz"),
        ("json-zst", "out.json.zst"),
    ] {
        cli(&db)
            .arg("export")
            .arg("-o")
            .arg(dir.path().join(name))
            .args(["--export-format", format])
            .assert()
            .success();
    }
}

#[tokio::test]
async fn a_released_lock_lets_writes_run() {
    let (_dir, db) = store().await;
    let held = StoreLock::acquire(&db).unwrap();
    refuses_as_held(cli(&db).args(["record-outcome", "--fact-id", "1", "--outcome", "positive"]));
    drop(held);
    cli(&db)
        .args(["record-outcome", "--fact-id", "1", "--outcome", "positive"])
        .assert()
        .success();
}
