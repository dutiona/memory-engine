//! `remember(content)` (ADR-0020 §8): store one Semantic memory.

use std::sync::Arc;

use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};
use rmcp::model::Tool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::CallContext;
use crate::home;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    content: String,
}

pub fn definition() -> Tool {
    super::tool(
        "remember",
        "Store one memory for later sessions. Answers with its id, where it was stored and who sees it.",
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "minLength": 1, "description": "The memory, as a self-contained statement." }
            },
            "required": ["content"],
            "additionalProperties": false
        }),
    )
}

pub async fn run(ctx: &CallContext<'_>, call: &str, args: Value) -> Result<String, String> {
    let (content, provider) = match validate(ctx, args) {
        Ok(valid) => valid,
        Err(message) => return super::refuse(ctx, call, "remember", message).await,
    };
    // The record comes first and the fact names it, so no fact exists without its record. A
    // fact write that fails after it leaves a record no fact names: a failed remember.
    let record = super::record(ctx, call, "remember", serde_json::Map::new()).await?;
    // Until #1022 derives a project key, every session is keyless (ADR-0021 §6).
    let request = AddFactRequest {
        content,
        fact_type: FactType::Semantic,
        source_event_id: Some(record),
        scope: Some(home::bucket(ctx.machine)),
        opts: Some(AddFactOptions {
            metadata: Some(json!({ "machine": ctx.machine.as_str() })),
            ..Default::default()
        }),
    };
    let provider = Arc::clone(provider) as Arc<dyn EmbeddingProvider>;
    let id = ctx
        .engine
        .add_fact(&request, provider, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "stored {id} in unscoped on {}, visible to all agents",
        ctx.machine.as_str()
    ))
}

fn validate<'a>(
    ctx: &CallContext<'a>,
    args: Value,
) -> Result<(String, &'a Arc<crate::embedding::HttpEmbeddingProvider>), String> {
    let Args { content } = serde_json::from_value(args).map_err(|e| e.to_string())?;
    if content.trim().is_empty() {
        return Err("content must be a non-empty string".to_owned());
    }
    let provider = ctx
        .embedder
        .ok_or("no embedder is configured, so nothing can be remembered")?;
    Ok((content, provider))
}
