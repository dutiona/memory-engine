//! `remember(content, level?)` (ADR-0020 §8): store one Semantic memory in the session's
//! project (its machine's bucket outside a project), or at the user level when asked.

use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use rmcp::model::Tool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CallContext, Level, placement};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    content: String,
    #[serde(default)]
    level: Level,
}

pub fn definition() -> Tool {
    super::tool(
        "remember",
        "Store one memory for later sessions: in this session's project (outside a project, this \
         machine's), or at the user level when `level` is `user`. Answers with its id, where it was stored \
         and who sees it.",
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "minLength": 1, "description": "The memory, as a self-contained statement." },
                "level": {
                    "type": "string",
                    "enum": ["project", "user"],
                    "description": "project (the default): this session's project; user: true in every project."
                }
            },
            "required": ["content"],
            "additionalProperties": false
        }),
    )
}

pub async fn run(ctx: &CallContext<'_>, call: &str, args: Value) -> Result<String, String> {
    let (content, level) = match validate(args) {
        Ok(valid) => valid,
        Err(message) => return super::refuse(ctx, call, "remember", message).await,
    };
    let target = ctx.home_at(level);
    let path = target.path(ctx.user);
    // The record comes first and the fact names it, so no fact exists without its record. It
    // is filed under the target, whose node it creates in its own transaction, so a project
    // exists only once an event was filed there; a fact write that fails after it leaves a
    // record no fact names: a failed remember in a project that is known.
    let mut detail = serde_json::Map::new();
    detail.insert("level".to_owned(), level.as_str().into());
    let record = super::record(ctx, call, "remember", detail, &target).await?;
    let request = AddFactRequest {
        content,
        fact_type: FactType::Semantic,
        source_event_id: Some(record),
        scope: Some(path),
        opts: Some(AddFactOptions {
            metadata: Some(json!({ "machine": ctx.machine.as_str() })),
            ..Default::default()
        }),
    };
    let id = ctx
        .engine
        .add_fact_deferred(&request)
        .await
        .map_err(|e| e.to_string())?;
    let receipt = placement::receipt(id, &target);
    Ok(
        match placement::hint(&request.content, id, &target, &ctx.own_home()) {
            Some(hint) => format!("{receipt}\n{hint}"),
            None => receipt,
        },
    )
}

fn validate(args: Value) -> Result<(String, Level), String> {
    let Args { content, level } = serde_json::from_value(args).map_err(|e| e.to_string())?;
    if content.trim().is_empty() {
        return Err("content must be a non-empty string".to_owned());
    }
    Ok((content, level))
}
