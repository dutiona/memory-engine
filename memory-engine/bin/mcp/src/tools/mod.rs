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

pub use definitions::all_tool_definitions;

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
