//! Activity stream + session lifecycle handlers (#224): `record_activity`,
//! `checkpoint_session`, `load_context`.

use std::sync::Arc;

use chrono::Utc;
use memory_engine::engine::MemoryEngine;
use memory_engine::traits::EmbeddingProvider;
use rmcp::model::{CallToolResult, ErrorData};
use serde_json::{Value, json};

use crate::depth;
use crate::embedding::HttpEmbeddingProvider;
use crate::error::to_mcp_error;
use crate::tools::parse::{get_datetime, get_depth, get_str, get_usize, ok_json};

pub async fn handle_record_activity(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
    filter_config: &memory_engine::ActivityFilterConfig,
) -> Result<CallToolResult, ErrorData> {
    let tool_name =
        get_str(args, "tool")?.ok_or_else(|| ErrorData::invalid_params("missing tool", None))?;
    let session_id = get_str(args, "session_id")?
        .ok_or_else(|| ErrorData::invalid_params("missing session_id", None))?;
    let tool_args = args.get("args").cloned().unwrap_or_else(|| json!({}));
    let result_summary = get_str(args, "result")?;
    let timestamp = get_datetime(args, "timestamp")?.unwrap_or_else(Utc::now);
    let scope = get_str(args, "scope")?;
    // `OutcomeClass::from_str` is infallible (the open `Other` arm captures any
    // value), so an arbitrary JSON string maps cleanly; `None` defers to the
    // engine's `OutcomeClass::Success` default.
    let outcome_class = get_str(args, "outcome_class")?.map(|s| {
        let Ok(class) = s.parse::<memory_engine::OutcomeClass>();
        class
    });

    let req = memory_engine::RecordActivityRequest {
        tool_name,
        args: tool_args,
        result: result_summary,
        session_id,
        timestamp,
        scope_path: scope,
        outcome_class,
    };

    // The async engine takes an owned `Option<Arc<dyn EmbeddingProvider>>` (#631 §1.2):
    // promotion-to-fact embeds via the provider on the blocking offload, so it must own
    // a clone rather than borrow the handler's reference.
    let provider = embedder.map(|e| Arc::clone(e) as Arc<dyn EmbeddingProvider>);
    let result = engine
        .record_activity(&req, provider, filter_config)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "activity_id": result.activity_id,
        "was_deduplicated": result.was_deduplicated,
        "promoted_fact_id": result.promoted_fact_id,
        "status": result.status.to_string(),
    }))
}

pub async fn handle_checkpoint_session(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let session_id = get_str(args, "session_id")?
        .ok_or_else(|| ErrorData::invalid_params("missing session_id", None))?;
    let scope = get_str(args, "scope")?;
    let summary = get_str(args, "summary")?;
    let metadata = args.get("metadata").cloned();

    engine
        .checkpoint_session(&session_id, scope.as_deref(), summary.as_deref(), metadata)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "session_id": session_id,
        "checkpointed": true,
    }))
}

pub async fn handle_load_context(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let scope =
        get_str(args, "scope")?.ok_or_else(|| ErrorData::invalid_params("missing scope", None))?;
    let activity_limit = get_usize(args, "activity_limit")?.unwrap_or(20);
    let fact_limit = get_usize(args, "fact_limit")?.unwrap_or(10);
    let depth_level = get_depth(args)?;

    let ctx = engine
        .load_context(&scope, activity_limit, fact_limit)
        .await
        .map_err(to_mcp_error)?;

    ok_json(depth::shape_project_context(&ctx, depth_level))
}
