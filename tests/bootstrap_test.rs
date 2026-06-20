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

/// Build a UUID-linked multi-turn JSONL session: each `(user, assistant)` pair
/// becomes one reconstructable turn, chained `userN -> assistantN -> userN+1`,
/// with one-minute-apart timestamps. Used to exercise `max_turns` truncation,
/// which keeps the TAIL of the turn list.
fn multi_turn_session(sid: &str, turns: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut prev_uuid = String::new();
    for (i, (user, assistant)) in turns.iter().enumerate() {
        let u_uuid = format!("u{i}");
        let a_uuid = format!("a{i}");
        let minute = i; // distinct, monotonically increasing minute per turn
        let user_entry = serde_json::json!({
            "type": "user", "sessionId": sid,
            "timestamp": format!("2024-06-01T10:{minute:02}:00Z"),
            "uuid": u_uuid,
            "parentUuid": if prev_uuid.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(prev_uuid.clone()) },
            "message": {"role": "user", "content": [{"type": "text", "text": user}]}
        });
        let asst_entry = serde_json::json!({
            "type": "assistant", "sessionId": sid,
            "timestamp": format!("2024-06-01T10:{minute:02}:05Z"),
            "uuid": a_uuid, "parentUuid": u_uuid,
            "message": {"role": "assistant", "content": [{"type": "text", "text": assistant}]}
        });
        out.push_str(&user_entry.to_string());
        out.push('\n');
        out.push_str(&asst_entry.to_string());
        out.push('\n');
        prev_uuid = u_uuid;
    }
    out
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
fn bootstrap_savepoint_rolls_back_on_embed_failure() {
    // #302: the error path in bootstrap_within_savepoint issues
    // `ROLLBACK TO bootstrap; RELEASE bootstrap` to keep the connection usable
    // after a mid-pipeline failure. An embedder that errors AFTER the marker
    // event is inserted is the natural trigger. Assert the session returns Err,
    // then that a SECOND, successful session on the SAME engine still processes —
    // proving the savepoint was rolled back (marker gone, so not skipped) and
    // released (no dangling open transaction).
    struct FailEmbedder;
    impl EmbeddingProvider for FailEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
            Err(MemoryError::NotFound("inject embed failure".into()))
        }
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    // First session: embedding fails mid-pipeline (after the marker insert).
    let err = engine.bootstrap_session(
        Cursor::new(success_fixture()),
        &FailEmbedder,
        &extractor,
        &config,
        None,
    );
    assert!(
        err.is_err(),
        "embed failure must propagate as Err from bootstrap_session"
    );

    // Second session on the SAME engine with a working embedder: if the savepoint
    // had been left open (or the marker not rolled back) this would fail or skip.
    let report = engine
        .bootstrap_session(
            Cursor::new(success_fixture()),
            &TestEmbedder,
            &extractor,
            &config,
            None,
        )
        .expect("connection must remain usable after a rolled-back session");
    assert_eq!(
        report.sessions_processed, 1,
        "second session must process (rollback removed the first marker; no open txn)"
    );
    assert_eq!(report.sessions_skipped, 0);
    assert!(report.facts_created > 0, "second session must store facts");
}

#[test]
fn bootstrap_pins_facts_when_classifier_returns_true() {
    // #301: the PersistenceClassifier branch in store_extracted_fact builds a
    // temporary Fact, calls should_pin, and sets new_fact.is_pinned. Integration
    // coverage existed only for should_pin == false (the dryrun recorder), so the
    // is_pinned == true PROPAGATION into a stored bootstrap row was untested.
    // Drive both poles and assert the stored flag follows the classifier.
    use memory_engine::Fact;
    use memory_engine::traits::PersistenceClassifier;

    struct AlwaysPin;
    impl PersistenceClassifier for AlwaysPin {
        fn should_pin(&self, _fact: &Fact) -> bool {
            true
        }
    }
    struct NeverPin;
    impl PersistenceClassifier for NeverPin {
        fn should_pin(&self, _fact: &Fact) -> bool {
            false
        }
    }

    let extractor = KeywordExtractor;

    // AlwaysPin: every bootstrapped fact must land pinned.
    let engine_pin = engine();
    let report_pin = engine_pin
        .bootstrap_session(
            Cursor::new(success_fixture()),
            &TestEmbedder,
            &extractor,
            &BootstrapConfig::default(),
            Some(&AlwaysPin),
        )
        .unwrap();
    assert!(report_pin.facts_created > 0, "fixture should create facts");
    let pinned = engine_pin.list_active_facts(None).unwrap();
    assert_eq!(pinned.len(), report_pin.facts_created);
    assert!(
        pinned.iter().all(|f| f.is_pinned),
        "AlwaysPin classifier must propagate is_pinned == true to every stored fact"
    );

    // NeverPin baseline: the flag is toggled by the classifier, not always true.
    let engine_unpin = engine();
    let report_unpin = engine_unpin
        .bootstrap_session(
            Cursor::new(success_fixture()),
            &TestEmbedder,
            &extractor,
            &BootstrapConfig::default(),
            Some(&NeverPin),
        )
        .unwrap();
    assert!(report_unpin.facts_created > 0);
    let unpinned = engine_unpin.list_active_facts(None).unwrap();
    assert!(
        unpinned.iter().all(|f| !f.is_pinned),
        "NeverPin classifier must leave every stored fact unpinned"
    );
}

#[test]
fn bootstrap_max_entries_caps_parsed_entries() {
    // #293 residual: a session of many small, individually-valid lines must be
    // truncated at the per-stream entry-count cap surfaced on BootstrapConfig,
    // bounding the in-memory entry Vec (and every downstream linear pass)
    // regardless of file size. Build 50 valid entries but cap at 5.
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig {
        max_entries: 5,
        skip_existing: false,
        ..Default::default()
    };

    let jsonl: String = (0..50)
        .map(|i| {
            serde_json::json!({
                "type": "user", "sessionId": "cap-sess", "timestamp": "2024-05-01T10:00:00Z",
                "uuid": format!("u{i}"), "parentUuid": serde_json::Value::Null,
                "message": {"role": "user", "content": [{"type": "text", "text": format!("line {i}")}]}
            })
            .to_string()
        })
        .map(|s| s + "\n")
        .collect();

    let report = engine
        .bootstrap_session(Cursor::new(jsonl), &TestEmbedder, &extractor, &config, None)
        .unwrap();

    assert_eq!(
        report.entries_parsed, 5,
        "entries_parsed must be capped at max_entries (5), got {}",
        report.entries_parsed
    );
}

#[test]
fn bootstrap_max_session_bytes_caps_stream() {
    // #293 residual: the per-stream byte ceiling stops the reader mid-file, so a
    // huge session is bounded even before the entry-count cap. With a tight byte
    // cap, only the prefix that fits is parsed; the rest is never read.
    let engine = engine();
    let extractor = KeywordExtractor;

    let line = "{\"type\":\"user\",\"sessionId\":\"bytes-sess\",\"uuid\":\"u0\",\"parentUuid\":null,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}";
    let one = format!("{line}\n");
    let mut jsonl = String::new();
    for _ in 0..50 {
        jsonl.push_str(&one);
    }
    // Admit roughly two lines' worth of bytes.
    let cap = (one.len() * 2) as u64;
    let config = BootstrapConfig {
        max_session_bytes: cap,
        skip_existing: false,
        ..Default::default()
    };

    let report = engine
        .bootstrap_session(Cursor::new(jsonl), &TestEmbedder, &extractor, &config, None)
        .unwrap();

    assert!(
        report.entries_parsed <= 3,
        "per-stream byte cap must bound parsed entries to the admitted prefix, got {}",
        report.entries_parsed
    );
    assert!(
        report.entries_parsed >= 1,
        "the admitted prefix should still parse at least one entry, got {}",
        report.entries_parsed
    );
}

#[test]
fn bootstrap_max_turns_limits_processing() {
    // #300: assert the DOWNSTREAM effect of max_turns truncation, not the
    // pre-truncation `turns_reconstructed` count (which is set before the slice
    // and so proves nothing). A 3-turn session where every turn carries a bug
    // keyword yields 3 candidates uncapped; with max_turns=1 only the TAIL turn
    // survives, so exactly 1 candidate/fact is produced — proving the slice fired.
    let extractor = KeywordExtractor;
    let turns: &[(&str, &str)] = &[
        ("first?", "The root cause was an off-by-one in turn one."),
        ("second?", "The root cause was a null deref in turn two."),
        (
            "third?",
            "The root cause was a race condition in turn three.",
        ),
    ];

    // Uncapped baseline.
    let engine_uncapped = engine();
    let report_uncapped = engine_uncapped
        .bootstrap_session(
            Cursor::new(multi_turn_session("mt-uncapped", turns)),
            &TestEmbedder,
            &extractor,
            &BootstrapConfig::default(),
            None,
        )
        .unwrap();
    assert_eq!(
        report_uncapped.turns_reconstructed, 3,
        "fixture must yield 3 turns"
    );
    assert_eq!(
        report_uncapped.candidates_found, 3,
        "uncapped: every turn is a Bug candidate"
    );
    assert_eq!(report_uncapped.facts_created, 3, "uncapped: 3 facts");

    // Capped to the tail turn only.
    let engine_capped = engine();
    let report_capped = engine_capped
        .bootstrap_session(
            Cursor::new(multi_turn_session("mt-capped", turns)),
            &TestEmbedder,
            &extractor,
            &BootstrapConfig {
                max_turns: 1,
                ..Default::default()
            },
            None,
        )
        .unwrap();

    assert_eq!(report_capped.sessions_processed, 1);
    assert!(
        report_capped.candidates_found < report_uncapped.candidates_found,
        "max_turns must reduce candidates: capped={} uncapped={}",
        report_capped.candidates_found,
        report_uncapped.candidates_found
    );
    assert_eq!(
        report_capped.candidates_found, 1,
        "max_turns=1 keeps exactly the tail turn"
    );
    assert_eq!(
        report_capped.facts_created, 1,
        "max_turns=1 produces one fact (the tail turn)"
    );
}

#[test]
fn bootstrap_mixed_session_fixture() {
    // #424: mixed_session.jsonl is the only fixture exercising thinking blocks,
    // progress-noise filtering, and Decision + Learning + Convention categories in
    // one session with UUID pairing across progress noise — yet it was referenced
    // by no test. Lock its behavior end to end.
    let engine = engine();
    let extractor = KeywordExtractor;
    let config = BootstrapConfig::default();

    let report = engine
        .bootstrap_session(
            Cursor::new(include_str!("fixtures/mixed_session.jsonl")),
            &TestEmbedder,
            &extractor,
            &config,
            None,
        )
        .unwrap();

    assert_eq!(report.sessions_processed, 1);
    assert!(report.entries_parsed > 0, "fixture entries must parse");
    // Two genuine user/assistant pairs survive progress-noise filtering.
    assert_eq!(
        report.turns_reconstructed, 2,
        "two turns reconstructed across the progress noise, got {}",
        report.turns_reconstructed
    );

    // The categories come from real content keywords ("turns out"/"reason is" →
    // Learning, "decided"/"went with" → Decision, "always use" → Convention), not
    // from the thinking block (whose text "Let me look at the existing error
    // handling patterns" carries no category keyword and seeds no spurious episode).
    assert!(
        report.category_counts.decision + report.category_counts.learning > 0,
        "fixture must yield at least one Decision or Learning episode (d={}, l={})",
        report.category_counts.decision,
        report.category_counts.learning
    );

    // Stored facts must not contain the noise/thinking-only text as standalone
    // leakage: every created fact traces to a keyword-matched turn.
    assert!(report.facts_created > 0, "fixture must create facts");
    let facts = engine.list_active_facts(None).unwrap();
    assert!(
        facts
            .iter()
            .all(|f| f.content.contains("User:") || f.content.contains("Assistant:")),
        "every fact's content must come from reconstructed turn text"
    );
}
