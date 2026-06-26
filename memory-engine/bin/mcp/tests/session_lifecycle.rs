//! Dispatch-level integration tests for the six session/debug MCP tools that had
//! zero `tools::dispatch()` coverage (#318, part of #256):
//! `memory_record_activity`, `memory_checkpoint_session`, `memory_load_context`,
//! `memory_replay_events`, `memory_fact_history`, `memory_bootstrap_session`.
//!
//! Each test bypasses the MCP transport and drives the real handler through
//! `tools::dispatch()`, mirroring the harness in `cognitive_tools.rs` /
//! `input_caps.rs`: an in-memory `MemoryEngine`, the `args()` map builder, and
//! `extract_json()` to read the JSON body out of a `CallToolResult`.

use std::sync::Arc;

use chrono::{Duration, Utc};
use memory_engine::MemoryEngine;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use memory_engine_mcp::tools;
use rmcp::model::{CallToolResult, ErrorCode, ErrorData, RawContent};
use serde_json::{Map, Value, json};

const DIM: usize = 3;

struct FakeEmbed;
impl EmbeddingProvider for FakeEmbed {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3])
    }
    fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
        memory_engine::EmbeddingFingerprint::new("mock", "test", 3)
    }
}

fn engine() -> (MemoryEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let e = MemoryEngine::builder(DIM)
        .path(dir.path().join("t.db"))
        .build()
        .unwrap();
    (e, dir)
}

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn cfg() -> memory_engine::ActivityFilterConfig {
    memory_engine::ActivityFilterConfig::default()
}

/// Dispatch with no embedder/summary provider (the in-memory test default).
async fn call(
    engine: &MemoryEngine,
    name: &str,
    a: Map<String, Value>,
) -> Result<CallToolResult, ErrorData> {
    tools::dispatch(name, a, engine, None, None, DIM, &cfg()).await
}

fn extract_json(result: &CallToolResult) -> Value {
    match &result.content[0].raw {
        RawContent::Text(t) => serde_json::from_str(&t.text).unwrap(),
        _ => panic!("expected text content"),
    }
}

/// Seed a fact directly (dispatch's `add_fact` needs an HTTP embedder; the
/// engine's own `add_fact` accepts an injected provider). Returns the fact id.
/// Setting `scope` registers the scope path in the engine's scope tree, which is
/// what makes a subsequent `memory_load_context` on that scope resolve.
async fn seed(
    engine: &MemoryEngine,
    content: &str,
    scope: Option<&str>,
    opts: Option<AddFactOptions>,
) -> i64 {
    let req = AddFactRequest {
        content: content.into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: scope.map(Into::into),
        opts,
    };
    engine
        .add_fact(
            &req,
            Arc::new(FakeEmbed) as Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// memory_record_activity — happy path + dedup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn record_activity_happy_path_records() {
    let (engine, _d) = engine();

    let body = extract_json(
        &call(
            &engine,
            "memory_record_activity",
            args(&[
                ("tool", json!("Read")),
                ("session_id", json!("sess-1")),
                ("args", json!({ "path": "/etc/hosts" })),
                ("result", json!("ok")),
            ]),
        )
        .await
        .unwrap(),
    );

    // Default ActivityFilterConfig has empty ignore/promote patterns → Record.
    assert_eq!(body["status"], json!("recorded"));
    assert_eq!(body["was_deduplicated"], json!(false));
    assert!(
        body["activity_id"].as_i64().is_some_and(|id| id > 0),
        "a recorded activity must carry a positive id: {body}"
    );
    // No embedder + non-promote filter → no fact promotion.
    assert!(body["promoted_fact_id"].is_null());
}

#[tokio::test]
async fn record_activity_second_identical_call_is_deduplicated() {
    let (engine, _d) = engine();
    // Identical (session_id, tool, args, default outcome_class) within the 300s
    // default dedup window must collapse: first recorded, second deduplicated.
    let activity = args(&[
        ("tool", json!("Read")),
        ("session_id", json!("sess-dedup")),
        ("args", json!({ "path": "/etc/hosts" })),
        ("result", json!("ok")),
    ]);

    let first = extract_json(
        &call(&engine, "memory_record_activity", activity.clone())
            .await
            .unwrap(),
    );
    assert_eq!(
        first["was_deduplicated"],
        json!(false),
        "first call records"
    );
    assert_eq!(first["status"], json!("recorded"));

    let second = extract_json(
        &call(&engine, "memory_record_activity", activity)
            .await
            .unwrap(),
    );
    assert_eq!(
        second["was_deduplicated"],
        json!(true),
        "second identical call within the window must dedup: {second}"
    );
    assert_eq!(second["status"], json!("deduplicated"));
    // Dedup collapses onto the existing row → same activity id.
    assert_eq!(second["activity_id"], first["activity_id"]);
}

#[tokio::test]
async fn record_activity_missing_required_arg_is_invalid_params() {
    let (engine, _d) = engine();
    // `tool` and `session_id` are both required; omit session_id.
    let err = call(
        &engine,
        "memory_record_activity",
        args(&[("tool", json!("Read"))]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// memory_checkpoint_session — surfaces via memory_load_context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn checkpoint_session_then_load_context_surfaces_checkpoint_and_fact() {
    let (engine, _d) = engine();
    let scope = "project:p";

    // Seed a fact in the scope so (a) the scope path registers in the scope tree
    // (load_context resolves it) and (b) load_context returns a relevant fact.
    let fact_id = seed(&engine, "a relevant project fact", Some(scope), None).await;

    // Checkpoint the session against that scope.
    let cp = extract_json(
        &call(
            &engine,
            "memory_checkpoint_session",
            args(&[
                ("session_id", json!("sess-cp")),
                ("scope", json!(scope)),
                ("summary", json!("wrapped up the session")),
            ]),
        )
        .await
        .unwrap(),
    );
    assert_eq!(cp["session_id"], json!("sess-cp"));
    assert_eq!(cp["checkpointed"], json!(true));

    // load_context must surface the checkpoint (matched by scope) and the fact.
    let ctx = extract_json(
        &call(
            &engine,
            "memory_load_context",
            args(&[("scope", json!(scope))]),
        )
        .await
        .unwrap(),
    );
    assert_eq!(ctx["scope_path"], json!(scope));

    let checkpoint = &ctx["last_checkpoint"];
    assert!(checkpoint.is_object(), "checkpoint must surface: {ctx}");
    assert_eq!(checkpoint["session_id"], json!("sess-cp"));
    assert_eq!(checkpoint["summary"], json!("wrapped up the session"));

    let facts = ctx["relevant_facts"].as_array().unwrap();
    assert!(
        facts.iter().any(|f| f["id"].as_i64() == Some(fact_id)),
        "the seeded fact must be retrievable via load_context: {ctx}"
    );
}

#[tokio::test]
async fn checkpoint_session_records_recent_activity_in_context() {
    let (engine, _d) = engine();
    let scope = "project:active";
    // A recorded activity also registers the scope path (ensure_scope_path), so
    // load_context resolves the scope and returns the activity.
    let _ = call(
        &engine,
        "memory_record_activity",
        args(&[
            ("tool", json!("Edit")),
            ("session_id", json!("sess-act")),
            ("scope", json!(scope)),
            ("result", json!("edited file")),
        ]),
    )
    .await
    .unwrap();

    let ctx = extract_json(
        &call(
            &engine,
            "memory_load_context",
            args(&[("scope", json!(scope))]),
        )
        .await
        .unwrap(),
    );
    let activities = ctx["recent_activities"].as_array().unwrap();
    assert_eq!(
        activities.len(),
        1,
        "the recorded activity must surface: {ctx}"
    );
    assert_eq!(activities[0]["tool_name"], json!("Edit"));
    // No checkpoint was written for this scope.
    assert!(ctx["last_checkpoint"].is_null());
}

#[tokio::test]
async fn load_context_unknown_scope_is_not_found() {
    let (engine, _d) = engine();
    // An unregistered scope path cannot resolve in the scope tree → NotFound,
    // which the handler maps to an error (not an empty context).
    let err = call(
        &engine,
        "memory_load_context",
        args(&[("scope", json!("project:never-seen"))]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND);
}

// ---------------------------------------------------------------------------
// memory_replay_events — returns events and honours `limit`
// ---------------------------------------------------------------------------

/// Ingest `n` events via the real `memory_ingest` tool. Returns nothing; the
/// engine's event log is now populated for replay.
async fn ingest_events(engine: &MemoryEngine, n: usize) {
    for i in 0..n {
        let _ = call(
            engine,
            "memory_ingest",
            args(&[
                ("event_type", json!("Interaction")),
                ("payload", json!({ "n": i })),
                ("source", json!("test")),
            ]),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn replay_events_returns_events_and_honours_limit() {
    let (engine, _d) = engine();
    ingest_events(&engine, 3).await;

    // Absent limit → defaults to a cap of 100, so all 3 come back.
    let all = extract_json(
        &call(&engine, "memory_replay_events", Map::new())
            .await
            .unwrap(),
    );
    assert_eq!(all["count"], json!(3), "all ingested events return: {all}");
    assert_eq!(all["events"].as_array().unwrap().len(), 3);

    // limit=1 returns exactly one event.
    let one = extract_json(
        &call(
            &engine,
            "memory_replay_events",
            args(&[("limit", json!(1))]),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        one["count"],
        json!(1),
        "limit=1 returns a single event: {one}"
    );
    assert_eq!(one["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn replay_events_empty_log_is_empty_not_error() {
    let (engine, _d) = engine();
    let body = extract_json(
        &call(&engine, "memory_replay_events", Map::new())
            .await
            .unwrap(),
    );
    assert_eq!(body["count"], json!(0));
    assert!(body["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn replay_events_inverted_id_range_is_invalid_params() {
    let (engine, _d) = engine();
    let err = call(
        &engine,
        "memory_replay_events",
        args(&[("id_range_start", json!(10)), ("id_range_end", json!(1))]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// memory_fact_history — timeline for a known fact
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fact_history_fresh_fact_has_created_only() {
    let (engine, _d) = engine();
    let fact_id = seed(&engine, "freshly created fact", None, None).await;

    let body = extract_json(
        &call(
            &engine,
            "memory_fact_history",
            args(&[("fact_id", json!(fact_id))]),
        )
        .await
        .unwrap(),
    );
    assert_eq!(body["fact_id"], json!(fact_id));
    let timeline = body["timeline"].as_array().unwrap();
    // A just-added fact has no t_valid/t_invalid/t_expired → only a Created entry.
    assert_eq!(timeline.len(), 1, "fresh fact has a single entry: {body}");
    assert_eq!(timeline[0]["kind"], json!("Created"));
}

#[tokio::test]
async fn fact_history_validity_window_yields_full_lifecycle() {
    let (engine, _d) = engine();
    // Set the bi-temporal real-world validity window so the derived timeline
    // carries the became-valid / became-invalid lifecycle transitions — the
    // "after adding/modifying a fact" path of #318.
    let t_valid = Utc::now() - Duration::days(2);
    let t_invalid = Utc::now() - Duration::days(1);
    let opts = AddFactOptions {
        t_valid: Some(t_valid),
        t_invalid: Some(t_invalid),
        ..Default::default()
    };
    let fact_id = seed(&engine, "fact with a validity window", None, Some(opts)).await;

    let body = extract_json(
        &call(
            &engine,
            "memory_fact_history",
            args(&[("fact_id", json!(fact_id))]),
        )
        .await
        .unwrap(),
    );
    let kinds: Vec<&str> = body["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    // Sorted by timestamp: created (now) is latest; valid/invalid are backdated.
    assert!(kinds.contains(&"Created"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"BecameValid"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"BecameInvalid"), "kinds: {kinds:?}");
    assert_eq!(
        kinds.len(),
        3,
        "exactly created + valid + invalid: {kinds:?}"
    );
}

#[tokio::test]
async fn fact_history_unknown_fact_is_not_found() {
    let (engine, _d) = engine();
    let err = call(
        &engine,
        "memory_fact_history",
        args(&[("fact_id", json!(999_999))]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND);
}

// ---------------------------------------------------------------------------
// memory_bootstrap_session — requires an embedding provider
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bootstrap_session_without_embedder_errors() {
    let (engine, _d) = engine();
    // A minimal, valid (non-oversized) JSONL stub passes the byte-cap gate, so the
    // failure is specifically the documented "embedding provider not configured"
    // path — dispatch is called with `None` embedder.
    let stub = "{\"role\":\"user\",\"content\":\"hello\"}\n";
    let err = call(
        &engine,
        "memory_bootstrap_session",
        args(&[("jsonl_data", json!(stub))]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(
        err.message.contains("embedding provider"),
        "error must name the embedder requirement, got: {}",
        err.message
    );
}

#[tokio::test]
async fn bootstrap_session_missing_jsonl_is_invalid_params() {
    let (engine, _d) = engine();
    // `jsonl_data` is required; the missing-key path errors before the embedder check.
    let err = call(&engine, "memory_bootstrap_session", Map::new())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}
