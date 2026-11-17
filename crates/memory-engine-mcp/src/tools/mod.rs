//! The model's tools (ADR-0020 §8): `recall` and `remember`.
//!
//! #1022 and #1023 add `set_level` and `forget`. Each tool is one module holding its
//! definition, its closed argument struct and its handler; this module routes a name to it,
//! mints the call ID that heads every answer and appends the call's record to the event log.

use std::sync::Arc;

use memory_engine::engine::MemoryEngine;
use rmcp::model::{CallToolResult, Content, ErrorData, Tool};
use serde_json::{Map, Value};

use crate::auth::Machine;
use crate::embedding::HttpEmbeddingProvider;
use crate::home::User;

mod recall;
mod remember;

/// The tools the server lists, in the order it lists them.
#[must_use]
pub fn definitions() -> Vec<Tool> {
    vec![recall::definition(), remember::definition()]
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
    let bucket = crate::home::Home::Bucket(ctx.machine.clone()).path(ctx.user);
    // Resolve first: creating the path is a write transaction, needed once per machine.
    let scope_id = match ctx.engine.resolve_scope_path(&bucket) {
        Some(id) => id,
        None => ctx
            .engine
            .ensure_scope_path(&bucket)
            .await
            .map_err(refused)?,
    };
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
    match record(ctx, call, tool, detail).await {
        Ok(_) => Err(message),
        Err(e) => Err(format!("{message} ({e})")),
    }
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
