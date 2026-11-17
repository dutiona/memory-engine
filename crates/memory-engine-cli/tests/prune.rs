//! `prune`: decay pruning as a maintenance command (ADR-0020 §8).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use memory_engine::StoreLock;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use predicates::prelude::*;
use tempfile::TempDir;

/// Facts: an Episodic one (id 1), a Semantic one (id 2), a pinned Episodic one (id 3).
async fn store() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.db");
    let engine = memory_engine::MemoryEngine::builder(4)
        .path(db.clone())
        .build()
        .unwrap();
    let fingerprint = memory_engine::EmbeddingFingerprint::new("mock", "test", 4);
    for (content, fact_type, pinned) in [
        ("an episode", FactType::Episodic, None),
        ("a fact about the world", FactType::Semantic, None),
        ("a pinned episode", FactType::Episodic, Some(true)),
    ] {
        engine
            .add_fact_precomputed(
                &AddFactRequest {
                    content: content.into(),
                    fact_type,
                    source_event_id: None,
                    scope: None,
                    opts: Some(AddFactOptions {
                        pinned,
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
    drop(engine);
    (dir, db)
}

fn cli(db: &Path) -> Command {
    let mut cmd = Command::cargo_bin("memory-engine-cli").unwrap();
    cmd.arg("--db").arg(db);
    cmd
}

fn expired_ids(db: &Path) -> Vec<i64> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT id FROM facts WHERE t_expired IS NOT NULL ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn prune_expires_what_decays_below_the_threshold_and_keeps_the_rest() {
    let (_dir, db) = store().await;
    let out = cli(&db)
        .args(["--format", "json", "prune", "--min-importance", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(report["facts_expired"], 1, "{report}");
    assert!(report["facts_evaluated"].as_u64().unwrap() >= 1, "{report}");
    // The Semantic fact is decay-exempt and the pinned one immune.
    assert_eq!(expired_ids(&db), vec![1]);
}

#[tokio::test]
async fn prune_with_the_default_policy_reports_its_counts() {
    let (_dir, db) = store().await;
    cli(&db)
        .arg("prune")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^expired \d+ of \d+ facts evaluated\n$").unwrap());
    cli(&db)
        .args(["--format", "plain", "prune"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^facts_expired=\d+\nfacts_evaluated=\d+\n$").unwrap());
}

#[tokio::test]
async fn prune_refuses_an_invalid_policy_before_the_store_opens() {
    let (_dir, db) = store().await;
    for (args, fault) in [
        (vec!["--half-life-days", "0"], "half_life_days"),
        (vec!["--min-importance", "2"], "min_importance"),
        (vec!["--half-life-override", "episodic=0"], "half_life for"),
    ] {
        cli(&db)
            .arg("prune")
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(fault));
    }
    assert_eq!(expired_ids(&db), Vec::<i64>::new());
}

#[tokio::test]
async fn prune_refuses_while_the_store_is_held() {
    let (_dir, db) = store().await;
    let held = StoreLock::acquire(&db).unwrap();
    cli(&db)
        .args(["prune", "--min-importance", "1"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("memory.db.store-lock"))
        .stderr(predicate::str::contains(
            "stop the memory-engine server first",
        ));
    assert!(
        expired_ids(&db).is_empty(),
        "a refused prune expires nothing"
    );
    drop(held);
    cli(&db)
        .args(["prune", "--min-importance", "1"])
        .assert()
        .success();
    assert_eq!(expired_ids(&db), vec![1]);
}
