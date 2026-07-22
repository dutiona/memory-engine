//! Phase 5a cognitive-pipeline (dream cycle) handlers (#225): `dream_cycle`,
//! `apply_cycle_report`, `get_recent_insights`.

use memory_engine::engine::MemoryEngine;
use memory_engine::{CycleOutcome, CycleReport, DefaultDreamCycle};
use rmcp::model::{CallToolResult, ErrorData};
use serde_json::{Value, json};

use crate::depth;
use crate::error::to_mcp_error;
use crate::tools::parse::{get_depth, get_str, ok_json, ok_serialized};

/// Run the dream-cycle pipeline, optionally applying the report (default: apply).
pub async fn handle_dream_cycle(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    // `apply` mutates the store, so a present-but-malformed value must NOT silently
    // fall back to `true` — reject it. Absent/null → default true.
    let apply = match args.get("apply") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(v) => {
            return Err(ErrorData::invalid_params(
                format!("'apply' must be a boolean, got {v}"),
                None,
            ));
        }
    };
    let cycle = DefaultDreamCycle::with_defaults();
    // #209: the guarded entry defers when the caller wrote facts since the cursor.
    match engine
        .run_dream_cycle_guarded(&cycle)
        .await
        .map_err(to_mcp_error)?
    {
        // Skip is NOT a report — emit `did_run: false` + the reason. No apply, no
        // watermark touch. (`skipped` serializes as `{"CallerWroteFacts":{…}}`.)
        CycleOutcome::Skipped(reason) => {
            let reason_json = serde_json::to_value(reason).map_err(|e| {
                ErrorData::internal_error(format!("serialize skip reason: {e}"), None)
            })?;
            ok_json(json!({ "did_run": false, "skipped": reason_json }))
        }
        // Ran: destructure the INNER report (never serialize the CycleOutcome enum —
        // that would emit `{"Ran":{…}}` and break the top-level `report` key).
        CycleOutcome::Ran(report) => {
            let report_json = serde_json::to_value(&report)
                .map_err(|e| ErrorData::internal_error(format!("serialize report: {e}"), None))?;
            if apply {
                let applied = engine
                    .apply_cycle_report(&report)
                    .await
                    .map_err(to_mcp_error)?;
                let applied_json = serde_json::to_value(&applied).map_err(|e| {
                    ErrorData::internal_error(format!("serialize apply result: {e}"), None)
                })?;
                ok_json(json!({
                    "did_run": true, "report": report_json,
                    "applied": applied_json, "did_apply": true
                }))
            } else {
                ok_json(json!({ "did_run": true, "report": report_json, "did_apply": false }))
            }
        }
    }
}

/// Apply a client-supplied `CycleReport` (the gated path).
pub async fn handle_apply_cycle_report(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let report_val = args
        .get("report")
        .ok_or_else(|| ErrorData::invalid_params("missing 'report'", None))?;
    let report: CycleReport = serde_json::from_value(report_val.clone())
        .map_err(|e| ErrorData::invalid_params(format!("invalid CycleReport: {e}"), None))?;
    let applied = engine
        .apply_cycle_report(&report)
        .await
        .map_err(to_mcp_error)?;
    ok_serialized(&applied)
}

/// Return recent insight facts in a project scope subtree, newest-first.
pub async fn handle_get_recent_insights(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let project_path = get_str(args, "project_path")?
        .ok_or_else(|| ErrorData::invalid_params("missing 'project_path'", None))?;
    // The schema declares `limit` as an integer `minimum: 1`; the MCP layer does not
    // validate args against the schema, so enforce it here. A present-but-malformed
    // value (string, negative, 0) is rejected rather than silently defaulting to 20 or
    // returning an empty list indistinguishable from "no insights exist". Absent → 20.
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 20,
        Some(v) => match v.as_u64().and_then(|n| usize::try_from(n).ok()) {
            Some(n) if n >= 1 => n,
            _ => {
                return Err(ErrorData::invalid_params(
                    format!("'limit' must be an integer >= 1, got {v}"),
                    None,
                ));
            }
        },
    };
    let depth_level = get_depth(args)?;

    let facts = engine
        .list_recent_insights(&project_path, limit)
        .await
        .map_err(to_mcp_error)?;
    let shaped: Vec<Value> = facts
        .iter()
        .map(|f| depth::shape_fact(f, depth_level, None))
        .collect();
    ok_json(json!({ "insights": shaped, "count": shaped.len() }))
}
