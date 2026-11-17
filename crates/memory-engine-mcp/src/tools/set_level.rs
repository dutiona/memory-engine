//! `set_level(id, level)` (ADR-0020 §8, ADR-0021 §7): move one of the session's memories
//! between its project (its machine's bucket outside a project) and the user level.

use chrono::{DateTime, Utc};
use memory_engine::MemoryError;
use memory_engine::types::Fact;
use rmcp::model::Tool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CallContext, Level, NOT_FOUND, placement};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    id: i64,
    level: Level,
}

pub fn definition() -> Tool {
    super::tool(
        "set_level",
        "Move one of this session's memories, by its id, between this session's project (outside a \
         project, this machine's) and the user level. Changes nothing else. Answers where it is now.",
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer", "description": "The memory's id, as remember or recall gave it." },
                "level": { "type": "string", "enum": ["project", "user"] }
            },
            "required": ["id", "level"],
            "additionalProperties": false
        }),
    )
}

/// A fact `recall` could return now: not expired, and valid now or until later.
fn live(fact: &Fact, now: DateTime<Utc>) -> bool {
    fact.t_expired.is_none() && fact.t_invalid.is_none_or(|end| end > now)
}

pub async fn run(ctx: &CallContext<'_>, call: &str, args: Value) -> Result<String, String> {
    let refuse = |message: String| super::refuse(ctx, call, "set_level", message);
    let Args { id, level } = match serde_json::from_value(args) {
        Ok(args) => args,
        Err(e) => return refuse(e.to_string()).await,
    };
    // A memory of another Home, a forgotten one and a missing id all answer the same.
    let (fact, held) = match super::owned_fact(ctx, id).await {
        Ok(Some((fact, held))) if live(&fact, Utc::now()) => (fact, held),
        Ok(_) => return refuse(NOT_FOUND.to_owned()).await,
        Err(message) => return refuse(message).await,
    };
    let target = ctx.home_at(level);
    let to = target.path(ctx.user);
    let mut detail = serde_json::Map::new();
    detail.insert("fact_ids".to_owned(), json!([id]));
    detail.insert("level".to_owned(), level.as_str().into());
    detail.insert("from".to_owned(), held.path(ctx.user).into());
    detail.insert("to".to_owned(), to.clone().into());
    let record = super::call_record(ctx, call, "set_level", detail);
    let bucket = ctx.bucket().path(ctx.user);
    match ctx
        .engine
        .move_fact(id, fact.scope_id, &to, &bucket, &record)
        .await
    {
        Ok(_) if held == target => Ok(placement::unchanged(id, &target)),
        Ok(_) => Ok(placement::receipt(id, &target)),
        Err(MemoryError::NotFound(_)) => {
            refuse(format!(
                "memory {id} changed while it was being moved; nothing was changed"
            ))
            .await
        }
        Err(e) => refuse(format!("cannot record this move: {e}")).await,
    }
}
