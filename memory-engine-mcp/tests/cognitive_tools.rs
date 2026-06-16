//! Dispatch-level integration tests for the Phase-5a cognitive MCP tools (#225):
//! `memory_dream_cycle`, `memory_apply_cycle_report`, `memory_get_recent_insights`.

use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use memory_engine::{INSIGHT_MARKER_KEY, MemoryEngine};
use memory_engine_mcp::tools;
use rmcp::model::{CallToolResult, ErrorCode, ErrorData, RawContent};
use serde_json::{Map, Value, json};

const DIM: usize = 3;

struct FakeEmbed;
impl EmbeddingProvider for FakeEmbed {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3]) // identical for all → one DBSCAN cluster
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

fn call(
    engine: &MemoryEngine,
    name: &str,
    a: Map<String, Value>,
) -> Result<CallToolResult, ErrorData> {
    tools::dispatch(name, a, engine, None, None, DIM, &cfg())
}

fn extract_json(result: &CallToolResult) -> Value {
    match &result.content[0].raw {
        RawContent::Text(t) => serde_json::from_str(&t.text).unwrap(),
        _ => panic!("expected text content"),
    }
}

/// Seed a Semantic fact directly (dispatch's add_fact needs an HTTP embedder).
fn seed(engine: &MemoryEngine, content: &str, scope: Option<&str>, insight: bool) -> i64 {
    let metadata =
        insight.then(|| json!({ INSIGHT_MARKER_KEY: { "flushed_at": "2024-01-01T00:00:00Z" } }));
    let req = AddFactRequest {
        content: content.into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: scope.map(Into::into),
        opts: Some(AddFactOptions {
            metadata,
            ..Default::default()
        }),
    };
    engine.add_fact(&req, &FakeEmbed, None).unwrap()
}

#[test]
fn dream_cycle_apply_true_runs_and_applies() {
    let (engine, _d) = engine();
    for i in 0..3 {
        seed(&engine, &format!("pattern {i}"), None, false);
    }
    let body = extract_json(
        &call(
            &engine,
            "memory_dream_cycle",
            args(&[("apply", json!(true))]),
        )
        .unwrap(),
    );
    assert_eq!(body["did_apply"], json!(true));
    assert!(!body["report"]["deltas"].as_array().unwrap().is_empty());
    assert_eq!(
        body["applied"]["promoted"],
        json!(1),
        "the 3-fact cluster promotes one representative"
    );
}

#[test]
fn dream_cycle_dry_run_then_apply_roundtrips() {
    let (engine, _d) = engine();
    for i in 0..3 {
        seed(&engine, &format!("pattern {i}"), None, false);
    }
    // Dry-run: produce but do not apply.
    let dry = extract_json(
        &call(
            &engine,
            "memory_dream_cycle",
            args(&[("apply", json!(false))]),
        )
        .unwrap(),
    );
    assert_eq!(dry["did_apply"], json!(false));
    assert!(dry.get("applied").is_none());

    // Feed the unapplied report back through apply_cycle_report (serde round-trip across the boundary).
    let report = dry["report"].clone();
    let applied = extract_json(
        &call(
            &engine,
            "memory_apply_cycle_report",
            args(&[("report", report)]),
        )
        .unwrap(),
    );
    assert_eq!(applied["promoted"], json!(1));
}

#[test]
fn apply_cycle_report_unknown_fact_is_invalid_params() {
    let (engine, _d) = engine();
    // Hand-built report: AdjustScore on a nonexistent fact → CycleError::UnknownFact → invalid_params.
    let report = json!({
        "deltas": [ { "AdjustScore": { "fact_id": 999_999, "adjustment": 1 } } ],
        "identity": { "anchors": [], "core": [], "predictions": [] },
        "metadata": {
            "cycle_id": 0, "ran_at": "2026-06-16T00:00:00Z",
            "time_window": { "start": "2026-06-16T00:00:00Z", "end": "2026-06-16T00:00:00Z" },
            "facts_selected": 0, "method_version": "test", "processed_ids": []
        }
    });
    let err = call(
        &engine,
        "memory_apply_cycle_report",
        args(&[("report", report)]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

#[test]
fn apply_cycle_report_malformed_json_is_invalid_params() {
    let (engine, _d) = engine();
    let err = call(
        &engine,
        "memory_apply_cycle_report",
        args(&[("report", json!({ "deltas": "nonsense" }))]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

#[test]
fn apply_cycle_report_missing_report_key_is_invalid_params() {
    let (engine, _d) = engine();
    // The `report` key is absent entirely (distinct from present-but-malformed).
    let err = call(&engine, "memory_apply_cycle_report", args(&[])).unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

#[test]
fn dream_cycle_on_empty_store_succeeds_with_no_deltas() {
    let (engine, _d) = engine();
    // No facts seeded: DefaultDreamCycle guards empty/under-min buckets, so the cycle
    // must succeed (not error) and produce an empty delta set.
    let body = extract_json(
        &call(
            &engine,
            "memory_dream_cycle",
            args(&[("apply", json!(true))]),
        )
        .unwrap(),
    );
    assert_eq!(body["did_apply"], json!(true));
    assert!(
        body["report"]["deltas"].as_array().unwrap().is_empty(),
        "empty store yields no deltas"
    );
}

#[test]
fn get_recent_insights_limit_zero_is_invalid_params() {
    let (engine, _d) = engine();
    seed(&engine, "insight", Some("project:p"), true);
    // Schema declares minimum:1; limit=0 must be rejected, not silently empty.
    let err = call(
        &engine,
        "memory_get_recent_insights",
        args(&[("project_path", json!("project:p")), ("limit", json!(0))]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

#[test]
fn get_recent_insights_sparse_depth_shapes_facts() {
    let (engine, _d) = engine();
    seed(&engine, "insight one", Some("project:p"), true);
    let body = extract_json(
        &call(
            &engine,
            "memory_get_recent_insights",
            args(&[
                ("project_path", json!("project:p")),
                ("depth", json!("sparse")),
            ]),
        )
        .unwrap(),
    );
    assert_eq!(body["count"], json!(1));
    // Sparse shaping drops heavy fields (embedding/content_hash) but keeps id + content.
    let fact = &body["insights"][0];
    assert!(fact["id"].as_i64().is_some());
    assert!(
        fact.get("embedding").is_none(),
        "sparse must omit embedding"
    );
}

#[test]
fn get_recent_insights_returns_marked_facts_scoped_and_limited() {
    let (engine, _d) = engine();
    seed(&engine, "insight one", Some("project:p"), true);
    seed(&engine, "insight two", Some("project:p/sub"), true); // subtree
    seed(&engine, "ordinary", Some("project:p"), false); // unmarked → excluded
    seed(
        &engine,
        "other project insight",
        Some("project:other"),
        true,
    ); // out of subtree → excluded

    let body = extract_json(
        &call(
            &engine,
            "memory_get_recent_insights",
            args(&[("project_path", json!("project:p"))]),
        )
        .unwrap(),
    );
    assert_eq!(body["count"], json!(2), "only the two in-subtree insights");

    // limit truncates.
    let limited = extract_json(
        &call(
            &engine,
            "memory_get_recent_insights",
            args(&[("project_path", json!("project:p")), ("limit", json!(1))]),
        )
        .unwrap(),
    );
    assert_eq!(limited["count"], json!(1));

    // unknown project → empty, not an error.
    let empty = extract_json(
        &call(
            &engine,
            "memory_get_recent_insights",
            args(&[("project_path", json!("project:nope"))]),
        )
        .unwrap(),
    );
    assert_eq!(empty["count"], json!(0));
}
