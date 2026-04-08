use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactRequest, FactType};
use memory_engine_mcp::tools;
use rmcp::model::{CallToolResult, RawContent};
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct FakeEmbed;
impl EmbeddingProvider for FakeEmbed {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3])
    }
}

fn test_engine() -> (MemoryEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig::new(dir.path().join("test.db"), 3);
    let engine = MemoryEngine::open(&config).unwrap();
    (engine, dir)
}

fn add_test_fact(engine: &MemoryEngine, content: &str) -> i64 {
    engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap()
}

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// Extract the JSON value from a successful tool result.
fn extract_json(result: &CallToolResult) -> Value {
    let content = &result.content[0];
    match &content.raw {
        RawContent::Text(t) => serde_json::from_str(&t.text).unwrap(),
        _ => panic!("expected text content"),
    }
}

// ---------------------------------------------------------------------------
// Pin / Unpin
// ---------------------------------------------------------------------------

#[test]
fn test_pin_unpin_roundtrip() {
    let (engine, _dir) = test_engine();
    let fact_id = add_test_fact(&engine, "important fact");

    // Pin
    let result = tools::dispatch(
        "memory_pin_fact",
        args(&[("fact_id", json!(fact_id))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();
    let v = extract_json(&result);
    assert_eq!(v["fact_id"], fact_id);
    assert_eq!(v["pinned"], true);

    // Verify via get_fact
    let fact = engine.get_fact(fact_id).unwrap();
    assert!(fact.is_pinned);

    // Unpin
    let result = tools::dispatch(
        "memory_unpin_fact",
        args(&[("fact_id", json!(fact_id))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();
    let v = extract_json(&result);
    assert_eq!(v["pinned"], false);

    let fact = engine.get_fact(fact_id).unwrap();
    assert!(!fact.is_pinned);
}

#[test]
fn test_pin_missing_fact() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch(
        "memory_pin_fact",
        args(&[("fact_id", json!(9999))]),
        &engine,
        None,
        None,
        3,
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Forget
// ---------------------------------------------------------------------------

#[test]
fn test_forget_defaults() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "old fact");

    let result = tools::dispatch("memory_forget", Map::new(), &engine, None, None, 3).unwrap();
    let v = extract_json(&result);
    assert!(v["facts_evaluated"].is_number());
    assert!(v["facts_expired"].is_number());
}

#[test]
fn test_forget_validation_error() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch(
        "memory_forget",
        args(&[("half_life_days", json!(-1.0))]),
        &engine,
        None,
        None,
        3,
    );
    assert!(result.is_err());
}

#[test]
fn test_forget_with_overrides() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "episodic event");

    let result = tools::dispatch(
        "memory_forget",
        args(&[("half_life_overrides", json!({"Episodic": 30.0}))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();
    let v = extract_json(&result);
    assert!(v["facts_evaluated"].is_number());
}

// ---------------------------------------------------------------------------
// Consolidate
// ---------------------------------------------------------------------------

#[test]
fn test_consolidate_no_provider() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch("memory_consolidate", Map::new(), &engine, None, None, 3);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("not configured"));
}

#[test]
fn test_consolidate_validation() {
    let (engine, _dir) = test_engine();

    // dedup_threshold out of range
    let result = tools::dispatch(
        "memory_consolidate",
        args(&[("dedup_threshold", json!(2.0))]),
        &engine,
        None,
        None,
        3,
    );
    assert!(result.is_err());

    // min_cluster_size too small
    let result = tools::dispatch(
        "memory_consolidate",
        args(&[("min_cluster_size", json!(1))]),
        &engine,
        None,
        None,
        3,
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Dump state
// ---------------------------------------------------------------------------

#[test]
fn test_dump_state_json() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "fact for dump");

    let result = tools::dispatch(
        "memory_dump_state",
        args(&[("format", json!("json"))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();
    let v = extract_json(&result);
    let path = v["path"].as_str().unwrap();
    assert!(path.ends_with(".json"));
    assert!(std::path::Path::new(path).exists());

    std::fs::remove_file(path).ok();
}

#[test]
fn test_dump_state_custom_path() {
    let (engine, dir) = test_engine();
    add_test_fact(&engine, "fact for custom dump");

    let custom_path = dir.path().join("my-dump.json");

    let result = tools::dispatch(
        "memory_dump_state",
        args(&[
            ("format", json!("json")),
            ("path", json!(custom_path.display().to_string())),
        ]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();
    let v = extract_json(&result);
    assert_eq!(
        v["path"].as_str().unwrap(),
        custom_path.display().to_string()
    );
    assert!(custom_path.exists());
}

#[test]
fn test_dump_state_default_path() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "fact for default dump");

    // No format or path → defaults to json + temp dir
    let result = tools::dispatch("memory_dump_state", Map::new(), &engine, None, None, 3).unwrap();
    let v = extract_json(&result);
    let path = v["path"].as_str().unwrap();
    assert!(path.contains("memory-dump-"));
    assert!(path.ends_with(".json"));
    assert!(std::path::Path::new(path).exists());

    std::fs::remove_file(path).ok();
}

// ---------------------------------------------------------------------------
// Outcome tracking (Phase 5a, #63)
// ---------------------------------------------------------------------------

#[test]
fn test_record_outcome_and_counts() {
    let (engine, _dir) = test_engine();
    let fact_id = add_test_fact(&engine, "outcome test fact");

    // Record outcomes via MCP dispatch
    let r1 = tools::dispatch(
        "memory_record_outcome",
        args(&[("fact_id", json!(fact_id)), ("outcome", json!("Positive"))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();
    let v1 = extract_json(&r1);
    assert_eq!(v1["fact_id"], fact_id);
    assert_eq!(v1["outcome"], "Positive");
    assert!(v1["event_id"].as_i64().unwrap() > 0);

    let _ = tools::dispatch(
        "memory_record_outcome",
        args(&[("fact_id", json!(fact_id)), ("outcome", json!("Negative"))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();

    let _ = tools::dispatch(
        "memory_record_outcome",
        args(&[("fact_id", json!(fact_id)), ("outcome", json!("Positive"))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();

    // Query counts
    let r2 = tools::dispatch(
        "memory_outcome_counts",
        args(&[("fact_id", json!(fact_id))]),
        &engine,
        None,
        None,
        3,
    )
    .unwrap();
    let v2 = extract_json(&r2);
    assert_eq!(v2["fact_id"], fact_id);
    assert_eq!(v2["positive"], 2);
    assert_eq!(v2["negative"], 1);
    assert_eq!(v2["neutral"], 0);
}

#[test]
fn test_record_outcome_nonexistent_fact() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch(
        "memory_record_outcome",
        args(&[("fact_id", json!(999)), ("outcome", json!("Positive"))]),
        &engine,
        None,
        None,
        3,
    );
    assert!(result.is_err());
}

#[test]
fn test_record_outcome_invalid_variant() {
    let (engine, _dir) = test_engine();
    let fact_id = add_test_fact(&engine, "variant test");

    let result = tools::dispatch(
        "memory_record_outcome",
        args(&[
            ("fact_id", json!(fact_id)),
            ("outcome", json!("InvalidVariant")),
        ]),
        &engine,
        None,
        None,
        3,
    );
    assert!(result.is_err());
}
