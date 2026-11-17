//! `forget(id, reason)` (ADR-0020 §8, #1023): retract one of the session's memories so it
//! does not come up again. It never deletes: the row and every event stay, and only the
//! maintenance CLI (`unforget`, `erase`) reverses or deletes.

use memory_engine::types::ForgetOutcome;
use rmcp::model::Tool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CallContext, NOT_FOUND};
use crate::home::Home;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    id: i64,
    reason: String,
}

pub fn definition() -> Tool {
    super::tool(
        "forget",
        "Retract one of this session's memories so it does not come up again: it and any identical \
         copy leave recall, and later imports and promotions keep it out. Acts only on this session's \
         memories and the user level's. Answers forgotten, already forgotten or not found.",
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer", "description": "The memory's id, as remember or recall gave it." },
                "reason": { "type": "string", "minLength": 1, "description": "Why it should not come up again." }
            },
            "required": ["id", "reason"],
            "additionalProperties": false
        }),
    )
}

pub async fn run(ctx: &CallContext<'_>, call: &str, args: Value) -> Result<String, String> {
    let refuse = |message: String| super::refuse(ctx, call, "forget", message);
    let Args { id, reason } = match serde_json::from_value(args) {
        Ok(args) => args,
        Err(e) => return refuse(e.to_string()).await,
    };
    if reason.trim().is_empty() {
        return refuse("reason must be a non-empty string".to_owned()).await;
    }
    // ADR-0021 §5's by-ID authority: the session's own Home and the user level, never the
    // `project` swap. The port checks the fact's scope inside its write.
    let homes: Vec<String> = [ctx.own_home(), Home::User]
        .iter()
        .map(|home| home.path(ctx.user))
        .collect();
    let mut detail = serde_json::Map::new();
    detail.insert("fact_ids".to_owned(), json!([id]));
    let record = super::call_record(ctx, call, "forget", detail);
    let bucket = ctx.bucket().path(ctx.user);
    match ctx
        .engine
        .forget_fact(id, &homes, super::capture_cut(&reason), &bucket, &record)
        .await
    {
        Ok((ForgetOutcome::Forgotten, _)) => Ok("forgotten".to_owned()),
        Ok((ForgetOutcome::AlreadyForgotten, _)) => Ok("already forgotten".to_owned()),
        Ok((ForgetOutcome::NotFound, _)) => Ok(NOT_FOUND.to_owned()),
        Err(e) => refuse(format!("cannot forget memory {id}: {e}")).await,
    }
}
