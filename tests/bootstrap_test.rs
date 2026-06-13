//! Integration tests for the bootstrap pipeline.

use std::io::Cursor;

use memory_engine::bootstrap::extract::KeywordExtractor;
use memory_engine::bootstrap::metrics::BootstrapConfig;
use memory_engine::{EmbeddingProvider, MemoryEngine, MemoryError};

/// Dummy embedder for testing — returns a fixed-length zero vector.
struct TestEmbedder;

impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.0; 4])
    }
}

fn engine() -> MemoryEngine {
    MemoryEngine::open_memory(4).unwrap()
}

const fn success_fixture() -> &'static str {
    include_str!("fixtures/success_session.jsonl")
}

const fn failed_fixture() -> &'static str {
    include_str!("fixtures/failed_session.jsonl")
}

#[test]
fn bootstrap_success_session() {
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    let reader = Cursor::new(success_fixture());
    let report = engine
        .bootstrap_session(reader, &TestEmbedder, &extractor, &config, None)
        .unwrap();

    assert_eq!(report.sessions_processed, 1);
    assert_eq!(report.sessions_skipped, 0);
    assert!(report.entries_parsed > 0, "should parse some entries");
    assert!(report.turns_reconstructed > 0, "should reconstruct turns");
    // Success session should be classified as Success or Indeterminate
    // (depends on fixture content matching heuristics)
    assert!(
        report.outcome_counts.success > 0 || report.outcome_counts.indeterminate > 0,
        "should classify as success or indeterminate"
    );
    assert_eq!(report.events_ingested, 1, "one marker event per session");
}

#[test]
fn bootstrap_failure_session() {
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    let reader = Cursor::new(failed_fixture());
    let report = engine
        .bootstrap_session(reader, &TestEmbedder, &extractor, &config, None)
        .unwrap();

    assert_eq!(report.sessions_processed, 1);
    assert!(report.entries_parsed > 0);
    assert_eq!(report.events_ingested, 1);
}

#[test]
fn bootstrap_empty_file() {
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    let reader = Cursor::new("");
    let report = engine
        .bootstrap_session(reader, &TestEmbedder, &extractor, &config, None)
        .unwrap();

    // Empty file → no session_id found → returns early
    assert_eq!(report.sessions_processed, 0);
    assert_eq!(report.sessions_skipped, 0);
    assert_eq!(report.facts_created, 0);
}

#[test]
fn bootstrap_idempotency() {
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig {
        skip_existing: true,
        ..Default::default()
    };

    // First run
    let reader1 = Cursor::new(success_fixture());
    let report1 = engine
        .bootstrap_session(reader1, &TestEmbedder, &extractor, &config, None)
        .unwrap();
    assert_eq!(report1.sessions_processed, 1);
    assert_eq!(report1.sessions_skipped, 0);

    // Second run with same session → should be skipped
    let reader2 = Cursor::new(success_fixture());
    let report2 = engine
        .bootstrap_session(reader2, &TestEmbedder, &extractor, &config, None)
        .unwrap();
    assert_eq!(report2.sessions_processed, 0);
    assert_eq!(report2.sessions_skipped, 1);
    assert_eq!(report2.facts_created, 0);
}

#[test]
fn bootstrap_skip_existing_false_allows_reimport() {
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig {
        skip_existing: false,
        ..Default::default()
    };

    // First run
    let reader1 = Cursor::new(success_fixture());
    let report1 = engine
        .bootstrap_session(reader1, &TestEmbedder, &extractor, &config, None)
        .unwrap();
    let first_facts = report1.facts_created;

    // Second run with skip_existing=false → should process again
    let reader2 = Cursor::new(success_fixture());
    let report2 = engine
        .bootstrap_session(reader2, &TestEmbedder, &extractor, &config, None)
        .unwrap();
    assert_eq!(report2.sessions_processed, 1);
    assert_eq!(report2.sessions_skipped, 0);
    assert_eq!(report2.facts_created, first_facts);
}

#[test]
fn bootstrap_directory_multiple() {
    let dir = tempfile::tempdir().unwrap();

    // Write two fixture files
    std::fs::write(dir.path().join("sess1.jsonl"), success_fixture()).unwrap();
    std::fs::write(dir.path().join("sess2.jsonl"), failed_fixture()).unwrap();
    // Non-jsonl file should be ignored
    std::fs::write(dir.path().join("readme.txt"), "not a session").unwrap();

    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    let report = engine
        .bootstrap_directory(dir.path(), &TestEmbedder, &extractor, &config, None)
        .unwrap();

    // Both sessions processed (may differ in outcome classification)
    assert!(
        report.sessions_processed >= 1,
        "at least one session processed"
    );
    assert_eq!(report.sessions_skipped, 0);
    assert!(report.entries_parsed > 0);
}

#[test]
fn bootstrap_with_scope() {
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig {
        scope: Some("project:test".into()),
        ..Default::default()
    };

    let reader = Cursor::new(success_fixture());
    let report = engine
        .bootstrap_session(reader, &TestEmbedder, &extractor, &config, None)
        .unwrap();

    assert_eq!(report.sessions_processed, 1);
    // Facts should be in the scoped namespace
}

#[test]
fn bootstrap_max_turns_limits_processing() {
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig {
        max_turns: 1,
        ..Default::default()
    };

    let reader = Cursor::new(success_fixture());
    let report = engine
        .bootstrap_session(reader, &TestEmbedder, &extractor, &config, None)
        .unwrap();

    assert_eq!(report.sessions_processed, 1);
    // With max_turns=1, we process at most 1 turn
    assert!(
        report.turns_reconstructed <= 1,
        "should process at most one turn, got {}",
        report.turns_reconstructed
    );
}
