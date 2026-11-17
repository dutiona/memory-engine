//! MCP tool surface: schema definitions, argument parsing, handlers, and the
//! string-keyed dispatcher.
//!
//! This module is the dispatcher seam. The four concerns are split into
//! submodules:
//!
//! - [`definitions`] — the JSON tool schemas ([`all_tool_definitions`] + `tool_def`).
//! - [`parse`] — argument parsing / validation helpers and result shaping
//!   (`pub(crate)`; used by the handlers and `parse_consolidate_config`).
//! - [`handlers`] — the per-tool `handle_*` functions, grouped by phase.
//!
//! The public surface is unchanged: [`all_tool_definitions`], [`dispatch`],
//! [`MAX_FLUSH_INSIGHTS`], and [`MAX_BOOTSTRAP_BYTES`] resolve under
//! `memory_engine_mcp::tools::*` exactly as before.

use std::sync::Arc;

use memory_engine::engine::MemoryEngine;
use memory_engine::traits::SummaryGenerator;
use rmcp::model::{CallToolResult, ErrorData};
use serde_json::{Map, Value};

use crate::embedding::HttpEmbeddingProvider;

// Submodules are PRIVATE so the parsing/handler helpers keep the exact
// encapsulation they had as `fn`s in the old god-module: reachable by the
// dispatcher + handler submodules (descendants of `tools`), but NOT by the
// rest of the crate (`server.rs`/`main.rs`). Only `all_tool_definitions`,
// `dispatch`, and the two caps are re-exported as the public surface.
mod definitions;
mod handlers;
mod parse;
mod recall;
mod remember;

use rmcp::model::{Content, Tool};

use crate::auth::Machine;
use crate::home::User;

/// Every tool the server lists: the model tools first.
#[must_use]
pub fn all_tool_definitions() -> Vec<Tool> {
    let mut tools = vec![recall::definition(), remember::definition()];
    tools.extend(definitions::all_tool_definitions());
    tools
}

/// What one model-tool call runs against.
pub(crate) struct CallContext<'a> {
    pub(crate) engine: &'a MemoryEngine,
    pub(crate) embedder: Option<&'a Arc<HttpEmbeddingProvider>>,
    pub(crate) user: &'a User,
    pub(crate) machine: &'a Machine,
}

/// A fresh call ID: 16 lowercase hex digits from the OS random number generator
/// (ADR-0020 §8).
///
/// # Errors
///
/// The OS random number generator failed.
pub fn new_call_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{:016x}", u64::from_be_bytes(bytes)))
}

/// Run `name` when it is a model tool, else `None`. Every answer, failures included, is a
/// tool result whose text starts with the call line (ADR-0020 §8).
pub(crate) async fn call_model_tool(
    name: &str,
    args: Map<String, Value>,
    ctx: &CallContext<'_>,
) -> Option<Result<CallToolResult, ErrorData>> {
    if !matches!(name, "recall" | "remember") {
        return None;
    }
    // Unreachable once main's startup probe succeeded: Linux getrandom(2) never fails after
    // one success. No call ID can head this answer, so it is the one JSON-RPC error.
    let call = match new_call_id() {
        Ok(call) => call,
        Err(e) => {
            return Some(Err(ErrorData::internal_error(
                format!("no call ID: the OS random number generator failed: {e}"),
                None,
            )));
        }
    };
    let args = Value::Object(args);
    let outcome = if name == "recall" {
        recall::run(ctx, &call, args).await
    } else {
        remember::run(ctx, &call, args).await
    };
    Some(Ok(respond(&call, outcome)))
}

/// Append the call's record to the event log (ADR-0021 §12: a call record is an ADR-0001
/// event, never a table beside the log), filed under the session's own Home, and return the
/// event's id. `detail` adds `fact_ids` or `error`. #1022 adds the request's project facts.
async fn record(
    ctx: &CallContext<'_>,
    call: &str,
    tool: &str,
    detail: Map<String, Value>,
) -> Result<i64, String> {
    let refused = |e: memory_engine::MemoryError| format!("cannot record this call: {e}");
    let scope_id = ctx
        .engine
        .ensure_scope_path(&crate::home::bucket(ctx.machine))
        .await
        .map_err(refused)?;
    let mut payload = Map::new();
    payload.insert("call".to_owned(), call.into());
    payload.insert("tool".to_owned(), tool.into());
    payload.insert("machine".to_owned(), ctx.machine.as_str().into());
    payload.extend(detail);
    let event = memory_engine::types::NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: memory_engine::types::EventType::MemoryOp,
        payload: Value::Object(payload),
        source: "mcp".to_owned(),
        session_id: None,
        scope_id,
        origin_node_id: "mcp".to_owned(),
        sequence_id: 0,
        created_at: None,
    };
    ctx.engine.ingest(&event).await.map_err(refused)
}

/// Record a call that failed before anything was stored, then answer its failure.
async fn refuse(
    ctx: &CallContext<'_>,
    call: &str,
    tool: &str,
    message: String,
) -> Result<String, String> {
    let mut detail = Map::new();
    detail.insert("error".to_owned(), capture_cut(&message).into());
    record(ctx, call, tool, detail).await?;
    Err(message)
}

/// ADR-0020 §6's capture cap, 512 bytes back to a character boundary: no client-supplied
/// value can make a record outgrow the event payload limit.
fn capture_cut(text: &str) -> &str {
    let mut end = text.len().min(512);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn respond(call: &str, outcome: Result<String, String>) -> CallToolResult {
    match outcome {
        Ok(body) => CallToolResult::success(vec![Content::text(format!("call {call}\n{body}"))]),
        Err(message) => CallToolResult::error(vec![Content::text(format!(
            "call {call}\nerror: {message}"
        ))]),
    }
}

fn tool(name: &'static str, description: &'static str, schema: Value) -> Tool {
    let Value::Object(schema) = schema else {
        unreachable!("a tool schema is a JSON object")
    };
    Tool::new(name, description, Arc::new(schema))
}

// ---------------------------------------------------------------------------
// Trust-boundary input-size caps (#266/#267/#355/#294)
// ---------------------------------------------------------------------------
//
// The MCP server consumes untrusted JSON-RPC input. Several handlers materialize
// client-controlled collections before validating them, which is a pre-allocation
// DoS surface (CWE-400 resource exhaustion / CWE-770 allocation without limit).
// These caps are deliberately GENEROUS: each sits orders of magnitude above any
// realistic legitimate payload, so they reject only absurd input while never
// breaking large-but-reasonable use. They are also mirrored as `maxItems` /
// `maxLength` in the tool JSON schemas so a schema-aware client is warned up front.

/// Max number of insights accepted by `memory_flush_insights` in a single call.
/// A pre-compaction flush realistically carries at most a few hundred insights.
pub const MAX_FLUSH_INSIGHTS: usize = 10_000;

/// Max raw `jsonl_data` byte length accepted by `memory_bootstrap_session`.
/// 50 MB is far above any realistic Claude Code session log.
pub const MAX_BOOTSTRAP_BYTES: usize = 50 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Route a tool call to the appropriate handler.
///
/// # Errors
///
/// Returns [`ErrorData`] if the tool name is unknown, required arguments are
/// missing or malformed, or the underlying engine operation fails.
#[allow(
    clippy::needless_pass_by_value,
    reason = "args is passed by value from the MCP framework's dispatch boundary; \
              taking &Map would force callers to retain ownership"
)]
pub async fn dispatch(
    name: &str,
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<Arc<HttpEmbeddingProvider>>,
    summary_gen: Option<Arc<dyn SummaryGenerator + Send + Sync>>,
    embed_dim: usize,
    filter_config: &memory_engine::ActivityFilterConfig,
) -> Result<CallToolResult, ErrorData> {
    use handlers::{cognitive, outcome, p0, p1, p2, session};

    let args = &args;
    let embedder = embedder.as_ref();
    match name {
        "memory_ingest" => p0::handle_ingest(args, engine).await,
        "memory_add_fact" => p0::handle_add_fact(args, engine, embedder, embed_dim).await,
        "memory_query" => p0::handle_query(args, engine, embedder, embed_dim).await,
        "memory_resume_context" => p0::handle_resume_context(args, engine).await,
        "memory_list_due" => p0::handle_list_due(args, engine).await,
        "memory_next_due_time" => p0::handle_next_due_time(args, engine).await,
        "memory_explain_fact" => p0::handle_explain_fact(args, engine).await,
        "memory_get_fact" => p0::handle_get_fact(args, engine).await,
        "memory_statistics" => p0::handle_statistics(engine).await,
        "memory_flush_insights" => p0::handle_flush_insights(args, engine, embedder).await,
        // P1 tools
        "memory_consolidate" => {
            p1::handle_consolidate(args, engine, embedder, summary_gen.as_ref()).await
        }
        "memory_forget" => p1::handle_forget(args, engine).await,
        "memory_dump_state" => p1::handle_dump_state(args, engine).await,
        "memory_pin_fact" => p1::handle_pin_fact(args, engine).await,
        "memory_unpin_fact" => p1::handle_unpin_fact(args, engine).await,
        // P2 tools
        "memory_replay_events" => p2::handle_replay_events(args, engine).await,
        "memory_fact_history" => p2::handle_fact_history(args, engine).await,
        "memory_bootstrap_session" => p2::handle_bootstrap_session(args, engine, embedder).await,
        // Phase 5a: Outcome tracking
        "memory_record_outcome" => outcome::handle_record_outcome(args, engine).await,
        "memory_outcome_counts" => outcome::handle_outcome_counts(args, engine).await,
        // Activity stream + session lifecycle (#224)
        "memory_record_activity" => {
            session::handle_record_activity(args, engine, embedder, filter_config).await
        }
        "memory_checkpoint_session" => session::handle_checkpoint_session(args, engine).await,
        "memory_load_context" => session::handle_load_context(args, engine).await,
        // Phase 5a: Cognitive pipeline (dream cycle) (#225)
        "memory_dream_cycle" => cognitive::handle_dream_cycle(args, engine).await,
        "memory_apply_cycle_report" => cognitive::handle_apply_cycle_report(args, engine).await,
        "memory_get_recent_insights" => cognitive::handle_get_recent_insights(args, engine).await,
        _ => Err(ErrorData::invalid_params(
            format!("unknown tool: {name}"),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_cut_keeps_whole_characters() {
        assert_eq!(capture_cut("short"), "short");
        let long = "é".repeat(400); // 800 bytes
        let cut = capture_cut(&long);
        assert!(cut.len() <= 512 && cut.len() >= 511, "{}", cut.len());
        assert!(cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_call_line_is_call_and_sixteen_lowercase_hex_digits() {
        let call = new_call_id().unwrap();
        assert_eq!(call.len(), 16);
        assert!(
            call.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{call}"
        );
        assert_ne!(new_call_id().unwrap(), call);
        let result = respond(&call, Err("boom".to_owned()));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0].as_text().unwrap().text.clone();
        assert_eq!(text, format!("call {call}\nerror: boom"));
    }
}
