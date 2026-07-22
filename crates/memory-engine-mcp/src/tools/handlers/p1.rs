//! P1 tool handlers: `consolidate`, `forget`, `dump_state`, `pin_fact`,
//! `unpin_fact`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use memory_engine::ForgetPolicy;
use memory_engine::engine::MemoryEngine;
use memory_engine::inspect_types::DumpFormat;
use memory_engine::traits::{EmbeddingProvider, SummaryGenerator};
use rmcp::model::{CallToolResult, ErrorData};
use serde_json::{Value, json};

use crate::embedding::HttpEmbeddingProvider;
use crate::error::{ValidationError, to_mcp_error};
use crate::tools::parse::{
    default_dump_path, get_i64, get_str, ok_json, parse_consolidate_config, parse_fact_type,
    require_f64_if_present, validate_dump_path,
};

pub async fn handle_consolidate(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
    summary_gen: Option<&Arc<dyn SummaryGenerator + Send + Sync>>,
) -> Result<CallToolResult, ErrorData> {
    let generator = summary_gen.ok_or(ValidationError::NoSummaryProvider)?;
    // Issue #116: summaries are embedded via the EmbeddingProvider, not the
    // SummaryGenerator, so consolidation now requires an embedder too.
    let embedder = embedder.ok_or(ValidationError::NoEmbeddingProvider)?;

    let config = parse_consolidate_config(args)?;

    // The async engine takes both consumer traits as owned `Arc<dyn _>` (#631 §1.2) so it
    // can clone them into the lock-free `spawn_blocking` summarize/embed offload.
    let generator = Arc::clone(generator) as Arc<dyn SummaryGenerator>;
    let embedder = Arc::clone(embedder) as Arc<dyn EmbeddingProvider>;
    let stats = engine
        .consolidate(generator, embedder, &config)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "duplicates_removed": stats.duplicates_removed,
        "clusters_created": stats.clusters_created,
        "global_summaries": stats.global_summaries,
    }))
}

pub async fn handle_forget(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let mut policy = ForgetPolicy::default();

    if let Some(v) = require_f64_if_present(args, "half_life_days")? {
        policy.half_life_days = v;
    }
    if let Some(v) = require_f64_if_present(args, "min_importance")? {
        policy.min_importance = v;
    }
    if let Some(v) = require_f64_if_present(args, "recency_weight")? {
        policy.recency_weight = v;
    }
    if let Some(v) = require_f64_if_present(args, "frequency_weight")? {
        policy.frequency_weight = v;
    }
    if let Some(v) = require_f64_if_present(args, "graph_degree_weight")? {
        policy.graph_degree_weight = v;
    }
    if let Some(v) = require_f64_if_present(args, "base_importance_weight")? {
        policy.base_importance_weight = v;
    }

    // Parse per-FactType half-life overrides: {"Episodic": 30.0, "Procedural": 365.0}
    if let Some(val) = args.get("half_life_overrides") {
        match val {
            Value::Object(overrides) => {
                let mut map = HashMap::new();
                for (key, v) in overrides {
                    let ft = parse_fact_type(key)?;
                    let hl = v.as_f64().ok_or_else(|| {
                        ValidationError::Other(format!(
                            "half_life_overrides[\"{key}\"] must be a number"
                        ))
                    })?;
                    map.insert(ft, hl);
                }
                policy.half_life_overrides = map;
            }
            Value::Null => {} // explicitly null — use default
            _ => {
                return Err(ValidationError::Other(
                    "half_life_overrides must be a JSON object".to_owned(),
                )
                .into());
            }
        }
    }

    policy.validate().map_err(to_mcp_error)?;

    let stats = engine.forget(&policy).await.map_err(to_mcp_error)?;

    ok_json(json!({
        "facts_expired": stats.facts_expired,
        "facts_evaluated": stats.facts_evaluated,
    }))
}

pub async fn handle_dump_state(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let format_str = get_str(args, "format")?.unwrap_or_else(|| "json".to_owned());

    let ext = match format_str.as_str() {
        "json" => "json",
        "sqlite" => "db",
        other => {
            return Err(ValidationError::Other(format!("unsupported dump format: {other}")).into());
        }
    };

    let path = match get_str(args, "path")? {
        Some(p) => validate_dump_path(&PathBuf::from(p))?,
        None => default_dump_path(&std::env::temp_dir(), ext),
    };

    let dump_format = match format_str.as_str() {
        "json" => DumpFormat::Json(path.clone()),
        "sqlite" => DumpFormat::Sqlite(path.clone()),
        _ => unreachable!(), // validated above
    };

    engine
        .dump_state(&dump_format)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({ "path": path.display().to_string() }))
}

pub async fn handle_pin_fact(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")?
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    engine.pin_fact(fact_id).await.map_err(to_mcp_error)?;

    ok_json(json!({ "fact_id": fact_id, "pinned": true }))
}

pub async fn handle_unpin_fact(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")?
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    engine.unpin_fact(fact_id).await.map_err(to_mcp_error)?;

    ok_json(json!({ "fact_id": fact_id, "pinned": false }))
}
