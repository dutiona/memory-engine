use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const EMBED_DIM: usize = 4;
const EMBEDDING_JSON: &str = "[0.1, 0.2, 0.3, 0.4]";

/// Create a test database (empty, with schema initialized) and return (tempdir, db_path).
fn create_empty_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let config = memory_engine::EngineConfig::new(db_path.clone(), EMBED_DIM);
    let _engine = memory_engine::MemoryEngine::open(&config).unwrap();
    (dir, db_path)
}

/// Create a test database with a seeded event for source_event_id tests.
fn create_db_with_event() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let config = memory_engine::EngineConfig::new(db_path.clone(), EMBED_DIM);
    let engine = memory_engine::MemoryEngine::open(&config).unwrap();

    engine
        .ingest(&memory_engine::NewEvent {
            timestamp: chrono::Utc::now(),
            event_type: memory_engine::EventType::Interaction,
            payload: serde_json::json!({"test": true}),
            source: "test".into(),
            session_id: None,
            scope_id: 1,
            origin_node_id: "test-node".into(),
            sequence_id: 1,
            created_at: None,
        })
        .unwrap();

    drop(engine);
    (dir, db_path)
}

fn cli() -> Command {
    Command::cargo_bin("memory-engine-cli").unwrap()
}

// --- Happy paths ---

#[test]
fn add_fact_happy_path_plain() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "plain",
            "add-fact",
            "--content",
            "The sky is blue",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^\d+\n$").unwrap());
}

#[test]
fn add_fact_happy_path_table() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "The sky is blue",
            "--fact-type",
            "episodic",
            "--embedding",
            EMBEDDING_JSON,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Created fact"))
        .stderr(predicate::str::contains("episodic"));
}

#[test]
fn add_fact_json_output() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "add-fact",
            "--content",
            "Rust is fast",
            "--fact-type",
            "procedural",
            "--embedding",
            EMBEDDING_JSON,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"content\""))
        .stdout(predicate::str::contains("Rust is fast"))
        .stdout(predicate::str::contains("\"fact_type\""))
        .stdout(predicate::str::contains("\"id\""));
}

#[test]
fn add_fact_with_optional_flags() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "add-fact",
            "--content",
            "User moved to Istanbul",
            "--fact-type",
            "episodic",
            "--embedding",
            EMBEDDING_JSON,
            "--importance",
            "0.9",
            "--t-valid",
            "2026-03-01T00:00:00Z",
            "--t-invalid",
            "2026-06-01T00:00:00Z",
            "--scope",
            "beam/experiment",
            "--metadata",
            r#"{"source":"beam","trial":1}"#,
            "--pinned",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"is_pinned\": true"))
        .stdout(predicate::str::contains("\"importance\": 0.9"));
}

#[test]
fn add_fact_with_source_event_id() {
    let (_dir, db_path) = create_db_with_event();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "add-fact",
            "--content",
            "Event-linked fact",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
            "--source-event-id",
            "1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"source_event_id\": 1"));
}

// --- Validation errors ---

#[test]
fn add_fact_invalid_fact_type() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "invalid",
            "--embedding",
            EMBEDDING_JSON,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn add_fact_importance_out_of_range() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
            "--importance",
            "1.5",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("importance must be in [0, 1]"));
}

#[test]
fn add_fact_temporal_inconsistency() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
            "--t-valid",
            "2026-06-01T00:00:00Z",
            "--t-invalid",
            "2026-03-01T00:00:00Z",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("t-valid"));
}

#[test]
fn add_fact_embedding_dimension_mismatch() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            "[0.1, 0.2]", // 2-dim, DB expects 4
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn add_fact_missing_embedding() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--embedding"));
}

#[test]
fn add_fact_malformed_embedding() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            "[not, valid, json]",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid embedding JSON"));
}

#[test]
fn add_fact_nonexistent_db() {
    cli()
        .args([
            "--db",
            "/tmp/nonexistent_add_fact_test.db",
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn add_fact_metadata_non_object() {
    let (_dir, db_path) = create_empty_db();
    // String scalar
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
            "--metadata",
            "\"just a string\"",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("metadata must be a JSON object"));

    // Array
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
            "--metadata",
            "[1, 2, 3]",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("metadata must be a JSON object"));
}

#[test]
fn add_fact_scope_creates_segments() {
    let (_dir, db_path) = create_empty_db();

    // Add fact with a nested scope path
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "plain",
            "add-fact",
            "--content",
            "Scoped fact",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
            "--scope",
            "project/experiment",
        ])
        .assert()
        .success();

    // Verify the fact exists and inspect it
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "inspect",
            "1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scoped fact"));
}

#[test]
fn add_fact_invalid_datetime() {
    let (_dir, db_path) = create_empty_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "test",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON,
            "--t-valid",
            "not-a-date",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid"));
}
