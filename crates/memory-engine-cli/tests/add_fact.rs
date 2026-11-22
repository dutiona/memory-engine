use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const EMBED_DIM: usize = 4;
const EMBEDDING_JSON: &str = "[0.1, 0.2, 0.3, 0.4]";

/// Create a test database (empty, with schema initialized) and return (tempdir, `db_path`).
fn create_empty_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let _engine = memory_engine::MemoryEngine::builder(EMBED_DIM)
        .path(db_path.clone())
        .build()
        .unwrap();
    (dir, db_path)
}

/// Create a test database with a seeded event for `source_event_id` tests.
async fn create_db_with_event() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let engine = memory_engine::MemoryEngine::builder(EMBED_DIM)
        .path(db_path.clone())
        .build()
        .unwrap();

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
        .await
        .unwrap();

    drop(engine);
    (dir, db_path)
}

/// The embedder settings `add-fact`'s identity flags read from the environment: each test
/// clears them, so a host's own configuration never declares an identity for it.
const EMBED_ENV: [&str; 8] = [
    "MEMORY_ENGINE_EMBED_URL",
    "MEMORY_ENGINE_EMBED_MODEL",
    "MEMORY_ENGINE_EMBED_PROVIDER",
    "MEMORY_ENGINE_EMBED_API_KEY",
    "MEMORY_ENGINE_EMBED_ARTIFACT_SHA256",
    "MEMORY_ENGINE_EMBED_POOLING",
    "MEMORY_ENGINE_EMBED_NORMALIZATION",
    "MEMORY_ENGINE_EMBED_MAX_INPUT_TOKENS",
];

fn cli() -> Command {
    let mut cmd = Command::cargo_bin("memory-engine-cli").unwrap();
    for var in EMBED_ENV {
        cmd.env_remove(var);
    }
    cmd
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
            "--base-importance",
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
        .stdout(predicate::str::contains("\"base_importance\": 0.9"));
}

#[tokio::test]
async fn add_fact_with_source_event_id() {
    let (_dir, db_path) = create_db_with_event().await;
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
            "--base-importance",
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
            "--embed-model",
            "test-model",
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
    // Under #613 the dimension is established on the first embedding write, not at
    // DB creation. First establish dim=4 with a valid embedding...
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "add-fact",
            "--content",
            "seed",
            "--fact-type",
            "semantic",
            "--embedding",
            EMBEDDING_JSON, // 4-dim
            "--embed-model",
            "test-model",
        ])
        .assert()
        .success();
    // ...then a 2-dim embedding disagrees with the recorded identity → rejected.
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
            "[0.1, 0.2]", // 2-dim, store is dim 4
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
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
            "--embed-model",
            "test-model",
            "--t-valid",
            "not-a-date",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid"));
}

/// #1024 (decision 7): the operator's vector was computed from `--content`, so a content that
/// redaction would change is refused and nothing is stored; clean content still goes in.
#[test]
fn add_fact_refuses_a_vector_beside_unredacted_content() {
    let (_dir, db_path) = create_empty_db();
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let add = |content: &str| {
        cli()
            .args([
                "--db",
                db_path.to_str().unwrap(),
                "--format",
                "plain",
                "add-fact",
            ])
            .args(["--content", content, "--fact-type", "semantic"])
            .args(["--embedding", EMBEDDING_JSON, "--embed-model", "test-model"])
            .assert()
    };
    add(&format!("deploy key {aws}"))
        .failure()
        .stderr(predicate::str::contains("redaction would change the text"));
    add("deploy key rotated").success();
    let count = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert_eq!(count, 1);
}

// --- #1060: a pre-computed vector declares its identity ---

const SHA: &str = "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439";

/// `add-fact` of one fact into `db` with the given identity flags.
fn add_declared(
    db: &std::path::Path,
    content: &str,
    identity: &[&str],
) -> assert_cmd::assert::Assert {
    cli()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--format",
            "plain",
            "add-fact",
        ])
        .args(["--content", content, "--fact-type", "semantic"])
        .args(["--embedding", EMBEDDING_JSON])
        .args(identity)
        .assert()
}

/// The active embedding space: `(model, provider, dim, artifact_sha256)`, if any.
fn active_space(db: &std::path::Path) -> Option<(String, String, i64, Option<String>)> {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(
        "SELECT model, provider, dim, artifact_sha256 FROM embedding_spaces WHERE status = 'active'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .ok()
}

fn fact_count(db: &std::path::Path) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn add_fact_records_its_declared_identity_on_a_blank_store() {
    let (_dir, db) = create_empty_db();
    add_declared(&db, "one", &["--embed-model", "m", "--embed-provider", "p"]).success();
    assert_eq!(active_space(&db), Some(("m".into(), "p".into(), 4, None)));
}

#[test]
fn add_fact_records_a_declared_served_artifact() {
    let (_dir, db) = create_empty_db();
    add_declared(
        &db,
        "one",
        &[
            "--embed-model",
            "m",
            "--embed-provider",
            "p",
            "--embed-artifact-sha256",
            SHA,
            "--embed-pooling",
            "last",
            "--embed-normalization",
            "l2",
            "--embed-max-input-tokens",
            "2048",
        ],
    )
    .success();
    assert_eq!(
        active_space(&db),
        Some(("m".into(), "p".into(), 4, Some(SHA.into())))
    );
}

#[test]
fn add_fact_refuses_an_undeclared_vector() {
    let (_dir, blank) = create_empty_db();
    add_declared(&blank, "one", &[])
        .failure()
        .stderr(predicate::str::contains("--embed-model"));
    assert_eq!((fact_count(&blank), active_space(&blank)), (0, None));

    let (_dir2, stamped) = create_empty_db();
    add_declared(&stamped, "one", &["--embed-model", "m"]).success();
    add_declared(&stamped, "two", &[])
        .failure()
        .stderr(predicate::str::contains("--embed-model"));
    assert_eq!(fact_count(&stamped), 1);
}

#[test]
fn add_fact_accepts_a_declaration_the_store_records() {
    let (_dir, db) = create_empty_db();
    add_declared(&db, "one", &["--embed-model", "m", "--embed-provider", "p"]).success();
    add_declared(&db, "two", &["--embed-model", "m", "--embed-provider", "p"]).success();
    assert_eq!(fact_count(&db), 2);
}

#[test]
fn add_fact_refuses_a_declaration_the_store_does_not_record() {
    let (_dir, db) = create_empty_db();
    add_declared(&db, "one", &["--embed-model", "m", "--embed-provider", "p"]).success();
    let before = std::fs::read(&db).unwrap();
    add_declared(
        &db,
        "two",
        &["--embed-model", "other", "--embed-provider", "p"],
    )
    .failure()
    .stderr(predicate::str::contains("embedding model mismatch"));
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "a refused add-fact leaves the store as it was"
    );
    assert_eq!(fact_count(&db), 1);
}
