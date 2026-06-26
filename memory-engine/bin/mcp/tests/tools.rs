// Engine-generated dump filenames are always lowercase ".json" on all supported
// platforms; the case-sensitive ends_with check is intentionally exact.
#![allow(clippy::case_sensitive_file_extension_comparisons)]

use std::sync::Arc;

use memory_engine::MemoryEngine;
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
    fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
        memory_engine::EmbeddingFingerprint::new("mock", "test", 3)
    }
}

fn test_engine() -> (MemoryEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let engine = MemoryEngine::builder(3)
        .path(dir.path().join("test.db"))
        .build()
        .unwrap();
    (engine, dir)
}

async fn add_test_fact(engine: &MemoryEngine, content: &str) -> i64 {
    let emb: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbed);
    engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            emb,
            None,
        )
        .await
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

#[tokio::test]
async fn test_pin_unpin_roundtrip() {
    let (engine, _dir) = test_engine();
    let fact_id = add_test_fact(&engine, "important fact").await;

    // Pin
    let result = tools::dispatch(
        "memory_pin_fact",
        args(&[("fact_id", json!(fact_id))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
    let v = extract_json(&result);
    assert_eq!(v["fact_id"], fact_id);
    assert_eq!(v["pinned"], true);

    // Verify via get_fact
    let fact = engine.get_fact(fact_id).await.unwrap();
    assert!(fact.is_pinned);

    // Unpin
    let result = tools::dispatch(
        "memory_unpin_fact",
        args(&[("fact_id", json!(fact_id))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
    let v = extract_json(&result);
    assert_eq!(v["pinned"], false);

    let fact = engine.get_fact(fact_id).await.unwrap();
    assert!(!fact.is_pinned);
}

#[tokio::test]
async fn test_pin_missing_fact() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch(
        "memory_pin_fact",
        args(&[("fact_id", json!(9999))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Forget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_forget_defaults() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "old fact").await;

    let result = tools::dispatch(
        "memory_forget",
        Map::new(),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
    let v = extract_json(&result);
    assert!(v["facts_evaluated"].is_number());
    assert!(v["facts_expired"].is_number());
}

#[tokio::test]
async fn test_forget_validation_error() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch(
        "memory_forget",
        args(&[("half_life_days", json!(-1.0))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_forget_with_overrides() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "episodic event").await;

    let result = tools::dispatch(
        "memory_forget",
        args(&[("half_life_overrides", json!({"Episodic": 30.0}))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
    let v = extract_json(&result);
    assert!(v["facts_evaluated"].is_number());
}

// ---------------------------------------------------------------------------
// Consolidate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_consolidate_no_provider() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch(
        "memory_consolidate",
        Map::new(),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("not configured"));
}

#[tokio::test]
async fn test_consolidate_validation() {
    let (engine, _dir) = test_engine();

    // dedup_threshold out of range
    let result = tools::dispatch(
        "memory_consolidate",
        args(&[("dedup_threshold", json!(2.0))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());

    // NOTE: cluster_threshold range validation (#344) is unit-tested directly in
    // tools/mod.rs `parse_consolidate_config` tests — a provider-less dispatch here
    // short-circuits on NoSummaryProvider before reaching threshold validation, so
    // it cannot honestly exercise that path.

    // min_cluster_size too small
    let result = tools::dispatch(
        "memory_consolidate",
        args(&[("min_cluster_size", json!(1))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Dump state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_dump_state_json() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "fact for dump").await;

    let result = tools::dispatch(
        "memory_dump_state",
        args(&[("format", json!("json"))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
    let v = extract_json(&result);
    let path = v["path"].as_str().unwrap();
    assert!(path.ends_with(".json"));
    assert!(std::path::Path::new(path).exists());

    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn test_dump_state_custom_path() {
    let (engine, dir) = test_engine();
    add_test_fact(&engine, "fact for custom dump").await;

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
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
    let v = extract_json(&result);
    assert_eq!(
        v["path"].as_str().unwrap(),
        custom_path.display().to_string()
    );
    assert!(custom_path.exists());
}

#[tokio::test]
async fn test_dump_state_default_path() {
    let (engine, _dir) = test_engine();
    add_test_fact(&engine, "fact for default dump").await;

    // No format or path → defaults to json + temp dir
    let result = tools::dispatch(
        "memory_dump_state",
        Map::new(),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
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

#[tokio::test]
async fn test_record_outcome_and_counts() {
    let (engine, _dir) = test_engine();
    let fact_id = add_test_fact(&engine, "outcome test fact").await;

    // Record outcomes via MCP dispatch
    let r1 = tools::dispatch(
        "memory_record_outcome",
        args(&[("fact_id", json!(fact_id)), ("outcome", json!("Positive"))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
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
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();

    let _ = tools::dispatch(
        "memory_record_outcome",
        args(&[("fact_id", json!(fact_id)), ("outcome", json!("Positive"))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();

    // Query counts
    let r2 = tools::dispatch(
        "memory_outcome_counts",
        args(&[("fact_id", json!(fact_id))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .unwrap();
    let v2 = extract_json(&r2);
    assert_eq!(v2["fact_id"], fact_id);
    assert_eq!(v2["positive"], 2);
    assert_eq!(v2["negative"], 1);
    assert_eq!(v2["neutral"], 0);
}

#[tokio::test]
async fn test_record_outcome_nonexistent_fact() {
    let (engine, _dir) = test_engine();

    let result = tools::dispatch(
        "memory_record_outcome",
        args(&[("fact_id", json!(999)), ("outcome", json!("Positive"))]),
        &engine,
        None,
        None,
        3,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_record_outcome_invalid_variant() {
    let (engine, _dir) = test_engine();
    let fact_id = add_test_fact(&engine, "variant test").await;

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
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;
    assert!(result.is_err());
}
