//! Phase 5a outcome-tracking handlers: `record_outcome`, `outcome_counts`.

use memory_engine::engine::MemoryEngine;
use rmcp::model::{CallToolResult, ErrorData};
use serde_json::{Value, json};

use crate::error::to_mcp_error;
use crate::tools::parse::{get_i64, get_str, ok_json, parse_outcome};

pub async fn handle_record_outcome(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let outcome_str = get_str(args, "outcome")
        .ok_or_else(|| ErrorData::invalid_params("missing outcome", None))?;
    let outcome = parse_outcome(&outcome_str)?;

    let event_id = engine
        .record_outcome(fact_id, outcome)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "event_id": event_id,
        "fact_id": fact_id,
        "outcome": outcome,
    }))
}

pub async fn handle_outcome_counts(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    let counts = engine
        .get_outcome_counts(fact_id)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "fact_id": fact_id,
        "positive": counts.positive,
        "negative": counts.negative,
        "neutral": counts.neutral,
    }))
}
