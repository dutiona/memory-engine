//! The model's tools (ADR-0020 §8): `recall`, `remember`, `forget` and `set_level`.
//!
//! Each tool is one module holding its
//! definition, its closed argument struct and its handler; this module routes a name to it,
//! mints the call ID that heads every answer and appends the call's record to the event log.

use std::sync::Arc;

use memory_engine::engine::MemoryEngine;
use memory_engine::types::{EventType, NewEvent};
use rmcp::model::{CallToolResult, Content, ErrorData, Tool};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::auth::Machine;
use crate::embedding::HttpEmbeddingProvider;
use crate::home::{Home, User};
use crate::project::{ProjectFacts, ProjectKey};

mod forget;
mod placement;
mod recall;
mod remember;
mod set_level;

/// The tools the server lists, in the order it lists them.
#[must_use]
pub fn definitions() -> Vec<Tool> {
    vec![
        recall::definition(),
        remember::definition(),
        forget::definition(),
        set_level::definition(),
    ]
}

/// What one model-tool call runs against.
pub(crate) struct CallContext<'a> {
    pub(crate) engine: &'a MemoryEngine,
    pub(crate) embedder: Option<&'a Arc<HttpEmbeddingProvider>>,
    pub(crate) user: &'a User,
    pub(crate) machine: &'a Machine,
    pub(crate) facts: &'a ProjectFacts,
    /// The session's project key, derived once from `facts` and `machine`.
    pub(crate) key: Option<ProjectKey>,
}

/// The level a write asks for (ADR-0020 §8): the session's project (or its machine's bucket
/// outside a project), or the user level.
#[derive(Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    #[default]
    Project,
    User,
}

impl Level {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

impl CallContext<'_> {
    /// The Home a write at `level` targets.
    pub(crate) fn home_at(&self, level: Level) -> Home {
        match level {
            Level::Project => self.own_home(),
            Level::User => Home::User,
        }
    }

    /// The session's own Home: its project, else its machine's bucket.
    pub(crate) fn own_home(&self) -> Home {
        Home::own(self.key.clone(), self.machine)
    }

    pub(crate) fn bucket(&self) -> Home {
        Home::Bucket(self.machine.clone())
    }
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
    if !matches!(name, "recall" | "remember" | "forget" | "set_level") {
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
    let outcome = match name {
        "recall" => recall::run(ctx, &call, args).await,
        "remember" => remember::run(ctx, &call, args).await,
        "forget" => forget::run(ctx, &call, args).await,
        _ => set_level::run(ctx, &call, args).await,
    };
    Some(Ok(respond(&call, outcome)))
}

/// The answer to a by-ID call on a memory the session may not act on: the same as for a
/// missing id, so it reveals nothing (ADR-0020 §8).
pub(crate) const NOT_FOUND: &str = "not found";

/// ADR-0021 §5's by-ID authority for `set_level` (`forget` checks the same Homes inside its
/// write): the fact and the Home that
/// holds it when that Home is the session's own or the user level; `None` for anything else
/// (root, another project, another bucket, a fact `recall` reached through `project`, a
/// missing id). Liveness is the caller's.
pub(crate) async fn owned_fact(
    ctx: &CallContext<'_>,
    id: i64,
) -> Result<Option<(memory_engine::types::Fact, Home)>, String> {
    let fact = match ctx.engine.get_fact(id).await {
        Ok(fact) => fact,
        Err(memory_engine::MemoryError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    Ok([ctx.own_home(), Home::User]
        .into_iter()
        .find(|home| ctx.engine.resolve_scope_path(&home.path(ctx.user)) == Some(fact.scope_id))
        .map(|home| (fact, home)))
}

/// The call's record (ADR-0020 §8): its ID, tool and machine, the request's project facts
/// with the key and rules version, and `detail` (`fact_ids`, `error`, `level`, ...).
fn call_record(
    ctx: &CallContext<'_>,
    call: &str,
    tool: &str,
    detail: Map<String, Value>,
) -> NewEvent {
    let mut payload = Map::new();
    payload.insert("call".to_owned(), call.into());
    payload.insert("tool".to_owned(), tool.into());
    payload.insert("machine".to_owned(), ctx.machine.as_str().into());
    payload.insert(
        memory_engine::redact::PROVENANCE_KEY.to_owned(),
        ctx.facts.provenance(ctx.key.as_ref()),
    );
    payload.extend(detail);
    NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: EventType::MemoryOp,
        payload: Value::Object(payload),
        source: "mcp".to_owned(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "mcp".to_owned(),
        sequence_id: 0,
        created_at: None,
    }
}

/// Append the call's record to the event log under `home` (ADR-0021 §12: a call record is an
/// ADR-0001 event, never a table beside the log), creating `home` in the record's own
/// transaction, and return the event's id. Records go under the calling machine's bucket,
/// except a valid `remember`'s, which goes under the Home it writes to: a record is
/// provenance, and filing a read's under a project would make that project known (ADR-0020
/// §8). Re-filing reads the payload, never the record's scope.
async fn record(
    ctx: &CallContext<'_>,
    call: &str,
    tool: &str,
    detail: Map<String, Value>,
    home: &Home,
) -> Result<i64, String> {
    ctx.engine
        .ingest_at(&home.path(ctx.user), &call_record(ctx, call, tool, detail))
        .await
        .map_err(|e| format!("cannot record this call: {e}"))
}

/// Record a call that failed before anything was stored, then answer its failure. The record
/// keeps ADR-0020 §6's excerpt of the message, without the userinfo of any URL it quotes from
/// a tool argument; a message that does not settle redaction records the refusal's own text.
async fn refuse(
    ctx: &CallContext<'_>,
    call: &str,
    tool: &str,
    message: String,
) -> Result<String, String> {
    let mut detail = Map::new();
    let recorded = memory_engine::redact::excerpt(&crate::project::redact_userinfo(&message))
        .unwrap_or_else(|e| e.to_string());
    detail.insert("error".to_owned(), recorded.into());
    match record(ctx, call, tool, detail, &ctx.bucket()).await {
        Ok(_) => Err(message),
        Err(e) => Err(format!("{message} ({e})")),
    }
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
