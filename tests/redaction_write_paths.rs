//! Every `MemoryEngine` write path redacts before it stores, classifies or embeds (#1024).
//!
//! The tripwire reads the facade's sources and fails until each public function, and each method
//! of a trait it implements, is listed here as gated (with the test that covers it) or exempt
//! (with the reason). A new write method therefore cannot land unclassified.
#![allow(clippy::unwrap_used)] // test code: panic-on-unwrap is the intended failure signal (#725)

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use memory_engine::engine::MemoryEngine;
use memory_engine::error::{EmbedderError, MemoryError, Result};
use memory_engine::redact::ScrubbingProposer;
use memory_engine::traits::{
    ConflictArbiter, CrudDecision, DeltaProposer, EmbeddingProvider, PersistenceClassifier,
    SummaryGenerator,
};
use memory_engine::types::{
    AddFactOptions, AddFactRequest, ClassifierInput, EventType, FactType, ForgetOutcome, NewEvent,
    NextEmbedding,
};
use memory_engine::types::{ConsolidationLevel, ConsolidationProposal, Fact, MergeGroup, NewFact};
use memory_engine::{
    Backoff, ConsolidationConfig, CycleDelta, EmbeddingFingerprint, LlmDreamCycle,
    read_embed_queue_status, read_redaction_counts,
};
use serde_json::{Value, json};

const DIM: usize = 4;

// ── The tripwire ─────────────────────────────────────────────────────────────

/// Gated: each name, and the test (here unless named) that proves it redacts.
const GATED: &[(&str, &str)] = &[
    (
        "add_fact",
        "add_fact_redacts_before_classifier_embedder_and_store",
    ),
    (
        "add_fact_deferred",
        "a_deferred_fact_queues_its_redacted_text",
    ),
    ("add_facts_batch", "batches_redact_every_entry"),
    ("add_facts_batch_partial", "batches_redact_every_entry"),
    ("ingest", "event_payloads_redact_all_but_genuine_provenance"),
    (
        "ingest_at",
        "event_payloads_redact_all_but_genuine_provenance",
    ),
    ("move_fact", "forget_and_move_records_redact"),
    ("forget_fact", "forget_and_move_records_redact"),
    ("checkpoint_session", "checkpoints_redact_and_stay_whole"),
    ("settle_embedding", "a_recorded_embedder_error_is_redacted"),
    ("record_activity", "tests/activity_stream_test.rs"),
    (
        "record_lifecycle_event",
        "a_captured_event_redacts_its_texts",
    ),
    ("import_backfill", "an_import_redacts_with_the_denylist"),
    (
        "bootstrap_session",
        "tests/bootstrap_test.rs (bootstrap's own pipeline)",
    ),
    (
        "bootstrap_directory",
        "tests/bootstrap_test.rs (bootstrap's own pipeline)",
    ),
    (
        "bootstrap_memory_directory",
        "tests/bootstrap_memory_dir.rs (bootstrap's own pipeline)",
    ),
    (
        "add_fact_precomputed",
        "a_caller_vector_beside_unredacted_text_is_refused",
    ),
    (
        "resolve_conflict",
        "a_caller_vector_beside_unredacted_text_is_refused",
    ),
    (
        "apply_cycle_report",
        "cycle_deltas_with_unredacted_text_are_skipped_and_counted",
    ),
    (
        "run_dream_cycle",
        "tests/dream_cycle_test.rs (applies its report)",
    ),
    (
        "run_dream_cycle_guarded",
        "tests/dream_cycle_test.rs (applies its report)",
    ),
    ("consolidate", "consolidation_summaries_are_redacted"),
    (
        "impl:DreamCtx for EngineDreamCtx::consolidate",
        "consolidation_summaries_are_redacted (DreamCtx delegates)",
    ),
    (
        "impl:SummaryGenerator for ScrubbingSummaries::summarize",
        "consolidation_summaries_are_redacted (the scrubbing wrapper)",
    ),
    (
        "promote_with_lineage",
        "a_caller_vector_beside_unredacted_text_is_refused",
    ),
    (
        "impl:DreamCtx for EngineDreamCtx::promote",
        "a_caller_vector_beside_unredacted_text_is_refused (DreamCtx delegates)",
    ),
];

/// Exempt: each name, and why it stores no caller text.
const EXEMPT: &[(&str, &str)] = &[
    // Identifiers, configuration or numbers, not content.
    ("set_config", "operator configuration"),
    ("ensure_scope_path", "a scope path is an identifier"),
    ("record_outcome", "an outcome is an enum"),
    ("link_session_facts", "edges between existing facts"),
    ("expire_edge", "no text"),
    ("pin_fact", "no text"),
    ("unpin_fact", "no text"),
    ("record_lineage", "ids of existing facts"),
    ("delete_lineage", "deletes"),
    ("erase", "deletes"),
    (
        "unforget_fact",
        "reverses a forget; its record holds ids only",
    ),
    ("prune", "decay pruning; no new text"),
    (
        "impl:DreamCtx for EngineDreamCtx::prune",
        "decay pruning; no new text",
    ),
    (
        "record_insight",
        "the consumer's InsightStream stores it, not this store",
    ),
    // Text read back from this store or one of its own snapshots (plan decision 13).
    (
        "restore_json",
        "restores an operator-supplied snapshot of an ME store",
    ),
    (
        "restore_json_memory",
        "restores an operator-supplied snapshot of an ME store",
    ),
    (
        "restore_sqlite",
        "restores an operator-supplied backup of an ME store",
    ),
    ("reconstruct", "re-embeds stored text"),
    ("archive", "moves stored rows to cold storage"),
    ("next_embedding", "reads the queue"),
    // Reads.
    ("query", "read"),
    ("execute_query", "read"),
    ("impl:DreamCtx for EngineDreamCtx::query", "read"),
    ("get_fact", "read"),
    ("impl:DreamCtx for EngineDreamCtx::get_fact", "read"),
    ("list_active_facts", "read"),
    (
        "impl:DreamCtx for EngineDreamCtx::list_active_facts",
        "read",
    ),
    ("list_summaries", "read"),
    (
        "impl:DreamCtx for EngineDreamCtx::list_undreamt_in_period",
        "read",
    ),
    ("impl:DreamCtx for EngineDreamCtx::outcome_counts", "read"),
    (
        "impl:DreamCtx for EngineDreamCtx::outcome_counts_batch",
        "read",
    ),
    ("get_outcome_counts", "read"),
    ("get_outcome_counts_batch", "read"),
    ("get_provenance", "read"),
    ("get_full_lineage", "read"),
    ("load_context", "read"),
    ("resume_context", "read"),
    ("sample_dormant", "read"),
    ("list_due", "read"),
    ("next_due_time", "read"),
    ("list_archives", "read"),
    ("verify_archives", "read"),
    ("search_archives_fallback", "read"),
    ("statistics", "read"),
    ("list_recent_insights", "read"),
    ("replay_events", "read"),
    ("explain_fact", "read"),
    ("fact_history", "read"),
    (
        "dump_state",
        "writes a snapshot file of the store, not to it",
    ),
    ("graph_degree", "read"),
    ("graph_component", "read"),
    ("graph_neighbors", "read"),
    ("graph_stats", "read"),
    ("graph_has_node", "read"),
    ("get_config", "read"),
    ("resolve_scope_path", "read"),
    ("read_embed_queue_status", "read"),
    ("read_redaction_counts", "read"),
    ("read_quarantined", "read"),
    ("read_recorded_embedding", "read"),
    (
        "declared_identity",
        "construction: an identity is an identifier, compared, never stored",
    ),
    ("end_idle_segments", "ends segments by time; stores no text"),
    ("open_status_connection", "read"),
    ("load_graph_from_db", "read"),
    ("load_scope_tree", "read"),
    ("verify_embedding_identity", "read"),
    ("verify_embedding_fingerprint", "read"),
    ("embed_work_arrived", "a wake-up"),
    // Construction, lifecycle and accessors.
    ("builder", "construction"),
    ("build", "construction"),
    ("new", "construction"),
    ("path", "construction"),
    ("into_config", "construction"),
    ("backup_dir", "construction"),
    ("reranker", "construction"),
    ("upcaster_registry", "construction"),
    ("read_only", "construction"),
    ("read_pool_size", "construction"),
    ("search_config", "construction"),
    ("open_from_config", "construction"),
    ("with_backend", "construction"),
    ("with_backup_dir", "construction"),
    ("with_read_only", "construction"),
    ("with_read_pool_size", "construction"),
    ("with_search_config", "construction"),
    ("with_upcaster_registry", "construction"),
    ("close", "lifecycle"),
    ("flush_snapshot", "writes the engine's own projections"),
    ("impl:Drop for MemoryEngine::drop", "lifecycle"),
    ("impl:Debug for MemoryEngine::fmt", "Debug"),
    (
        "impl:Default for ActivityFilterConfig::default",
        "ActivityFilterConfig's default",
    ),
    ("mem_ctx", "accessor"),
    ("storage", "accessor (test builds only)"),
    ("embed_dim", "accessor"),
    ("is_file_backed", "accessor"),
    ("is_read_only", "accessor"),
    ("reopen_required", "accessor"),
    ("reranker_name", "accessor"),
    ("delay", "Backoff arithmetic"),
    (
        "apply_filter",
        "the activity filter's decision; record_activity stores",
    ),
    (
        "format_promote_content",
        "formats the already redacted result; record_activity stores",
    ),
    ("ignore_patterns", "accessor"),
    ("promote_patterns", "accessor"),
];

/// Test-only files: the engine's tests and the equivalence harness's test doubles.
const TEST_FILES: &[&str] = &["tests.rs", "equivalence.rs"];

fn strip_test_modules(mut s: String) -> String {
    while let Some(at) = s.find("#[cfg(test)]") {
        let rest = &s[at..];
        let Some(open) = rest.find('{') else { break };
        let head = &rest[..open];
        if !head.contains("mod ") || head.contains(';') {
            // An attribute on something else (or `mod x;`): drop the attribute only.
            s.replace_range(at..at + "#[cfg(test)]".len(), "");
            continue;
        }
        let mut depth = 0;
        let mut end = at + open;
        for (i, c) in s[at + open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = at + open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        s.replace_range(at..end, "");
    }
    s
}

fn fn_name(line: &str) -> Option<&str> {
    let at = line.find("fn ")?;
    let name: &str = &line[at + 3..];
    let end = name.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&name[..end])
}

/// `Trait for Type` of an `impl … for …` header, without paths or generic arguments: methods
/// are keyed by both, so a same-named method of another trait, or of another implementation
/// of the same trait, is classified on its own.
fn trait_name(header: &str) -> String {
    let after_impl = header.trim_start().trim_start_matches("impl");
    // Skip the impl's own generic parameters, `impl<T: X> Trait for …`.
    let mut rest = after_impl.trim_start();
    if rest.starts_with('<') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        rest = &rest[i + 1..];
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let mut parts = rest.splitn(2, " for ");
    let name = |path: &str| {
        let path = path
            .trim()
            .split(['<', ' ', '{'])
            .next()
            .unwrap_or_default();
        path.rsplit("::").next().unwrap_or_default().to_owned()
    };
    let trait_ = name(parts.next().unwrap_or_default());
    let type_ = name(parts.next().unwrap_or_default());
    format!("{trait_} for {type_}")
}

/// Every `.rs` file under `dir`, its submodules included.
fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

fn facade_functions() -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine"),
        &mut files,
    );
    let mut names = BTreeSet::new();
    for path in files {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        if TEST_FILES.contains(&file.as_str()) {
            continue;
        }
        let source = strip_test_modules(std::fs::read_to_string(&path).unwrap());
        let mut impl_for_depth: Option<i32> = None;
        let mut impl_trait = String::new();
        // An `impl` header, possibly over several lines (a `where` clause), until its `{`.
        let mut header: Option<String> = None;
        let mut depth = 0_i32;
        for line in source.lines() {
            let trimmed = line.trim_start();
            if impl_for_depth.is_none() && header.is_none() && trimmed.starts_with("impl") {
                header = Some(String::new());
            }
            if let Some(h) = header.as_mut() {
                h.push(' ');
                h.push_str(trimmed);
                if trimmed.contains('{') || trimmed.ends_with(';') {
                    if h.contains(" for ") && trimmed.contains('{') {
                        impl_for_depth = Some(depth);
                        impl_trait = trait_name(h);
                    }
                    header = None;
                }
            }
            let public = trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("pub const fn ")
                || trimmed.starts_with("pub(crate) fn ")
                || trimmed.starts_with("pub(crate) async fn ")
                || trimmed.starts_with("pub(crate) const fn ");
            let in_impl_for = impl_for_depth.is_some_and(|d| depth == d + 1);
            if let Some(name) = fn_name(trimmed).filter(|_| trimmed.contains("fn ")) {
                if in_impl_for && (trimmed.starts_with("fn ") || trimmed.starts_with("async fn ")) {
                    names.insert(format!("impl:{impl_trait}::{name}"));
                } else if public {
                    names.insert(name.to_owned());
                }
            }
            depth += i32::try_from(line.matches('{').count()).unwrap()
                - i32::try_from(line.matches('}').count()).unwrap();
            if impl_for_depth.is_some_and(|d| depth <= d) {
                impl_for_depth = None;
            }
        }
    }
    names
}

/// A same-named method of two traits, or of two implementations of one trait, gets two keys, so
/// one's exemption never covers the other.
#[test]
fn impl_methods_are_keyed_by_their_trait_and_type() {
    for (header, want) in [
        (
            " impl DreamCtx for EngineDreamCtx<'_> {",
            "DreamCtx for EngineDreamCtx",
        ),
        (" impl DreamCtx for AuditedCtx {", "DreamCtx for AuditedCtx"),
        (
            " impl<T: Send> crate::traits::AuditSink for log::Logger<T> {",
            "AuditSink for Logger",
        ),
        (
            " impl SummaryGenerator for ScrubbingSummaries where Self: Sized {",
            "SummaryGenerator for ScrubbingSummaries",
        ),
    ] {
        assert_eq!(trait_name(header), want, "{header}");
    }
}

#[test]
fn every_facade_function_is_classified() {
    let classified: BTreeSet<&str> = GATED.iter().chain(EXEMPT).map(|(n, _)| *n).collect();
    let unclassified: Vec<String> = facade_functions()
        .into_iter()
        .filter(|n| !classified.contains(n.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "classify each as gated (and test it) or exempt (with its reason): {unclassified:?}"
    );
}

// ── Canaries ─────────────────────────────────────────────────────────────────

/// Synthetic secrets, assembled at run time.
struct Canaries {
    aws: String,
    machine_token: String,
    password: String,
    hex: String,
}

impl Canaries {
    fn new() -> Self {
        Self {
            aws: format!("AKIA{}", "IOSFODNN7EXAMPLE"),
            machine_token: format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay"),
            password: format!("s3cr3t{}", "PassW0rdq"),
            hex: "a3f1b2c4d5e6f7a8".repeat(2),
        }
    }

    fn text(&self) -> String {
        format!(
            "note {} and {} via postgres://svc:{}@db.internal/x and api_key={} end",
            self.aws, self.machine_token, self.password, self.hex
        )
    }

    fn secrets(&self) -> [&str; 4] {
        [&self.aws, &self.machine_token, &self.password, &self.hex]
    }
}

const SCRUBBED: &str = "note [REDACTED:aws-access-key] and [REDACTED:machine-token] via \
     postgres://svc:[REDACTED:connection-string-password]@db.internal/x and \
     api_key=[REDACTED:hex-secret] end";

fn assert_no_canary_in(text: &str, c: &Canaries) {
    for s in c.secrets() {
        assert!(!text.contains(s), "a canary leaked: {text}");
    }
}

/// No file of the store's directory (database, WAL, sidecars) holds a canary or 12 bytes of one.
fn assert_store_files_clean(dir: &Path, c: &Canaries) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        for secret in c.secrets() {
            for window in secret.as_bytes().windows(12) {
                assert!(
                    !bytes.windows(12).any(|w| w == window),
                    "a fragment of a canary is on disk"
                );
            }
        }
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    fn seen(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct RecordingEmbedder(Arc<Recorder>);

impl EmbeddingProvider for RecordingEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.0.0.lock().unwrap().push(text.to_owned());
        Ok(vec![0.5; DIM])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("test", "test", DIM)
    }
}

struct RecordingClassifier(Arc<Recorder>);

impl PersistenceClassifier for RecordingClassifier {
    fn should_pin(&self, input: &ClassifierInput) -> bool {
        self.0
            .0
            .lock()
            .unwrap()
            .push(format!("{} {}", input.content, input.metadata));
        false
    }
}

struct Store {
    dir: tempfile::TempDir,
    db: PathBuf,
    engine: MemoryEngine,
}

impl Store {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let engine = MemoryEngine::builder(DIM).path(&db).build().unwrap();
        Self { dir, db, engine }
    }

    async fn close(mut self) -> (tempfile::TempDir, PathBuf) {
        self.engine.close().await.unwrap();
        (self.dir, self.db)
    }
}

fn request(content: String, metadata: Value) -> AddFactRequest {
    AddFactRequest {
        content,
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: Some("unscoped:laptop".into()),
        opts: Some(AddFactOptions {
            metadata: Some(metadata),
            ..Default::default()
        }),
    }
}

fn event(payload: Value) -> NewEvent {
    NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: EventType::MemoryOp,
        payload,
        source: "test".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "test".into(),
        sequence_id: 0,
        created_at: None,
    }
}

async fn events(engine: &MemoryEngine) -> Vec<Value> {
    engine
        .replay_events(&memory_engine::inspect::ReplayFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

// ── The write paths ──────────────────────────────────────────────────────────

#[tokio::test]
async fn add_fact_redacts_before_classifier_embedder_and_store() {
    let c = Canaries::new();
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    let id = store
        .engine
        .add_fact(
            &request(c.text(), json!({ "note": c.text(), "api_key": c.hex })),
            Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
            Some(Arc::new(RecordingClassifier(Arc::clone(&recorder)))),
        )
        .await
        .unwrap();
    let fact = store.engine.get_fact(id).await.unwrap();
    assert_eq!(fact.content, SCRUBBED);
    assert_eq!(fact.metadata["note"], SCRUBBED);
    assert_eq!(fact.metadata["api_key"], "[REDACTED:hex-secret]");
    let seen = recorder.seen();
    assert_eq!(
        seen.len(),
        2,
        "the embedder and the classifier each saw the fact once"
    );
    for s in &seen {
        assert_no_canary_in(s, &c);
    }
    let (_dir, db) = store.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 9);
    assert_store_files_clean(db.parent().unwrap(), &c);
}

/// A fact is kept whole, never cut: only its secrets change.
#[tokio::test]
async fn a_long_fact_comes_back_whole() {
    let c = Canaries::new();
    let store = Store::new();
    let prose = "word ".repeat(820);
    let id = store
        .engine
        .add_fact_deferred(&request(format!("{prose}{}", c.aws), json!({})))
        .await
        .unwrap();
    assert_eq!(
        store.engine.get_fact(id).await.unwrap().content,
        format!("{prose}[REDACTED:aws-access-key]")
    );
}

#[tokio::test]
async fn a_deferred_fact_queues_its_redacted_text() {
    let c = Canaries::new();
    let store = Store::new();
    store
        .engine
        .add_fact_deferred(&request(c.text(), json!({})))
        .await
        .unwrap();
    let NextEmbedding::Due(item) = store.engine.next_embedding().await.unwrap() else {
        panic!("the fact is queued")
    };
    assert_eq!(item.content, SCRUBBED);
    let (_dir, db) = store.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 4);
    assert_store_files_clean(db.parent().unwrap(), &c);
}

#[tokio::test]
async fn batches_redact_every_entry() {
    let c = Canaries::new();
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    let embedder = || Arc::new(RecordingEmbedder(Arc::clone(&recorder)));
    let ids = store
        .engine
        .add_facts_batch(
            &[
                request(c.text(), json!({})),
                request(format!("b {}", c.aws), json!({})),
            ],
            embedder(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store.engine.get_fact(ids[1]).await.unwrap().content,
        "b [REDACTED:aws-access-key]"
    );
    let oversized = "x".repeat(me_types::limits::MAX_PAYLOAD_BYTES + 1);
    let results = store
        .engine
        .add_facts_batch_partial(
            &[
                request(oversized, json!({})),
                request(format!("c {}", c.machine_token), json!({})),
            ],
            embedder(),
            None,
        )
        .await
        .unwrap();
    assert!(results[0].is_err(), "the oversized entry alone fails");
    let id = *results[1].as_ref().unwrap();
    assert_eq!(
        store.engine.get_fact(id).await.unwrap().content,
        "c [REDACTED:machine-token]"
    );
    for s in &recorder.seen() {
        assert_no_canary_in(s, &c);
    }
    let (_dir, db) = store.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 4 + 1 + 1);
    assert_store_files_clean(db.parent().unwrap(), &c);
}

fn provenance(root: &str, branch: &str, key: &Value) -> Value {
    json!({ "facts": { "root": root, "branch": branch }, "dropped": {}, "key": key, "rules": 1 })
}

#[tokio::test]
async fn event_payloads_redact_all_but_genuine_provenance() {
    let c = Canaries::new();
    let store = Store::new();
    // A dot-free root holding a UUID scores above the entropy threshold: genuine provenance keeps
    // it, while the same string anywhere else is redacted.
    let root = "/srv/work/9932fd5f-1a85-4237-809b-5e6ae329dda1/scratchpad/cache";
    store
        .engine
        .ingest(&event(json!({ "project": provenance(root, "main", &json!(null)), "error": root, "text": c.text() })))
        .await
        .unwrap();
    store
        .engine
        .ingest_at(
            "unscoped:laptop",
            &event(
                json!({ "project": provenance(root, &c.machine_token, &json!(c.machine_token)) }),
            ),
        )
        .await
        .unwrap();
    store
        .engine
        .ingest(&event(json!({ "project": { "note": c.text() } })))
        .await
        .unwrap();
    let payloads = events(&store.engine).await;
    assert_eq!(payloads[0]["project"]["facts"]["root"], root);
    assert_eq!(payloads[0]["error"], "[REDACTED:high-entropy]");
    assert_eq!(payloads[0]["text"], SCRUBBED);
    assert_eq!(payloads[1]["project"]["facts"]["root"], root);
    assert_eq!(
        payloads[1]["project"]["facts"]["branch"],
        "[REDACTED:machine-token]"
    );
    assert_eq!(payloads[1]["project"]["key"], "[REDACTED:machine-token]");
    assert_eq!(payloads[2]["project"]["note"], SCRUBBED);
    let long = format!("{} {}", "word ".repeat(820), c.aws);
    store
        .engine
        .ingest(&event(json!({ "text": long })))
        .await
        .unwrap();
    let kept = events(&store.engine).await[3]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        kept,
        format!("{} [REDACTED:aws-access-key]", "word ".repeat(820)),
        "a 4 KiB leaf is kept whole"
    );
    let (_dir, db) = store.close().await;
    assert_eq!(
        read_redaction_counts(&db).unwrap().redacted,
        1 + 4 + 2 + 4 + 1
    );
    assert_store_files_clean(db.parent().unwrap(), &c);
}

#[tokio::test]
async fn forget_and_move_records_redact() {
    let c = Canaries::new();
    let store = Store::new();
    let homes = vec!["unscoped:laptop".to_owned()];
    let recorder = Arc::new(Recorder::default());
    let id = store
        .engine
        .add_fact(
            &request("plain fact".into(), json!({})),
            Arc::new(RecordingEmbedder(recorder)),
            None,
        )
        .await
        .unwrap();
    let record = event(json!({ "why": c.text() }));
    let moved = store
        .engine
        .add_fact_deferred(&request("another plain fact".into(), json!({})))
        .await
        .unwrap();
    let from = store.engine.resolve_scope_path("unscoped:laptop").unwrap();
    store
        .engine
        .move_fact(moved, from, "user:alice", "unscoped:laptop", &record)
        .await
        .unwrap();
    // A forget of a fact that does not exist stores its record only, never its reason.
    let (outcome, _) = store
        .engine
        .forget_fact(
            9_999,
            &homes,
            &format!("because {}", c.aws),
            "unscoped:laptop",
            &record,
        )
        .await
        .unwrap();
    assert_eq!(outcome, ForgetOutcome::NotFound);
    let (outcome, _) = store
        .engine
        .forget_fact(
            id,
            &homes,
            &format!("because {}", c.aws),
            "unscoped:laptop",
            &record,
        )
        .await
        .unwrap();
    assert_eq!(outcome, ForgetOutcome::Forgotten);
    let explained = format!("{:?}", store.engine.explain_fact(id).await.unwrap());
    assert!(
        explained.contains("because [REDACTED:aws-access-key]"),
        "{explained}"
    );
    assert!(
        events(&store.engine)
            .await
            .iter()
            .all(|p| p["why"] == SCRUBBED || p.get("why").is_none())
    );
    let (_dir, db) = store.close().await;
    // Three records of 4 secrets each (a move, a forget that found nothing, a forget), and one
    // stored reason.
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 4 + 4 + 4 + 1);
    assert_store_files_clean(db.parent().unwrap(), &c);
}

#[tokio::test]
async fn checkpoints_redact_and_stay_whole() {
    let c = Canaries::new();
    let store = Store::new();
    let summary = format!("{} {}", "step ".repeat(820), c.text());
    store
        .engine
        .checkpoint_session(
            "s1",
            Some("unscoped:laptop"),
            Some(&summary),
            Some(json!({ "note": c.text() })),
        )
        .await
        .unwrap();
    store
        .engine
        .ensure_scope_path("unscoped:laptop")
        .await
        .unwrap();
    let context = store
        .engine
        .load_context("unscoped:laptop", 5, 5)
        .await
        .unwrap();
    let checkpoint = context.last_checkpoint.unwrap();
    assert_eq!(
        checkpoint.summary.unwrap(),
        format!("{} {SCRUBBED}", "step ".repeat(820))
    );
    assert_eq!(checkpoint.metadata["note"], SCRUBBED);
    let (_dir, db) = store.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 8);
    assert_store_files_clean(db.parent().unwrap(), &c);
}

#[tokio::test]
async fn a_recorded_embedder_error_is_redacted() {
    let c = Canaries::new();
    let store = Store::new();
    store
        .engine
        .add_fact_deferred(&request("plain".into(), json!({})))
        .await
        .unwrap();
    let NextEmbedding::Due(item) = store.engine.next_embedding().await.unwrap() else {
        panic!("queued")
    };
    let error = MemoryError::from(EmbedderError::Failed(format!("upstream said {}", c.aws)));
    let fingerprint = EmbeddingFingerprint::new("test", "test", DIM);
    store
        .engine
        .settle_embedding(&item, Err(error), &fingerprint, Backoff::DEFAULT)
        .await
        .unwrap();
    let (_dir, db) = store.close().await;
    let last = read_embed_queue_status(&db).unwrap().last_error.unwrap();
    assert!(last.contains("[REDACTED:aws-access-key]"), "{last}");
    assert_store_files_clean(db.parent().unwrap(), &c);
}

#[tokio::test]
async fn a_read_only_engine_answers_read_only_first() {
    let store = Store::new();
    let (dir, db) = store.close().await;
    let oversized = "x".repeat(me_types::limits::MAX_PAYLOAD_BYTES + 1);
    let read_only = MemoryEngine::builder(DIM)
        .path(&db)
        .read_only(true)
        .build()
        .unwrap();
    // #972: `ReadOnly` wins over an oversized or a secret-bearing input on every gated write that
    // takes caller text (`settle_embedding`, `apply_cycle_report` and `bootstrap_*` check it on
    // their first line, before any input is read).
    let c = Canaries::new();
    let embedder = || Arc::new(RecordingEmbedder(Arc::default())) as Arc<dyn EmbeddingProvider>;
    let fingerprint = EmbeddingFingerprint::new("test", "test", DIM);
    for text in [oversized.clone(), c.text()] {
        let refusals = [
            read_only
                .add_fact(&request(text.clone(), json!({})), embedder(), None)
                .await,
            read_only
                .add_fact_deferred(&request(text.clone(), json!({})))
                .await,
            read_only
                .add_fact_precomputed(
                    &request(text.clone(), json!({})),
                    vec![0.5; DIM],
                    &fingerprint,
                    None,
                )
                .await,
            read_only
                .add_facts_batch(&[request(text.clone(), json!({}))], embedder(), None)
                .await
                .map(|_| 0),
            read_only.ingest(&event(json!({ "text": text }))).await,
            read_only
                .resolve_conflict(
                    &Adds,
                    1,
                    &NewFact::builder(text.clone(), vec![0.5; DIM], FactType::Semantic).build(),
                )
                .await
                .map(|_| 0),
            read_only
                .forget_fact(
                    1,
                    &["/".into()],
                    &text,
                    "unscoped:laptop",
                    &event(json!({})),
                )
                .await
                .map(|_| 0),
            read_only
                .checkpoint_session("s", None, Some(&text), None)
                .await
                .map(|()| 0),
            read_only
                .add_facts_batch_partial(&[request(text.clone(), json!({}))], embedder(), None)
                .await
                .map(|_| 0),
            read_only
                .ingest_at("unscoped:laptop", &event(json!({ "text": text })))
                .await,
            read_only
                .move_fact(
                    1,
                    1,
                    "unscoped:laptop",
                    "unscoped:laptop",
                    &event(json!({ "text": text })),
                )
                .await,
            read_only
                .record_activity(
                    &memory_engine::RecordActivityRequest {
                        tool_name: "Write".into(),
                        args: json!({ "content": text }),
                        result: Some(text.clone()),
                        session_id: "s".into(),
                        timestamp: chrono::Utc::now(),
                        scope_path: None,
                        outcome_class: None,
                        touched_paths: vec![],
                        cwd: None,
                    },
                    None,
                    &memory_engine::ActivityFilterConfig::default(),
                )
                .await
                .map(|_| 0),
        ];
        for (i, refusal) in refusals.into_iter().enumerate() {
            let err = refusal.unwrap_err();
            assert!(matches!(err, MemoryError::ReadOnly), "write {i}: {err}");
        }
    }
    drop(read_only);
    drop(dir);
}

#[tokio::test]
async fn an_oversized_write_stores_nothing() {
    let store = Store::new();
    let (dir, db) = store.close().await;
    let oversized = "x".repeat(me_types::limits::MAX_PAYLOAD_BYTES + 1);
    let engine = MemoryEngine::builder(DIM).path(&db).build().unwrap();
    for err in [
        engine
            .add_fact_deferred(&request(oversized.clone(), json!({})))
            .await
            .unwrap_err(),
        engine
            .ingest(&event(json!({ "text": oversized })))
            .await
            .unwrap_err(),
        engine
            .checkpoint_session("s", None, Some(&oversized), None)
            .await
            .unwrap_err(),
        engine
            .forget_fact(
                1,
                &["/".into()],
                &oversized,
                "unscoped:laptop",
                &event(json!({})),
            )
            .await
            .unwrap_err(),
    ] {
        assert!(
            err.to_string().contains("too large") || format!("{err:?}").contains("PayloadTooLarge"),
            "{err:?}"
        );
    }
    assert_eq!(events(&engine).await, Vec::<Value>::new());
    drop(dir);
}

// ── Caller vectors and derived text (plan decision 7) ────────────────────────

const fn is_redaction_refusal(err: &MemoryError) -> bool {
    matches!(
        err,
        MemoryError::Conflict(memory_engine::error::ConflictError::Redaction(_))
    )
}

struct Adds;

impl ConflictArbiter for Adds {
    fn arbitrate(&self, _old: &Fact, _new: &Fact) -> Result<CrudDecision> {
        Ok(CrudDecision::Add)
    }
}

#[tokio::test]
async fn a_caller_vector_beside_unredacted_text_is_refused() {
    let c = Canaries::new();
    let store = Store::new();
    let fingerprint = EmbeddingFingerprint::new("test", "test", DIM);
    let vector = vec![0.5; DIM];
    let err = store
        .engine
        .add_fact_precomputed(
            &request(c.text(), json!({})),
            vector.clone(),
            &fingerprint,
            None,
        )
        .await
        .unwrap_err();
    assert!(is_redaction_refusal(&err), "{err}");
    // The API stays open: a precomputed write with nothing to redact in its embedded text is
    // stored, and its metadata, which is not embedded, is scrubbed.
    let id = store
        .engine
        .add_fact_precomputed(
            &request("plain wisdom".into(), json!({ "note": c.text() })),
            vector.clone(),
            &fingerprint,
            None,
        )
        .await
        .unwrap();
    let fact = store.engine.get_fact(id).await.unwrap();
    assert_eq!(
        (fact.content.as_str(), &fact.metadata["note"]),
        ("plain wisdom", &json!(SCRUBBED))
    );
    let candidate = NewFact::builder(c.text(), vector, FactType::Semantic).build();
    let err = store
        .engine
        .resolve_conflict(&Adds, id, &candidate)
        .await
        .unwrap_err();
    assert!(is_redaction_refusal(&err), "{err}");
    assert_eq!(
        store.engine.list_active_facts(None).await.unwrap().len(),
        1,
        "nothing else was stored"
    );
    let (_dir, db) = store.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 4);
    assert_store_files_clean(db.parent().unwrap(), &c);
}

/// A proposer that always proposes merging every fact of its window under one summary.
struct Merges(String);

impl DeltaProposer for Merges {
    fn propose(&self, window: &[Fact], _prior: &[Fact]) -> Result<ConsolidationProposal> {
        let source_ids = window.iter().map(|f| f.id).collect();
        Ok(ConsolidationProposal {
            merges: vec![MergeGroup {
                source_ids,
                summary: self.0.clone(),
            }],
        })
    }
}

async fn two_facts(store: &Store, recorder: &Arc<Recorder>) {
    for text in ["fact one", "fact two"] {
        store
            .engine
            .add_fact(
                &request(text.into(), json!({})),
                Arc::new(RecordingEmbedder(Arc::clone(recorder))),
                None,
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn cycle_deltas_with_unredacted_text_are_skipped_and_counted() {
    let c = Canaries::new();
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    two_facts(&store, &recorder).await;
    // Without the wrapper the cycle embeds and submits the raw summary: the core refuses to
    // store it, counts it as blocked like a tombstone-blocked delta, and the window is still
    // dream-marked, so the deterministic proposer cannot livelock on it (#641).
    let raw = LlmDreamCycle::new(
        Arc::new(Merges(c.text())),
        Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
    );
    let mut report = store.engine.run_dream_cycle(&raw).await.unwrap();
    let target = store.engine.list_active_facts(None).await.unwrap()[0].id;
    report.deltas.push(CycleDelta::AddFact(
        NewFact::builder(c.text(), vec![0.5; DIM], FactType::Semantic).build(),
    ));
    report.deltas.push(CycleDelta::Quarantine {
        fact_id: target,
        reason: format!("because {}", c.aws),
    });
    let applied = store.engine.apply_cycle_report(&report).await.unwrap();
    assert_eq!(
        (
            applied.blocked,
            applied.synthesized,
            applied.facts_added,
            applied.quarantined
        ),
        (2, 0, 0, 1)
    );
    let quarantined = store.engine.get_fact(target).await.unwrap();
    assert_eq!(
        quarantined.metadata["quarantine"]["reason"],
        "because [REDACTED:aws-access-key]"
    );
    let again = store.engine.run_dream_cycle(&raw).await.unwrap();
    assert!(
        !again
            .deltas
            .iter()
            .any(|d| matches!(d, CycleDelta::Synthesize { .. })),
        "the marked window proposes nothing"
    );
    let (_dir, db) = store.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 1);
    assert_store_files_clean(db.parent().unwrap(), &c);
}

struct Noops;

impl ConflictArbiter for Noops {
    fn arbitrate(&self, _old: &Fact, _new: &Fact) -> Result<CrudDecision> {
        Ok(CrudDecision::Noop)
    }
}

/// Decision 6: a finding counts only when the text holding it is written.
#[tokio::test]
async fn findings_in_text_never_written_do_not_count() {
    let c = Canaries::new();
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    two_facts(&store, &recorder).await;
    let facts = store.engine.list_active_facts(None).await.unwrap();
    let (kept, gone) = (facts[0].id, facts[1].clone());
    // A candidate the arbiter declines stores nothing of its metadata.
    let candidate = NewFact::builder("plain candidate", vec![0.5; DIM], FactType::Semantic)
        .metadata(json!({ "note": c.text() }))
        .build();
    let resolution = store
        .engine
        .resolve_conflict(&Noops, kept, &candidate)
        .await
        .unwrap();
    assert_eq!(resolution.new_fact_id, None);
    // A delta whose text was forgotten is blocked (#1023), so its metadata is never written.
    store
        .engine
        .forget_fact(
            gone.id,
            &["/".into(), "unscoped:laptop".into()],
            "wrong",
            "unscoped:laptop",
            &event(json!({})),
        )
        .await
        .unwrap();
    let raw = LlmDreamCycle::new(
        Arc::new(Merges("plain".into())),
        Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
    );
    let mut report = store.engine.run_dream_cycle(&raw).await.unwrap();
    report.deltas = vec![CycleDelta::AddFact(
        NewFact::builder(gone.content.clone(), vec![0.5; DIM], FactType::Semantic)
            .metadata(json!({ "note": c.text() }))
            .build(),
    )];
    let applied = store.engine.apply_cycle_report(&report).await.unwrap();
    assert_eq!((applied.blocked, applied.facts_added), (1, 0));
    let (_dir, db) = store.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 0);
}

/// The report's own metadata reaches the cycle history; an oversized reason is never scanned;
/// a promotion the tombstone blocks writes nothing and counts nothing.
/// Scrubbed metadata stays within the limit: an `AddFact` whose placeholders outgrow it is skipped
/// as blocked and the rest of the report still applies (#641); a quarantine always applies, with a
/// marker when its reason has no room.
#[tokio::test]
async fn a_delta_whose_metadata_would_outgrow_the_limit_is_blocked() {
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    two_facts(&store, &recorder).await;
    let max = me_types::limits::MAX_PAYLOAD_BYTES;
    let full = store
        .engine
        .add_fact(
            &request("full note".into(), json!({ "note": "n".repeat(max - 30) })),
            Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
            None,
        )
        .await
        .unwrap();
    let escaped = store
        .engine
        .add_fact(
            &request(
                "escaped note".into(),
                json!({ "note": "n".repeat(max - 30) }),
            ),
            Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
            None,
        )
        .await
        .unwrap();
    // 100 one-byte URL passwords, each becoming a 38-byte placeholder.
    let passwords = "https://u:p@h ".repeat(100);
    let growing = json!({ "note": format!("{passwords}{}", "z".repeat(max - 2000)) });
    let raw = LlmDreamCycle::new(
        Arc::new(Merges("plain".into())),
        Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
    );
    let mut report = store.engine.run_dream_cycle(&raw).await.unwrap();
    report.deltas = vec![
        CycleDelta::Quarantine {
            fact_id: full,
            reason: "short".into(),
        },
        // Eight quotes: 15 bytes raw, 25 as stored, so its marker (21 as stored) is the shorter.
        CycleDelta::Quarantine {
            fact_id: escaped,
            reason: "\"\"\"\"\"\"\"\"x7chars".into(),
        },
        CycleDelta::AddFact(
            NewFact::builder("plain addition", vec![0.5; DIM], FactType::Semantic)
                .metadata(growing)
                .build(),
        ),
    ];
    let applied = store.engine.apply_cycle_report(&report).await.unwrap();
    assert_eq!(
        (applied.blocked, applied.quarantined, applied.facts_added),
        (1, 2, 0)
    );
    // The quarantine applies even to a fact already at the limit; a reason shorter than its
    // marker is kept.
    let quarantined = store.engine.get_fact(full).await.unwrap();
    assert_eq!(quarantined.metadata["quarantine"]["reason"], "short");
    let escaped = store.engine.get_fact(escaped).await.unwrap();
    assert_eq!(
        escaped.metadata["quarantine"]["reason"],
        "[omitted: 15 bytes]"
    );
}

/// An oversized label is a marker, never a refusal: the report still applies and its window is
/// still marked (#641).
#[tokio::test]
async fn an_oversized_cycle_label_is_a_marker() {
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    two_facts(&store, &recorder).await;
    let raw = LlmDreamCycle::new(
        Arc::new(Merges("plain".into())),
        Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
    );
    let mut report = store.engine.run_dream_cycle(&raw).await.unwrap();
    report.metadata.method_version = "v".repeat(me_types::limits::MAX_PAYLOAD_BYTES + 1);
    report.deltas.clear();
    store.engine.apply_cycle_report(&report).await.unwrap();
    let history = store
        .engine
        .get_config("dream_cycle_history")
        .await
        .unwrap()
        .unwrap();
    let marker = format!(
        "[omitted: {} bytes]",
        me_types::limits::MAX_PAYLOAD_BYTES + 1
    );
    assert!(history.contains(&marker), "{history}");
}

#[tokio::test]
async fn cycle_history_and_oversized_deltas() {
    let c = Canaries::new();
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    two_facts(&store, &recorder).await;
    let kept = store.engine.list_active_facts(None).await.unwrap()[0].id;
    // The same text in two Homes; a forget in one keeps the other active, but its text is
    // tombstoned, so a promotion of the other is blocked (#1023).
    let embedder =
        || Arc::new(RecordingEmbedder(Arc::clone(&recorder))) as Arc<dyn EmbeddingProvider>;
    let mut there = request("shared words".into(), json!({}));
    there.scope = Some("unscoped:desktop".into());
    let copy = store
        .engine
        .add_fact(&there, embedder(), None)
        .await
        .unwrap();
    let here = store
        .engine
        .add_fact(&request("shared words".into(), json!({})), embedder(), None)
        .await
        .unwrap();
    store
        .engine
        .forget_fact(
            here,
            &["unscoped:laptop".into()],
            "wrong",
            "unscoped:laptop",
            &event(json!({})),
        )
        .await
        .unwrap();
    let gone = copy;
    // A quarantine whose reason fits, but not beside the fact's own metadata once merged.
    let half = me_types::limits::MAX_PAYLOAD_BYTES * 6 / 10;
    let heavy = store
        .engine
        .add_fact(
            &request("heavy note".into(), json!({ "note": "n".repeat(half) })),
            embedder(),
            None,
        )
        .await
        .unwrap();
    let raw = LlmDreamCycle::new(
        Arc::new(Merges("plain".into())),
        Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
    );
    let mut report = store.engine.run_dream_cycle(&raw).await.unwrap();
    report.metadata.method_version = format!("v1 {}", c.aws);
    let now = chrono::Utc::now();
    report.deltas = vec![
        CycleDelta::Quarantine {
            fact_id: kept,
            reason: "q".repeat(me_types::limits::MAX_PAYLOAD_BYTES + 1),
        },
        CycleDelta::Quarantine {
            fact_id: heavy,
            reason: "r".repeat(half),
        },
        CycleDelta::Promote {
            fact_id: gone,
            provenance: memory_engine::types::PromotionProvenance {
                source_count: 1,
                session_count: 1,
                date_range_start: now,
                date_range_end: now,
                confidence: 0.9,
                method_version: format!("v1 {}", c.aws),
                representative_ids: vec![gone],
            },
        },
    ];
    let applied = store.engine.apply_cycle_report(&report).await.unwrap();
    // Both quarantines apply, each with a marker for a reason; the promotion is blocked.
    assert_eq!((applied.blocked, applied.quarantined), (1, 2));
    for (id, len) in [
        (kept, me_types::limits::MAX_PAYLOAD_BYTES + 1),
        (heavy, half),
    ] {
        let fact = store.engine.get_fact(id).await.unwrap();
        assert_eq!(
            fact.metadata["quarantine"]["reason"],
            format!("[omitted: {len} bytes]")
        );
    }
    let history = store
        .engine
        .get_config("dream_cycle_history")
        .await
        .unwrap()
        .unwrap();
    assert!(
        history.contains("v1 [REDACTED:aws-access-key]"),
        "{history}"
    );
    let (_dir, db) = store.close().await;
    assert_eq!(
        read_redaction_counts(&db).unwrap().redacted,
        1,
        "the history's key only"
    );
    assert_store_files_clean(db.parent().unwrap(), &c);
}

#[tokio::test]
async fn a_scrubbing_proposer_hands_the_cycle_redacted_summaries() {
    let c = Canaries::new();
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    two_facts(&store, &recorder).await;
    let cycle = LlmDreamCycle::new(
        Arc::new(ScrubbingProposer(Arc::new(Merges(c.text())))),
        Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
    );
    let report = store.engine.run_dream_cycle(&cycle).await.unwrap();
    let applied = store.engine.apply_cycle_report(&report).await.unwrap();
    assert_eq!(applied.synthesized, 1);
    let merged = store
        .engine
        .get_fact(applied.synthesized_fact_ids[0])
        .await
        .unwrap();
    assert_eq!(merged.content, SCRUBBED);
    for s in &recorder.seen() {
        assert_no_canary_in(s, &c);
    }
    // A summary over the write limit could never be stored: the wrapper drops its group.
    let oversized = "x".repeat(me_types::limits::MAX_PAYLOAD_BYTES + 1);
    let proposal = ScrubbingProposer(Arc::new(Merges(oversized)))
        .propose(&[], &[])
        .unwrap();
    assert_eq!(proposal.merges.len(), 0);
    let (_dir, db) = store.close().await;
    assert_store_files_clean(db.parent().unwrap(), &c);
}

struct Echoes(String);

impl SummaryGenerator for Echoes {
    fn summarize(&self, _items: &[memory_engine::SummarizableContent<'_>]) -> Result<String> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn consolidation_summaries_are_redacted() {
    let c = Canaries::new();
    let store = Store::new();
    let recorder = Arc::new(Recorder::default());
    // A single-linkage chain that forms one cluster: adjacent cosines of about 0.883 sit between
    // the 0.85 cluster and 0.90 dedup thresholds (the fixture of the engine's own test).
    let fingerprint = EmbeddingFingerprint::new("test", "test", DIM);
    for (text, vector) in [
        ("note a", vec![1.0, 0.0, 0.0, 0.0]),
        ("note b", vec![0.8829, 0.4695, 0.0, 0.0]),
        ("note c", vec![0.5592, 0.829, 0.0, 0.0]),
    ] {
        store
            .engine
            .add_fact_precomputed(&request(text.into(), json!({})), vector, &fingerprint, None)
            .await
            .unwrap();
    }
    let before = recorder.seen().len();
    store
        .engine
        .consolidate(
            Arc::new(Echoes(c.text())),
            Arc::new(RecordingEmbedder(Arc::clone(&recorder))),
            &ConsolidationConfig::default(),
        )
        .await
        .unwrap();
    let mut summaries = Vec::new();
    for level in [
        ConsolidationLevel::Local,
        ConsolidationLevel::Cluster,
        ConsolidationLevel::Global,
    ] {
        summaries.extend(store.engine.list_summaries(&level).await.unwrap());
    }
    assert!(
        !summaries.is_empty(),
        "the identical vectors cluster into a summary"
    );
    assert!(
        summaries.iter().all(|s| s.content == SCRUBBED),
        "{summaries:?}"
    );
    for s in &recorder.seen()[before..] {
        assert_no_canary_in(s, &c);
    }
    let (_dir, db) = store.close().await;
    assert_store_files_clean(db.parent().unwrap(), &c);
}

/// A captured event (#1025): its prompt, tool input and result, final message and compaction
/// summary are redacted in the payload, the memories and the activity, and counted.
#[tokio::test]
async fn a_captured_event_redacts_its_texts() {
    use memory_engine::types::lifecycle::{
        Actor, Content, Delivery, NeutralRecord, Recorded, StopOutcome, ToolOutcome,
    };

    let c = Canaries::new();
    let store = Store::new();
    let now = chrono::Utc::now();
    let send = |id: &str, content: Content| {
        let delivery = Delivery {
            machine: "laptop".into(),
            event_id: id.into(),
            time: now,
            seq: 1,
            harness: "claude-code".into(),
            client_version: "1.0.0".into(),
            body_sha256: [1; 32],
            body_size: 1,
            received_at: now,
            home: "user:u/project:p".into(),
            provenance: json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
        };
        let record = NeutralRecord {
            session_id: "s".into(),
            actor: Actor::Main,
            cwd: None,
            harness_version: None,
            content,
        };
        let engine = &store.engine;
        async move {
            engine
                .record_lifecycle_event(&delivery, Ok(record))
                .await
                .unwrap()
        }
    };
    let texts = [
        Content::Prompt {
            text: Some(c.text()),
        },
        Content::ToolUse {
            tool: "Bash".into(),
            input: json!({ "command": c.text() }),
            result: Some(c.text()),
            outcome: ToolOutcome::Success,
            paths: vec![],
            call: None,
            dedup_window_secs: Some(300),
        },
        Content::Stop {
            outcome: StopOutcome::Completed,
            message: Some(c.text()),
        },
        Content::Compact {
            trigger: Some("manual".into()),
            summary: c.text(),
        },
    ];
    for (n, content) in texts.into_iter().enumerate() {
        let stored = send(&format!("e{n}"), content).await;
        assert!(matches!(stored, Recorded::Stored { .. }), "{stored:?}");
    }

    let payloads = events(&store.engine).await;
    let field = |name: &str| {
        payloads
            .iter()
            .find_map(|p| p.get(name).filter(|v| !v.is_null()))
            .unwrap_or_else(|| panic!("no payload holds {name}"))
            .clone()
    };
    assert_eq!(field("prompt"), SCRUBBED);
    assert_eq!(field("result"), SCRUBBED);
    assert_eq!(field("message"), SCRUBBED);
    assert_eq!(field("summary"), SCRUBBED);
    assert_eq!(field("input")["command"], SCRUBBED);
    let memories: Vec<String> = store
        .engine
        .list_active_facts(None)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.content)
        .collect();
    assert_eq!(memories, [SCRUBBED, SCRUBBED]);
    let (dir, db) = store.close().await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (args, result): (String, String) = conn
        .query_row("SELECT args, result_summary FROM activities", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!(args.contains(SCRUBBED), "{args}");
    assert_eq!(result, SCRUBBED);
    drop(conn);
    // Four secrets in each of five texts: the prompt, the input, the result, the message and
    // the summary.
    assert_eq!(read_redaction_counts(&db).unwrap().redacted, 20);
    assert_store_files_clean(dir.path(), &c);
}

/// A backfill import (#1026): every text it stores, the prompt, the tool input and result, the
/// final message, the summary, the working directory, the upload's source, and a memory's body
/// and frontmatter, is redacted with every rule and the author denylist, and counted.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one upload of each kind, then every place a text lands
async fn an_import_redacts_with_the_denylist() {
    use memory_engine::types::lifecycle::{
        Actor, BatchMode, Content, ImportEvent, ImportItem, ImportMemory, MemoryItem,
        NeutralRecord, StopOutcome, ToolOutcome, Upload,
    };

    let c = Canaries::new();
    let host = "fixturehost";
    let denylist = vec![host.to_owned()];
    let text = format!("{} on {host}", c.text());
    let scrubbed = format!("{SCRUBBED} on [REDACTED:denylist]");
    let store = Store::new();
    let upload = |kind: &str| Upload {
        machine: "laptop".into(),
        harness: "claude-code".into(),
        client_version: "0.1.0".into(),
        kind: kind.into(),
        source: format!("projects/{host}/{}", c.aws),
        received_at: chrono::Utc::now(),
        home: "user:u/project:p".into(),
        provenance: json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
        body_sha256: [7; 32],
        census: json!({}),
        copy: false,
    };
    let event = |n: u8, content: Content| {
        ImportItem::Event(ImportEvent {
            event_id: format!("bf.{n}.x"),
            time: chrono::Utc::now() - chrono::Duration::days(1),
            seq: u32::from(n),
            body_sha256: [n; 32],
            body_size: 10,
            line: usize::from(n),
            record: NeutralRecord {
                session_id: "s".into(),
                actor: Actor::Main,
                cwd: Some(format!("/work/{host}/{}", c.aws)),
                harness_version: None,
                content,
            },
        })
    };
    let items = vec![
        event(
            1,
            Content::Prompt {
                text: Some(text.clone()),
            },
        ),
        event(
            2,
            Content::ToolUse {
                tool: "Bash".into(),
                input: json!({ "command": text }),
                result: Some(text.clone()),
                outcome: ToolOutcome::Success,
                paths: vec![],
                call: None,
                dedup_window_secs: Some(300),
            },
        ),
        event(
            3,
            Content::Stop {
                outcome: StopOutcome::Completed,
                message: Some(text.clone()),
            },
        ),
        event(
            4,
            Content::Compact {
                trigger: Some("manual".into()),
                summary: text.clone(),
            },
        ),
    ];
    let memory = vec![ImportItem::Memory(ImportMemory {
        event_id: "bf.m.1".into(),
        body_sha256: [9; 32],
        body_size: 10,
        item: MemoryItem {
            text: format!("{text}, remembered"),
            fact_type: FactType::Semantic,
            importance: 0.6,
            time: chrono::Utc::now() - chrono::Duration::days(2),
            metadata: json!({ "source": "memory-dir", "file": "m.md", "description": text }),
        },
    })];
    for (kind, items) in [("transcript", items), ("memory-file", memory)] {
        let upload = upload(kind);
        let dry = store
            .engine
            .import_backfill(&upload, items.clone(), &denylist, BatchMode::DryRun)
            .await
            .unwrap();
        assert!(!dry.source.contains(host) && !dry.source.contains(&c.aws));
        let write = store
            .engine
            .import_backfill(
                &upload,
                items,
                &denylist,
                BatchMode::Write {
                    preview: dry.preview,
                },
            )
            .await
            .unwrap();
        assert!(write.committed);
    }

    let payloads = events(&store.engine).await;
    let field = |name: &str| {
        payloads
            .iter()
            .find_map(|p| p.get(name).filter(|v| !v.is_null()))
            .unwrap_or_else(|| panic!("no payload holds {name}"))
            .clone()
    };
    assert_eq!(field("prompt"), scrubbed);
    assert_eq!(field("result"), scrubbed);
    assert_eq!(field("message"), scrubbed);
    assert_eq!(field("summary"), scrubbed);
    assert_eq!(field("input")["command"], scrubbed);
    let mut memories: Vec<Fact> = store.engine.list_active_facts(None).await.unwrap();
    memories.sort_by_key(|f| f.id);
    let texts: Vec<&str> = memories.iter().map(|f| f.content.as_str()).collect();
    let remembered = format!("{scrubbed}, remembered");
    assert_eq!(texts, [&scrubbed, &scrubbed, &remembered]);
    assert_eq!(memories[2].metadata["description"], scrubbed);
    let (dir, db) = store.close().await;
    // Four secrets and the literal in each of the five event texts, in the memory and in its
    // description; a canary and the literal in each of the four cwds and in the source the
    // memory's event stores (a transcript's events do not store it).
    assert_eq!(
        read_redaction_counts(&db).unwrap().redacted,
        5 * 5 + 2 * 5 + 4 * 2 + 2
    );
    assert_store_files_clean(dir.path(), &c);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !bytes.windows(host.len()).any(|w| w == host.as_bytes()),
            "the denylist literal is on disk"
        );
    }
}
