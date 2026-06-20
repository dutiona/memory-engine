// Local `FakeEmbed` structs are defined after let-bindings in test helpers —
// keeping the struct near its only use site is clearer than hoisting it to
// module level where it would be far from the code that exercises it.
#![allow(clippy::items_after_statements)]

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Create a test database with sample data and return (tempdir, `db_path`).
fn create_test_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let engine = memory_engine::MemoryEngine::builder(4)
        .path(db_path.clone())
        .build()
        .unwrap();

    struct FakeEmbed;
    impl memory_engine::EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
            memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    engine
        .add_fact(
            &memory_engine::AddFactRequest {
                content: "The sky is blue".into(),
                fact_type: memory_engine::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &memory_engine::AddFactRequest {
                content: "Rust is fast".into(),
                fact_type: memory_engine::FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &memory_engine::AddFactRequest {
                content: "Memory engines consolidate facts".into(),
                fact_type: memory_engine::FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();

    drop(engine);
    (dir, db_path)
}

fn cli() -> Command {
    Command::cargo_bin("memory-engine-cli").unwrap()
}

// --- stats ---

#[test]
fn stats_table_output() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Facts"))
        .stdout(predicate::str::contains("3"));
}

#[test]
fn stats_json_output() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "stats",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"active\""));
}

#[test]
fn stats_plain_output() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "plain",
            "stats",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("facts.active=3"));
}

#[test]
fn stats_plain_includes_max_depth_and_page_count() {
    // Seed a fact under a nested scope so max_depth > 0 is observable.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    let engine = memory_engine::MemoryEngine::builder(4)
        .path(db_path.clone())
        .build()
        .unwrap();

    struct FakeEmbed;
    impl memory_engine::EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
            memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    engine
        .add_fact(
            &memory_engine::AddFactRequest {
                content: "scoped fact".into(),
                fact_type: memory_engine::FactType::Semantic,
                source_event_id: None,
                scope: Some("project/sub".into()),
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();

    // Ground the expected max_depth via the JSON projection.
    let expected_max_depth = engine.statistics().unwrap().scopes.max_depth;
    assert!(
        expected_max_depth > 0,
        "nested scope should produce max_depth > 0"
    );
    drop(engine);

    // Plain output must include both scopes.max_depth and storage.page_count.
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "plain",
            "stats",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "scopes.max_depth={expected_max_depth}"
        )))
        .stdout(predicate::str::contains("storage.page_count="));
}

// --- inspect ---

#[test]
fn inspect_existing_fact() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "inspect", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("The sky is blue"));
}

#[test]
fn inspect_nonexistent_fact() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "inspect", "999"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

// --- explain ---

#[test]
fn explain_fact() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "explain", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fact_id"))
        .stdout(predicate::str::contains("scope"));
}

// --- query ---

#[test]
fn query_fts() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "query", "blue sky"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sky"));
}

#[test]
fn query_no_results() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "query", "xyznonexistent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

// --- query: filter flags (#430) ---

#[test]
fn query_filter_fact_type() {
    let (_dir, db_path) = create_test_db();
    let db = db_path.to_str().unwrap();
    // "Rust is fast" is Procedural: it passes --fact-type procedural …
    cli()
        .args(["--db", db, "query", "Rust", "--fact-type", "procedural"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust"));
    // … but --fact-type semantic filters it out, leaving no FTS match.
    cli()
        .args(["--db", db, "query", "Rust", "--fact-type", "semantic"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
    // A second variant end-to-end: episodic matches "Memory engines …".
    cli()
        .args(["--db", db, "query", "Memory", "--fact-type", "episodic"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Memory engines"));
    // Case-insensitive (ignore_case preserves the pre-#270 query behavior).
    cli()
        .args(["--db", db, "query", "Rust", "--fact-type", "PROCEDURAL"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust"));
}

#[test]
fn query_filter_fact_type_unknown_is_rejected() {
    let (_dir, db_path) = create_test_db();
    // After #270 the fact type is a clap ValueEnum, so an unknown value is
    // rejected at parse time rather than via an anyhow bail in the command body.
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "query",
            "x",
            "--fact-type",
            "unknown",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("invalid value").and(predicate::str::contains("--fact-type")),
        );
}

#[test]
fn query_filter_pinned_only_excludes_unpinned() {
    let (_dir, db_path) = create_test_db();
    let db = db_path.to_str().unwrap();
    // Baseline: the query returns the fact when the filter is absent …
    cli()
        .args(["--db", db, "query", "blue sky"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sky"));
    // … and the seeded facts are not pinned, so --pinned-only removes it.
    cli()
        .args(["--db", db, "query", "blue sky", "--pinned-only"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn query_filter_min_importance_excludes_below_threshold() {
    let (_dir, db_path) = create_test_db();
    let db = db_path.to_str().unwrap();
    cli()
        .args(["--db", db, "query", "blue sky"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sky"));
    // Seeded facts use the default importance (0.5), below the 0.99 threshold.
    cli()
        .args(["--db", db, "query", "blue sky", "--min-importance", "0.99"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn query_filter_scope_subtree_excludes_other_scopes() {
    let (_dir, db_path) = create_test_db();
    let db = db_path.to_str().unwrap();
    cli()
        .args(["--db", db, "query", "blue sky"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sky"));
    // Seeded facts live at the root scope; a subtree filter excludes them.
    cli()
        .args(["--db", db, "query", "blue sky", "--scope", "project/sub"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn query_filter_combined_fact_type_and_min_importance() {
    let (_dir, db_path) = create_test_db();
    let db = db_path.to_str().unwrap();
    // --fact-type procedural alone matches "Rust is fast" …
    cli()
        .args(["--db", db, "query", "Rust", "--fact-type", "procedural"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust"));
    // … adding --min-importance 0.99 removes it, proving the filters AND
    // together rather than one masking the other.
    cli()
        .args([
            "--db",
            db,
            "query",
            "Rust",
            "--fact-type",
            "procedural",
            "--min-importance",
            "0.99",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

// --- query: --valid-at temporal filtering ---

/// Create a test database with facts that have explicit `t_valid` / `t_invalid`
/// temporal bounds, for testing --valid-at filtering.
fn create_temporal_db() -> (TempDir, PathBuf) {
    use chrono::{TimeZone, Utc};

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("temporal.db");

    let engine = memory_engine::MemoryEngine::builder(4)
        .path(db_path.clone())
        .build()
        .unwrap();

    struct FakeEmbed;
    impl memory_engine::EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
            memory_engine::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    // Fact 1: valid from March 1 to March 31
    engine
        .add_fact(
            &memory_engine::AddFactRequest {
                content: "March event: spring equinox observed".into(),
                fact_type: memory_engine::FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(memory_engine::AddFactOptions {
                    t_valid: Some(Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap()),
                    t_invalid: Some(Utc.with_ymd_and_hms(2026, 3, 31, 0, 0, 0).unwrap()),
                    ..Default::default()
                }),
            },
            &FakeEmbed,
            None,
        )
        .unwrap();

    // Fact 2: valid from April 1, no end (still valid)
    engine
        .add_fact(
            &memory_engine::AddFactRequest {
                content: "April event: daylight saving adjustment".into(),
                fact_type: memory_engine::FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(memory_engine::AddFactOptions {
                    t_valid: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()),
                    ..Default::default()
                }),
            },
            &FakeEmbed,
            None,
        )
        .unwrap();

    // Fact 3: no temporal bounds (always valid)
    engine
        .add_fact(
            &memory_engine::AddFactRequest {
                content: "Timeless event: gravity exists".into(),
                fact_type: memory_engine::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();

    drop(engine);
    (dir, db_path)
}

#[test]
fn query_valid_at_filters_to_march() {
    let (_dir, db_path) = create_temporal_db();
    // March 15 should match: March event (in range) + timeless (no bounds)
    // but NOT April event (t_valid = April 1 > March 15)
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "query",
            "event",
            "--valid-at",
            "2026-03-15T00:00:00Z",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("spring equinox"))
        .stdout(predicate::str::contains("gravity"))
        .stdout(predicate::str::contains("daylight").not());
}

#[test]
fn query_valid_at_filters_to_april() {
    let (_dir, db_path) = create_temporal_db();
    // April 5 should match: April event (in range) + timeless (no bounds)
    // but NOT March event (t_invalid = March 31 <= April 5)
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "query",
            "event",
            "--valid-at",
            "2026-04-05T00:00:00Z",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("daylight"))
        .stdout(predicate::str::contains("gravity"))
        .stdout(predicate::str::contains("spring equinox").not());
}

#[test]
fn query_valid_at_invalid_format_fails() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "query",
            "sky",
            "--valid-at",
            "not-a-date",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid RFC 3339"));
}

#[test]
fn query_table_output_shows_temporal_columns() {
    let (_dir, db_path) = create_temporal_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "query", "event"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Valid"))
        .stdout(predicate::str::contains("Invalid"));
}

// --- migrate / schema (release-gate verify hook) ---

/// Create a current-schema DB, then roll its recorded `schema_version` back one
/// version to simulate a stale database. The on-disk schema is already current, but
/// the migration is idempotent (`CREATE INDEX IF NOT EXISTS`), so `migrate` re-applies
/// it cleanly — exercising the version reading, pending computation, and exit codes.
fn create_stale_db() -> (TempDir, PathBuf) {
    let (dir, db_path) = create_test_db();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let prev = memory_engine::CURRENT_SCHEMA_VERSION.saturating_sub(1);
    conn.execute(
        "UPDATE config SET value = ?1 WHERE key = 'schema_version'",
        [prev.to_string()],
    )
    .unwrap();
    (dir, db_path)
}

#[test]
fn schema_up_to_date_exits_zero() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "schema"])
        .assert()
        .success()
        .stdout(predicate::str::contains("up to date"));
}

#[test]
fn migrate_check_up_to_date_exits_zero() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "migrate", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no pending"));
}

#[test]
fn schema_mismatch_exits_nonzero() {
    let (_dir, db_path) = create_stale_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "schema"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("MISMATCH"));
}

#[test]
fn migrate_check_pending_exits_nonzero_without_mutating() {
    let (_dir, db_path) = create_stale_db();
    let current = memory_engine::CURRENT_SCHEMA_VERSION.to_string();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "migrate", "--check"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("pending migrations"))
        .stdout(predicate::str::contains(current));
    // --check must NOT mutate — the DB is still stale.
    cli()
        .args(["--db", db_path.to_str().unwrap(), "schema"])
        .assert()
        .failure();
}

#[test]
fn migrate_applies_pending_then_schema_matches() {
    let (_dir, db_path) = create_stale_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "migrate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("migrated"));
    cli()
        .args(["--db", db_path.to_str().unwrap(), "schema"])
        .assert()
        .success()
        .stdout(predicate::str::contains("up to date"));
}

#[test]
fn schema_json_output() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "--format",
            "json",
            "schema",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"matches\""))
        .stdout(predicate::str::contains("\"current_schema_version\""));
}

#[test]
fn migrate_check_newer_db_exits_nonzero() {
    let (_dir, db_path) = create_test_db();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let future = (memory_engine::CURRENT_SCHEMA_VERSION + 1).to_string();
    conn.execute(
        "UPDATE config SET value = ?1 WHERE key = 'schema_version'",
        [future],
    )
    .unwrap();
    drop(conn);

    // A DB newer than the binary is forward-incompatible: both `migrate --check` and
    // `schema` must signal non-zero (a release gate must not treat it as "nothing to do").
    cli()
        .args(["--db", db_path.to_str().unwrap(), "migrate", "--check"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("NEWER"));
    cli()
        .args(["--db", db_path.to_str().unwrap(), "schema"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("MISMATCH"));
}

// --- export + import roundtrip ---

#[test]
fn export_import_roundtrip() {
    let (dir, db_path) = create_test_db();

    // Export
    let export_path = dir.path().join("export.json");
    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "export",
            "-o",
            export_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(export_path.exists());

    // Import into new DB
    let new_db = dir.path().join("imported.db");
    cli()
        .args([
            "--db",
            new_db.to_str().unwrap(),
            "import",
            export_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Verify imported DB has same data
    cli()
        .args([
            "--db",
            new_db.to_str().unwrap(),
            "--format",
            "plain",
            "stats",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("facts.active=3"));
}

// --- dump ---

#[test]
fn dump_facts() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "dump", "facts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Active Facts"));
}

#[test]
fn dump_events() {
    let (_dir, db_path) = create_test_db();
    cli()
        .args(["--db", db_path.to_str().unwrap(), "dump", "events"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Events"));
}

// --- error handling ---

#[test]
fn missing_db_flag() {
    cli()
        .args(["stats"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--db"));
}

#[test]
fn nonexistent_db() {
    cli()
        .args(["--db", "/tmp/nonexistent_memory_engine.db", "stats"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn import_rejects_existing_db() {
    let (dir, db_path) = create_test_db();
    let fake_snapshot = dir.path().join("fake.json");
    std::fs::write(&fake_snapshot, "{}").unwrap();

    cli()
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "import",
            fake_snapshot.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}
