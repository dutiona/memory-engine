//! Integration tests for the bootstrap pipeline.

use std::io::Cursor;

use chrono::Utc;
use memory_engine::{
    BootstrapConfig, EmbeddingFingerprint, EmbeddingProvider, KeywordExtractor, MemoryEngine,
    MemoryError,
};

/// Dummy embedder for testing — returns a fixed-length zero vector.
struct TestEmbedder;

impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.0; 4])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", 4)
    }
}

fn engine() -> MemoryEngine {
    MemoryEngine::builder(4).build().unwrap()
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
fn bootstrap_leaves_t_valid_none_visible_but_unscheduled() {
    // #521: bootstrap intentionally leaves t_valid = None (valid-time is not externally
    // asserted for retro-observed facts; t_created carries the recency signal). Pin the
    // observable consequences: such facts are visible to active-at queries (None =
    // unbounded-valid) but are NOT scheduled by list_due (which requires t_valid IS NOT NULL).
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    let reader = Cursor::new(success_fixture());
    let report = engine
        .bootstrap_session(reader, &TestEmbedder, &extractor, &config, None)
        .unwrap();
    assert!(report.facts_created > 0, "fixture should create facts");

    // Visible, and every bootstrapped fact carries t_valid = None.
    let active = engine.list_active_facts(None).unwrap();
    assert_eq!(
        active.len(),
        report.facts_created,
        "all bootstrapped facts should be active/visible"
    );
    assert!(
        active.iter().all(|f| f.t_valid.is_none()),
        "bootstrap must leave t_valid = None on every created fact"
    );

    // Unscheduled: list_due requires t_valid IS NOT NULL, so none are due.
    let due = engine.list_due(Utc::now(), None).unwrap();
    assert!(
        due.is_empty(),
        "facts with t_valid = None must not be scheduled by list_due; got {} due",
        due.len()
    );
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
    // Re-processed (not skipped), but dedup-with-reinforcement (#520) reinforces the
    // already-stored facts instead of duplicating them.
    assert_eq!(
        report2.facts_created, 0,
        "re-import creates no new rows — the facts already exist"
    );
    assert_eq!(
        report2.facts_reinforced, first_facts,
        "re-import reinforces the existing facts"
    );
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
fn bootstrap_directory_recurses_and_skips_subagents() {
    // Real transcripts live one level down (`<project-slug>/<uuid>.jsonl`), and
    // `subagents/` holds lower-value subagent logs we exclude. Regression: a flat
    // top-level scan silently found nothing when pointed at `~/.claude/projects`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("proj/subagents")).unwrap();
    // Nested main transcript — must be discovered by recursion.
    std::fs::write(dir.path().join("proj/main.jsonl"), success_fixture()).unwrap();
    // Subagent transcript with a DISTINCT session id — must be skipped, so it
    // does not add to sessions_processed even though it would yield a fact.
    let subagent = "{\"type\":\"user\",\"sessionId\":\"subagent-xyz\",\"timestamp\":\"2024-03-01T10:00:00Z\",\"uuid\":\"sa-0\",\"parentUuid\":null,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Fix the bug in parser.rs\"}]}}\n{\"type\":\"assistant\",\"sessionId\":\"subagent-xyz\",\"timestamp\":\"2024-03-01T10:00:30Z\",\"uuid\":\"sa-1\",\"parentUuid\":\"sa-0\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Found the root cause and applied the fix; tests pass.\"}]}}\n";
    std::fs::write(dir.path().join("proj/subagents/sub.jsonl"), subagent).unwrap();

    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    let report = engine
        .bootstrap_directory(dir.path(), &TestEmbedder, &extractor, &config, None)
        .unwrap();

    assert_eq!(
        report.sessions_processed, 1,
        "nested main.jsonl imported (recursion); subagents/ excluded"
    );
    assert!(report.entries_parsed > 0);
}

#[test]
fn redaction_runs_before_extraction() {
    // #45/#51 (review P1): a pluggable SessionExtractor (the public API supports
    // LLM-powered extractors reaching external services) must receive ALREADY
    // REDACTED turns. Plant a secret in a turn, record what the extractor sees,
    // and assert the secret never reaches it.
    use std::sync::Mutex;

    use memory_engine::bootstrap::{
        CandidateEpisode, ExtractedFact, SessionExtractor, SessionOutcome,
    };

    const PLANTED: &str = "AKIAIOSFODNN7EXAMPLE";

    #[derive(Default)]
    struct RecordingExtractor {
        seen: Mutex<Vec<String>>,
    }
    impl SessionExtractor for RecordingExtractor {
        fn extract(
            &self,
            episode: &CandidateEpisode,
            _outcome: &SessionOutcome,
        ) -> memory_engine::Result<Vec<ExtractedFact>> {
            for turn in &episode.turns {
                self.seen.lock().unwrap().push(turn.user_text.clone());
                self.seen.lock().unwrap().push(turn.assistant_text.clone());
            }
            Ok(vec![]) // we only care about the extractor's INPUT
        }
    }

    let jsonl = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user", "sessionId": "redact-extract", "timestamp": "2024-02-01T10:00:00Z",
            "uuid": "r-0", "parentUuid": serde_json::Value::Null,
            "message": {"role": "user", "content": [{"type": "text", "text": "Fix the bug in parser.rs"}]}
        }),
        serde_json::json!({
            "type": "assistant", "sessionId": "redact-extract", "timestamp": "2024-02-01T10:00:30Z",
            "uuid": "r-1", "parentUuid": "r-0",
            "message": {"role": "assistant", "content": [{"type": "text",
                "text": format!("Found the root cause and applied the fix; tests pass. Token {PLANTED} leaked.")}]}
        }),
    );

    let engine = engine();
    let recorder = RecordingExtractor::default();
    let config = BootstrapConfig::default(); // redact = true

    engine
        .bootstrap_session(Cursor::new(jsonl), &TestEmbedder, &recorder, &config, None)
        .unwrap();

    let seen = recorder.seen.lock().unwrap().clone();
    assert!(
        !seen.is_empty(),
        "extractor should have been offered a candidate"
    );
    for text in &seen {
        assert!(
            !text.contains(PLANTED),
            "extractor received UNREDACTED turn text: {text:?}"
        );
    }
    // And the redaction was actually applied (placeholder present somewhere).
    assert!(
        seen.iter().any(|t| t.contains("[REDACTED:")),
        "expected a redaction placeholder in the extractor's input"
    );
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
