//! `erase`: the one maintenance command that deletes (#1023, ADR-0020 §8).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use memory_engine::StoreLock;
use memory_engine::types::{AddFactOptions, AddFactRequest, EventType, FactType, NewEvent};
use predicates::prelude::*;
use tempfile::TempDir;

/// A sentinel the porter stemmer keeps whole, so a raw byte search finds every copy.
const SENTINEL: &str = "zqxjvkpwqq";

fn event(payload: serde_json::Value) -> NewEvent {
    NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: EventType::MemoryOp,
        payload,
        source: "mcp".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "mcp".into(),
        sequence_id: 0,
        created_at: None,
    }
}

/// A store with laptop's remembered fact 1 (its record is event 1), desktop's fact 2 and
/// laptop's sentinel fact 3, which was recalled and forgotten with a reason quoting it, so
/// earlier versions of its row exist in the file and its tombstone holds the sentinel.
async fn store() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.db");
    let mut engine = memory_engine::MemoryEngine::builder(4)
        .path(db.clone())
        .build()
        .unwrap();
    let fingerprint = memory_engine::EmbeddingFingerprint::new("mock", "test", 4);
    let laptop_record = engine
        .ingest_at(
            "unscoped:laptop",
            &event(serde_json::json!({ "tool": "remember", "machine": "laptop" })),
        )
        .await
        .unwrap();
    for (content, home, machine, source) in [
        (
            "the kestrel nests",
            "unscoped:laptop",
            "laptop",
            Some(laptop_record),
        ),
        ("the falcon hunts", "unscoped:desktop", "desktop", None),
        (SENTINEL, "unscoped:laptop", "laptop", None),
    ] {
        engine
            .add_fact_precomputed(
                &AddFactRequest {
                    content: content.into(),
                    fact_type: FactType::Semantic,
                    source_event_id: source,
                    scope: Some(home.into()),
                    opts: Some(AddFactOptions {
                        metadata: Some(serde_json::json!({ "machine": machine })),
                        ..Default::default()
                    }),
                },
                vec![0.1, 0.2, 0.3, 0.4],
                &fingerprint,
                None,
            )
            .await
            .unwrap();
    }
    engine
        .execute_query(&memory_engine::MemoryQuery::new().text(format!("\"{SENTINEL}\"")))
        .await
        .unwrap();
    engine
        .forget_fact(
            3,
            &["unscoped:laptop".into()],
            &format!("{SENTINEL} was wrong"),
            "unscoped:laptop",
            &event(serde_json::json!({ "tool": "forget", "machine": "laptop" })),
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

fn sql_i64(db: &Path, sql: &str) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn json_report(db: &Path, args: &[&str]) -> serde_json::Value {
    let out = cli(db)
        .args(["--format", "json", "erase"])
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).unwrap()
}

fn holds_sentinel(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .filter(|p| {
            std::fs::read(p)
                .unwrap()
                .windows(SENTINEL.len())
                .any(|w| w == SENTINEL.as_bytes())
        })
        .map(|p| p.display().to_string())
        .collect()
}

#[tokio::test]
async fn erase_of_a_forgotten_fact_leaves_no_byte_of_its_content() {
    let (dir, db) = store().await;
    assert!(
        !holds_sentinel(dir.path()).is_empty(),
        "the fixture holds it"
    );
    let report = json_report(&db, &["--fact", "3"]);
    assert_eq!(report["erased_facts"], serde_json::json!([3]));
    assert_eq!(report["purged"], true, "{report}");
    assert_eq!(holds_sentinel(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn erase_fact_keeps_a_remember_record_linkable() {
    let (_dir, db) = store().await;
    json_report(&db, &["--fact", "1"]);
    assert_eq!(sql_i64(&db, "SELECT COUNT(*) FROM events WHERE id = 1"), 1);
    let linked: String = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT json_extract(payload, '$.linked') FROM events
             WHERE source = 'maintenance' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked, r#"[{"fact":1,"source_event":1}]"#);
}

#[tokio::test]
async fn erase_machine_keeps_every_other_machine() {
    let (_dir, db) = store().await;
    let report = json_report(&db, &["--machine", "laptop"]);
    assert_eq!(report["erased_facts"], serde_json::json!([1, 3]));
    assert_eq!(report["deleted"]["events"], 2, "{report}");
    assert_eq!(sql_i64(&db, "SELECT COUNT(*) FROM facts WHERE id = 2"), 1);
    let again = json_report(&db, &["--machine", "laptop"]);
    assert!(
        again["deleted"]
            .as_object()
            .unwrap()
            .values()
            .all(|n| n == 0),
        "{again}"
    );
}

/// The store's embedding identity, for the importer.
struct Fixed;
impl memory_engine::EmbeddingProvider for Fixed {
    fn embed(&self, _: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
    fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
        memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
    }
}

#[tokio::test]
async fn erase_keeps_a_forgotten_facts_tombstone() {
    let (dir, db) = store().await;
    json_report(&db, &["--fact", "3"]);
    assert_eq!(
        sql_i64(
            &db,
            "SELECT COUNT(*) FROM fact_tombstones WHERE fact_id IS NULL"
        ),
        1
    );
    let memories = dir.path().join("md");
    std::fs::create_dir(&memories).unwrap();
    std::fs::write(memories.join("s.md"), format!("{SENTINEL}\n")).unwrap();
    let engine = memory_engine::MemoryEngine::builder(4)
        .path(db.clone())
        .build()
        .unwrap();
    let report = engine
        .bootstrap_memory_directory(
            &memories,
            std::sync::Arc::new(Fixed),
            &memory_engine::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(report.facts_blocked, 1, "the block survives the erasure");
}

#[tokio::test]
async fn erase_refuses_an_invalid_machine_name() {
    let (_dir, db) = store().await;
    cli(&db)
        .args(["erase", "--machine", "a/b"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("DNS label"));
}

#[tokio::test]
async fn erase_fact_of_a_missing_fact_exits_1_and_purges() {
    let (_dir, db) = store().await;
    cli(&db)
        .args(["erase", "--fact", "99"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("fact 99 not found; purged"));
}

#[tokio::test]
async fn erase_prints_its_copies_and_orphans() {
    let (_dir, db) = store().await;
    // Fact 1 superseded fact 2, and fact 4 is a copy of fact 1.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "UPDATE facts SET t_expired = '2026-01-01T00:00:00+00:00' WHERE id = 2;
             INSERT INTO edges (source_fact_id, target_fact_id, relation_type, t_created)
                 VALUES (1, 2, 'supersedes', '2026-01-01T00:00:00+00:00');
             INSERT INTO facts (content, content_hash, embedding, fact_type, t_created,
                                last_accessed, scope_id)
                 SELECT content, content_hash, embedding, fact_type, t_created, last_accessed,
                        scope_id
                 FROM facts WHERE id = 1;",
        )
        .unwrap();
    cli(&db)
        .args(["erase", "--fact", "1"])
        .assert()
        .success()
        .stdout(
            "erased: activities=0 checkpoints=0 edges=1 events=0 facts=1 summaries=0\n\
             copies left: [4]\n\
             orphaned (still expired): [2]\n",
        );
}

#[tokio::test]
async fn erase_needs_exactly_one_target() {
    let (_dir, db) = store().await;
    cli(&db).arg("erase").assert().failure().code(2);
    cli(&db)
        .args(["erase", "--fact", "1", "--machine", "laptop"])
        .assert()
        .failure()
        .code(2);
}

#[tokio::test]
async fn erase_refuses_while_the_store_is_held() {
    let (_dir, db) = store().await;
    {
        let _held = StoreLock::acquire(&db).unwrap();
        cli(&db)
            .args(["erase", "--machine", "laptop"])
            .assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains(
                "stop the memory-engine server first",
            ));
        assert_eq!(sql_i64(&db, "SELECT COUNT(*) FROM facts"), 3);
    }
    cli(&db)
        .args(["erase", "--machine", "laptop"])
        .assert()
        .success();
}
