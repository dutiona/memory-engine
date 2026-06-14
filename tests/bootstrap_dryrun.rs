//! S0.3 (#43) — bootstrap/backdate audit dry-run.
//!
//! Characterizes ME's bootstrap behavior on five synthetic Claude-Code-style
//! sessions, empirically confirming the four properties B3 (#53) and the
//! conflation horizons (#42) depend on:
//!   1. fact yield  — keyword pre-filter + `KeywordExtractor` produce facts;
//!   2. idempotency — re-running a bootstrapped session is a no-op (`skip_existing`);
//!   3. `t_created` backdating — facts carry the historical session timestamp,
//!      never `Utc::now()`;
//!   4. NO cross-session fact dedup — the same fact in two sessions is stored
//!      twice (relevant to a 9-month backfill).
//!
//! Hermetic: in-memory engine + zero-vector embedder. No network, no Ollama,
//! no GPU contention (S0 is build-only). See `docs/audits/S0.3-bootstrap-audit.md`.

use std::cell::RefCell;
use std::io::Cursor;

use chrono::{DateTime, Datelike, Utc};
use memory_engine::bootstrap::extract::KeywordExtractor;
use memory_engine::bootstrap::metrics::BootstrapConfig;
use memory_engine::traits::PersistenceClassifier;
use memory_engine::{EmbeddingProvider, Fact, MemoryEngine, MemoryError};

/// Zero-vector embedder (dim 4) — retrieval is irrelevant to this audit.
struct TestEmbedder;
impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.0; 4])
    }
}

/// Records `(content, t_created)` for every fact the engine offers for pinning,
/// i.e. every bootstrapped fact — with its (backdated) `t_created` intact.
#[derive(Default)]
struct RecordingClassifier {
    seen: RefCell<Vec<(String, DateTime<Utc>)>>,
}
impl PersistenceClassifier for RecordingClassifier {
    fn should_pin(&self, fact: &Fact) -> bool {
        self.seen
            .borrow_mut()
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
        let ts = (base_dt + chrono::Duration::seconds(offset_secs)).to_rfc3339();
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
                ("user", "Remind me of the commit rule and fix the panic."),
                (
                    "assistant",
                    "Rule: always use rustfmt before every commit. Also fixed the panic; the root cause was an unwrap.",
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
#[test]
fn dryrun_yield_backdate_idempotency_no_dedup() {
    let engine = MemoryEngine::open_memory(4).unwrap();
    let extractor = KeywordExtractor;
    let recorder = RecordingClassifier::default();
    let config = BootstrapConfig::default(); // skip_existing = true

    let specs = specs();
    let sessions: Vec<String> = specs
        .iter()
        .map(|(sid, base, turns)| session(sid, base, turns))
        .collect();

    // --- First pass: yield ---
    let mut total_facts = 0;
    for (i, jsonl) in sessions.iter().enumerate() {
        let report = engine
            .bootstrap_session(
                Cursor::new(jsonl.as_bytes()),
                &TestEmbedder,
                &extractor,
                &config,
                Some(&recorder),
            )
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
    }
    println!("TOTAL facts (first pass) = {total_facts}");
    for (content, t) in recorder.seen.borrow().iter() {
        let snippet: String = content.chars().take(90).collect();
        println!("  fact t_created={t} content={snippet:?}");
    }
    // Deterministic yield: 4 single-category sessions + session 5's dual-category
    // turn (Convention + Bug) => 6 facts. Pins expected yield against extractor drift.
    assert_eq!(
        total_facts, 6,
        "expected 6 facts (4 single-category + session-5 dual-category), got {total_facts}"
    );

    // --- Idempotency: re-run all five, expect skips + zero new facts ---
    let facts_before_rerun = recorder.seen.borrow().len();
    for jsonl in &sessions {
        let report = engine
            .bootstrap_session(
                Cursor::new(jsonl.as_bytes()),
                &TestEmbedder,
                &extractor,
                &config,
                Some(&recorder),
            )
            .unwrap();
        assert_eq!(report.sessions_processed, 0, "re-run must skip");
        assert_eq!(report.sessions_skipped, 1);
        assert_eq!(report.facts_created, 0);
        assert_eq!(
            report.events_ingested, 0,
            "skipped session ingests no marker"
        );
    }
    assert_eq!(
        recorder.seen.borrow().len(),
        facts_before_rerun,
        "skipped sessions must create no facts"
    );

    // --- Backdating: every t_created is the historical session time (2024/2025),
    //     never Utc::now() (2026+). ---
    let seen = recorder.seen.borrow();
    assert!(!seen.is_empty(), "recorder should have captured facts");
    let now = Utc::now();
    for (content, t) in seen.iter() {
        assert!(
            t.year() < now.year(),
            "t_created not backdated (year >= current) ({t}) for {content:?}"
        );
        assert!(t < &now, "t_created must be historical, got {t}");
    }

    // --- No cross-session dedup: the IDENTICAL convention sentence appears verbatim
    //     in sessions 3 and 5. Assert (a) it is stored more than once (no dedup) AND
    //     (b) the copies originate in distinct backdated sessions (cross-session, not
    //     merely session 5's intra-session dual-category fan-out). ---
    let shared_copies = seen.iter().filter(|(c, _)| c.contains(SHARED)).count();
    let shared_sessions: std::collections::BTreeSet<i64> = seen
        .iter()
        .filter(|(c, _)| c.contains(SHARED))
        .map(|(_, t)| t.timestamp())
        .collect();
    println!(
        "shared convention: {shared_copies} stored copies across {} distinct sessions",
        shared_sessions.len()
    );
    assert!(
        shared_copies >= 2,
        "identical fact must be stored more than once (no dedup), got {shared_copies}"
    );
    assert!(
        shared_sessions.len() >= 2,
        "duplicate copies must originate in distinct backdated sessions (cross-session), got {}",
        shared_sessions.len()
    );
}
