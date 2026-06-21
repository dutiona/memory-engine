//! S0.3 (#43) — bootstrap/backdate audit dry-run.
//!
//! Characterizes ME's bootstrap behavior on five synthetic Claude-Code-style
//! sessions, empirically confirming the four properties B3 (#53) and the
//! conflation horizons (#42) depend on:
//!   1. fact yield  — keyword pre-filter + `KeywordExtractor` produce facts;
//!   2. idempotency — re-running a bootstrapped session is a no-op (`skip_existing`);
//!   3. `t_created` backdating — facts carry the historical session timestamp,
//!      never `Utc::now()`;
//!   4. cross-session dedup-with-reinforcement (#520) — the same fact in two
//!      sessions is stored ONCE and reinforced (`access_count` bumped, `t_created` =
//!      earliest, `last_accessed` = latest), relevant to a 9-month backfill.
//!
//! Hermetic: in-memory engine + zero-vector embedder. No network, no Ollama,
//! no GPU contention (S0 is build-only). See `docs/audits/S0.3-bootstrap-audit.md`.

#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Datelike, Utc};
use memory_engine::traits::PersistenceClassifier;
use memory_engine::{
    BootstrapConfig, EmbeddingFingerprint, EmbeddingProvider, Fact, KeywordExtractor, MemoryEngine,
    MemoryError, SessionExtractor,
};

/// Zero-vector embedder (dim 4) — retrieval is irrelevant to this audit.
struct TestEmbedder;
impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.0; 4])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", 4)
    }
}

/// Records `(content, t_created)` for every fact the engine offers for pinning,
/// i.e. every bootstrapped fact — with its (backdated) `t_created` intact.
#[derive(Default)]
struct RecordingClassifier {
    seen: Mutex<Vec<(String, DateTime<Utc>)>>,
}
impl PersistenceClassifier for RecordingClassifier {
    fn should_pin(&self, fact: &Fact) -> bool {
        self.seen
            .lock()
            .unwrap()
            .push((fact.content.clone(), fact.t_created));
        false
    }
}

/// Build a Claude-Code-style JSONL session from (role, text) turns, assigning
/// sequential timestamps 30s apart starting at `base` (RFC 3339).
fn session(sid: &str, base: &str, turns: &[(&str, &str)]) -> String {
    let base_dt = DateTime::parse_from_rfc3339(base)
        .unwrap()
        .with_timezone(&Utc);
    let mut out = String::new();
    for (i, (role, text)) in turns.iter().enumerate() {
        let offset_secs = i64::try_from(i).unwrap() * 30;
        let ts = (base_dt + chrono::Duration::try_seconds(offset_secs).unwrap()).to_rfc3339();
        let parent = if i == 0 {
            serde_json::Value::Null
        } else {
            serde_json::json!(format!("{sid}-{:04}", i - 1))
        };
        let entry = serde_json::json!({
            "type": role,
            "sessionId": sid,
            "timestamp": ts,
            "uuid": format!("{sid}-{i:04}"),
            "parentUuid": parent,
            "cwd": "/home/dev/proj",
            "gitBranch": "main",
            "message": {"role": role, "content": [{"type": "text", "text": text}]},
        });
        out.push_str(&entry.to_string());
        out.push('\n');
    }
    out
}

/// A session spec: (`session_id`, base RFC-3339 timestamp, `[(role, text)]` turns).
type SessionSpec = (
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
);

/// Five sessions backdated across 2024–2025. Sessions 3 and 5 share an
/// identical "always use rustfmt" convention (distinctive token `rustfmt`) to
/// probe cross-session dedup.
fn specs() -> Vec<SessionSpec> {
    vec![
        (
            "sess-bug-2024-01",
            "2024-01-15T09:00:00Z",
            vec![
                ("user", "Fix the off-by-one error in parser.rs"),
                (
                    "assistant",
                    "Found the root cause: the loop used the wrong bound. Applied the fix; tests pass.",
                ),
            ],
        ),
        (
            "sess-dec-2024-04",
            "2024-04-10T11:00:00Z",
            vec![
                ("user", "Which serialization library should we use?"),
                (
                    "assistant",
                    "We decided to go with serde; we chose it over alternatives for ecosystem maturity.",
                ),
            ],
        ),
        (
            "sess-conv-2024-07",
            "2024-07-20T14:00:00Z",
            vec![
                ("user", "Any formatting rule for commits?"),
                (
                    "assistant",
                    "Team convention: always use rustfmt before every commit.",
                ),
            ],
        ),
        (
            "sess-learn-2024-10",
            "2024-10-05T16:00:00Z",
            vec![
                ("user", "Why did the latency drop?"),
                (
                    "assistant",
                    "TIL the reason is spawn_blocking moved IO off the runtime; turns out it helped a lot.",
                ),
            ],
        ),
        (
            "sess-conv-2025-01",
            "2025-01-12T10:00:00Z",
            vec![
                // Convention turn — byte-identical (user + assistant) to session 3, so the
                // extracted fact content matches VERBATIM across two distinct sessions: the
                // cross-session dedup-with-reinforcement probe. insert_or_reinforce collapses
                // the two identical facts into one stored row and reinforces it (access_count
                // bumped, t_created = earliest, last_accessed = latest) — asserted below.
                ("user", "Any formatting rule for commits?"),
                (
                    "assistant",
                    "Team convention: always use rustfmt before every commit.",
                ),
                // Separate bug turn — keeps session 5 dual-fact, preserving the 6-fact yield.
                ("user", "Also, fix the panic in the worker."),
                (
                    "assistant",
                    "Fixed the panic; the root cause was an unwrap on an empty queue.",
                ),
            ],
        ),
    ]
}

/// The convention sentence repeated verbatim in sessions 3 and 5 (dedup probe).
const SHARED: &str = "always use rustfmt before every commit";

// A cohesive four-phase characterization run (yield → idempotency → backdating →
// no-dedup) sharing one engine + recorder; kept as a single test on purpose.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn dryrun_yield_backdate_idempotency_dedup_reinforce() {
    let engine = MemoryEngine::builder(4).build().unwrap();
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder);
    let extractor: Arc<dyn SessionExtractor> = Arc::new(KeywordExtractor);
    let recorder = Arc::new(RecordingClassifier::default());
    let config = BootstrapConfig::default(); // skip_existing = true

    let specs = specs();
    let sessions: Vec<String> = specs
        .iter()
        .map(|(sid, base, turns)| session(sid, base, turns))
        .collect();

    // --- First pass: yield ---
    let mut total_facts = 0;
    let mut total_reinforced = 0;
    for (i, jsonl) in sessions.iter().enumerate() {
        let report = engine
            .bootstrap_session(
                Cursor::new(jsonl.clone().into_bytes()),
                embedder.clone(),
                extractor.clone(),
                &config,
                Some(recorder.clone() as Arc<dyn PersistenceClassifier>),
            )
            .await
            .unwrap();
        println!(
            "session {i} ({}): processed={} entries={} turns={} candidates={} facts={} \
             outcome(s/f/i)={}/{}/{} prewarm(e/s/p)={}/{}/{}",
            specs[i].0,
            report.sessions_processed,
            report.entries_parsed,
            report.turns_reconstructed,
            report.candidates_found,
            report.facts_created,
            report.outcome_counts.success,
            report.outcome_counts.failure,
            report.outcome_counts.indeterminate,
            report.prewarm_metrics.episodic_count,
            report.prewarm_metrics.semantic_count,
            report.prewarm_metrics.procedural_count,
        );
        assert_eq!(report.sessions_processed, 1, "session {i} should process");
        assert_eq!(report.events_ingested, 1, "one marker event per session");
        total_facts += report.facts_created;
        total_reinforced += report.facts_reinforced;
    }
    println!("TOTAL facts (first pass) = {total_facts}");
    for (content, t) in recorder.seen.lock().unwrap().iter() {
        let snippet: String = content.chars().take(90).collect();
        println!("  fact t_created={t} content={snippet:?}");
    }
    // Deterministic yield with dedup-with-reinforcement: sessions 1–4 → 1 created fact
    // each (4); session 5 → its bug turn is created (1) while its convention turn (identical
    // to session 3's) is REINFORCED, not created ⇒ 5 created + 1 reinforced. The classifier
    // still sees all 6 candidate facts (it runs before the dedup decision).
    assert_eq!(
        total_facts, 5,
        "expected 5 created facts (sessions 1–4 + session-5 bug; convention reinforced), got {total_facts}"
    );
    assert_eq!(
        total_reinforced, 1,
        "expected 1 reinforced fact (session-5 convention dedups onto session-3's), got {total_reinforced}"
    );
    assert_eq!(
        recorder.seen.lock().unwrap().len(),
        6,
        "classifier should be offered all 6 candidate facts (it runs pre-dedup)"
    );

    // --- Idempotency: re-run all five, expect skips + zero new facts ---
    let facts_before_rerun = recorder.seen.lock().unwrap().len();
    for jsonl in &sessions {
        let report = engine
            .bootstrap_session(
                Cursor::new(jsonl.clone().into_bytes()),
                embedder.clone(),
                extractor.clone(),
                &config,
                Some(recorder.clone() as Arc<dyn PersistenceClassifier>),
            )
            .await
            .unwrap();
        assert_eq!(report.sessions_processed, 0, "re-run must skip");
        assert_eq!(report.sessions_skipped, 1);
        assert_eq!(report.facts_created, 0);
        assert_eq!(
            report.facts_reinforced, 0,
            "skipped session reinforces nothing"
        );
        assert_eq!(
            report.events_ingested, 0,
            "skipped session ingests no marker"
        );
    }
    assert_eq!(
        recorder.seen.lock().unwrap().len(),
        facts_before_rerun,
        "skipped sessions must create no facts"
    );

    // --- Backdating: every t_created is the historical session time (2024/2025),
    //     never Utc::now() (2026+). ---
    //
    // Snapshot the recorder's captured facts once into an owned Vec; the
    // `std::sync::MutexGuard` is released on this line, so nothing holds a thread-affine
    // guard across the later engine `.await` calls (the engine API is async now). The
    // owned clone stays in scope for the cross-session dedup checks below.
    let seen: Vec<(String, DateTime<Utc>)> = recorder.seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "recorder should have captured facts");
    let now = Utc::now();
    for (content, t) in &seen {
        assert!(
            t.year() < now.year(),
            "t_created not backdated (year >= current) ({t}) for {content:?}"
        );
        assert!(t < &now, "t_created must be historical, got {t}");
    }

    // --- Cross-session dedup-with-reinforcement (#520): the IDENTICAL convention sentence
    //     appears verbatim in sessions 3 and 5. The classifier runs BEFORE the dedup
    //     decision, so it is offered both occurrences across two distinct backdated
    //     sessions — that is the pre-dedup view, not the stored view. ---
    let classifier_copies = seen.iter().filter(|(c, _)| c.contains(SHARED)).count();
    let classifier_sessions: std::collections::BTreeSet<i64> = seen
        .iter()
        .filter(|(c, _)| c.contains(SHARED))
        .map(|(_, t)| t.timestamp())
        .collect();
    println!(
        "shared convention: classifier saw {classifier_copies} copies across {} distinct sessions",
        classifier_sessions.len()
    );
    assert_eq!(
        classifier_copies, 2,
        "classifier (pre-dedup) must be offered both occurrences, got {classifier_copies}"
    );
    assert_eq!(
        classifier_sessions.len(),
        2,
        "the two occurrences originate in distinct backdated sessions, got {}",
        classifier_sessions.len()
    );

    // --- Store-level dedup-with-reinforcement: query the persisted rows directly.
    //     Authoritative check — the two identical-content occurrences collapse to ONE
    //     stored row, reinforced: access_count bumped (one reinforcement), t_created
    //     rolled back to the earliest session (2024), last_accessed advanced to the
    //     latest (2025). Total active facts drop from 6 candidates to 5 stored. ---
    let stored_facts = engine.list_active_facts(None).await.unwrap();
    assert_eq!(
        stored_facts.len(),
        5,
        "store must contain 5 active facts after dedup, got {}",
        stored_facts.len()
    );
    let shared_rows: Vec<&Fact> = stored_facts
        .iter()
        .filter(|f| f.content.contains(SHARED))
        .collect();
    assert_eq!(
        shared_rows.len(),
        1,
        "store must contain exactly ONE row for the shared convention (dedup-on-insert), got {}",
        shared_rows.len()
    );
    let shared = shared_rows[0];
    assert_eq!(
        shared.access_count, 1,
        "the shared convention must be reinforced once (access_count), got {}",
        shared.access_count
    );
    assert_eq!(
        shared.t_created.year(),
        2024,
        "t_created must roll back to the earliest occurrence (session 3, 2024), got {}",
        shared.t_created.year()
    );
    assert_eq!(
        shared.last_accessed.year(),
        2025,
        "last_accessed must advance to the latest occurrence (session 5, 2025), got {}",
        shared.last_accessed.year()
    );
}
