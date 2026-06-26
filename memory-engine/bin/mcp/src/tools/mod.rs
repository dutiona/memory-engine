use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use memory_engine::ResumeConfig;
use memory_engine::bootstrap::{BootstrapConfig, KeywordExtractor};
use memory_engine::engine::MemoryEngine;
use memory_engine::inspect_types::{DumpFormat, FactExplanation, ReplayFilter, ReplayOrder};
use memory_engine::search::hybrid::SearchMode;
use memory_engine::traits::{
    ConsolidationConfig, EmbeddingProvider, ForgetPolicy, SummaryGenerator,
};
use memory_engine::types::{
    AddFactOptions, AddFactRequest, EmbeddingFingerprint, EventType, FactType, NewEvent, Outcome,
};
use memory_engine::{CycleOutcome, CycleReport, DefaultDreamCycle, INSIGHT_MARKER_KEY};
use rmcp::model::{CallToolResult, Content, ErrorData, Tool};
use serde_json::{Map, Value, json};

use crate::depth::{self, Depth};
use crate::embedding::HttpEmbeddingProvider;
use crate::error::{ValidationError, to_mcp_error};

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
// Tool definitions (JSON schemas)
// ---------------------------------------------------------------------------

/// Returns all tool definitions (P0, P1, P2, Phase 5) with JSON schemas.
///
/// The returned list is what the server advertises via the MCP `list_tools`
/// request; each [`Tool`] carries its name, description, and input JSON schema.
///
/// # Examples
///
/// ```
/// use memory_engine_mcp::tools;
///
/// let defs = tools::all_tool_definitions();
/// assert!(!defs.is_empty());
/// // Every tool is namespaced under the `memory_` prefix.
/// assert!(defs.iter().all(|t| t.name.starts_with("memory_")));
/// // The exact count and name-uniqueness are pinned in the integration tests,
/// // not here — a doc comment should illustrate the contract, not hardcode a
/// // number that drifts every time a tool is added.
/// ```
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one vec! literal per tool definition — extracting helpers adds noise without reducing complexity"
)]
pub fn all_tool_definitions() -> Vec<Tool> {
    vec![
        tool_def(
            "memory_ingest",
            "Append an event to the memory engine event log.",
            json!({
                "type": "object",
                "properties": {
                    "event_type": { "type": "string", "enum": ["Interaction", "ToolCall", "MemoryOp", "SystemEvent"], "description": "Type of event" },
                    "payload": { "description": "Event payload (arbitrary JSON)" },
                    "source": { "type": "string", "description": "Event source identifier" },
                    "session_id": { "type": "string", "description": "Session identifier (optional)" },
                    "scope": { "type": "string", "description": "Scope path (e.g. 'project/x'). Created if missing. No leading slash." },
                    "timestamp": { "type": "string", "format": "date-time", "description": "ISO 8601 timestamp. Defaults to now." }
                },
                "required": ["event_type", "payload", "source"]
            }),
        ),
        tool_def(
            "memory_add_fact",
            "Add a fact to memory with server-side embedding.",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Fact content text" },
                    "fact_type": { "type": "string", "enum": ["Episodic", "Semantic", "Procedural"], "default": "Semantic" },
                    "source_event_id": { "type": "integer", "description": "Link to source event" },
                    "scope": { "type": "string", "description": "Scope path" },
                    "base_importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5 },
                    "metadata": { "description": "Arbitrary JSON metadata" },
                    "t_valid": { "type": "string", "format": "date-time", "description": "Real-world validity start (future = scheduled memory)" },
                    "t_invalid": { "type": "string", "format": "date-time", "description": "Real-world validity end" },
                    "pinned": { "type": "boolean", "description": "Make this fact unforgettable" },
                    "embedding": { "type": "array", "items": { "type": "number" }, "description": "Pre-computed embedding (bypasses server-side embedding). Requires model + provider to declare its identity." },
                    "model": { "type": "string", "description": "Required with `embedding`: model slug that produced the vector (e.g. \"Qwen/Qwen3-Embedding-0.6B\"). Recorded on a fresh store; checked against the store identity otherwise." },
                    "provider": { "type": "string", "description": "Required with `embedding`: serving backend (e.g. \"tei\", \"ollama\")." },
                    "matryoshka_base_dim": { "type": "integer", "description": "Optional with `embedding`: native model dimension before MRL truncation. Omit if not truncated." },
                    "element_type": { "type": "string", "description": "Optional with `embedding`: vector element type (default \"float32\")." }
                },
                "required": ["content"]
            }),
        ),
        tool_def(
            "memory_query",
            "Search memory using hybrid FTS + vector retrieval.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Search query text" },
                    "embedding": { "type": "array", "items": { "type": "number" }, "description": "Pre-computed query embedding. Requires model + provider to declare its identity (verified against the store)." },
                    "model": { "type": "string", "description": "Required with `embedding`: model slug that produced the query vector. Checked against the store identity; mismatch is rejected." },
                    "provider": { "type": "string", "description": "Required with `embedding`: serving backend (e.g. \"tei\", \"ollama\")." },
                    "matryoshka_base_dim": { "type": "integer", "description": "Optional with `embedding`: native model dimension before MRL truncation." },
                    "element_type": { "type": "string", "description": "Optional with `embedding`: vector element type (default \"float32\")." },
                    "mode": { "type": "string", "enum": ["fts", "vector", "hybrid"], "default": "hybrid", "description": "Search mode" },
                    "scope": { "type": "string" },
                    "scope_mode": { "type": "string", "enum": ["exact", "subtree", "ancestors", "inherited"], "default": "subtree" },
                    "period_start": { "type": "string", "format": "date-time" },
                    "period_end": { "type": "string", "format": "date-time" },
                    "fact_type": { "type": "string", "enum": ["Episodic", "Semantic", "Procedural"] },
                    "min_importance": { "type": "number" },
                    "pinned_only": { "type": "boolean", "default": false },
                    "limit": { "type": "integer", "default": 10 },
                    "include_expired_probe": { "type": "boolean", "default": false, "description": "Run a secondary probe for expired facts matching the query. Adds one SQL query. Only effective with text search." },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                }
            }),
        ),
        tool_def(
            "memory_resume_context",
            "Retrieve tiered cognitive boot context (4-tier: pinned → high-importance → due → recent).",
            json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string" },
                    "pinned_cap": { "type": "integer", "default": 50 },
                    "high_importance_cap": { "type": "integer", "default": 20 },
                    "high_importance_min": { "type": "number", "default": 0.7 },
                    "due_cap": { "type": "integer", "default": 10 },
                    "recent_cap": { "type": "integer", "default": 10 },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                }
            }),
        ),
        tool_def(
            "memory_list_due",
            "List facts whose scheduled time (t_valid) has arrived.",
            json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string" },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                }
            }),
        ),
        tool_def(
            "memory_next_due_time",
            "Get the next scheduled fact time (for timer-based polling).",
            json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string" }
                }
            }),
        ),
        tool_def(
            "memory_explain_fact",
            "Explain a fact's state, provenance, and graph context.",
            json!({
                "type": "object",
                "properties": {
                    "fact_id": { "type": "integer", "description": "Fact ID to explain" },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                },
                "required": ["fact_id"]
            }),
        ),
        tool_def(
            "memory_get_fact",
            "Retrieve a single fact by ID.",
            json!({
                "type": "object",
                "properties": {
                    "fact_id": { "type": "integer", "description": "Fact ID" },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                },
                "required": ["fact_id"]
            }),
        ),
        tool_def(
            "memory_statistics",
            "Get aggregate engine statistics (facts, edges, scopes, storage).",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool_def(
            "memory_flush_insights",
            "Batch-add insights before context window compaction.",
            json!({
                "type": "object",
                "properties": {
                    "insights": {
                        "type": "array",
                        "maxItems": 10000,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "fact_type": { "type": "string", "enum": ["Episodic", "Semantic", "Procedural"], "default": "Semantic" },
                                "scope": { "type": "string" },
                                "base_importance": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                                "metadata": { "description": "Arbitrary JSON metadata" }
                            },
                            "required": ["content"]
                        },
                        "description": "Array of insights to add as facts"
                    }
                },
                "required": ["insights"]
            }),
        ),
        // -- P1 tools --
        tool_def(
            "memory_consolidate",
            "Run consolidation: deduplicate near-identical facts and cluster related ones into summaries. Requires summary generator to be configured.",
            json!({
                "type": "object",
                "properties": {
                    "dedup_threshold": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Cosine similarity threshold for deduplication (default: 0.92)" },
                    "cluster_threshold": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Cosine similarity threshold for clustering related facts; looser than dedup (default: 0.85)" },
                    "min_cluster_size": { "type": "integer", "minimum": 2, "description": "Minimum facts to form a cluster (default: 3)" }
                }
            }),
        ),
        tool_def(
            "memory_forget",
            "Prune stale facts using Ebbinghaus decay and multi-signal importance scoring. Pinned facts are never pruned.",
            json!({
                "type": "object",
                "properties": {
                    "half_life_days": { "type": "number", "exclusiveMinimum": 0, "description": "Base Ebbinghaus half-life in days (default: 69)" },
                    "half_life_overrides": { "type": "object", "description": "Per-FactType half-life overrides, e.g. {\"Episodic\": 30.0, \"Procedural\": 365.0}" },
                    "min_importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Threshold below which facts are expired (default: 0.1)" },
                    "recency_weight": { "type": "number", "minimum": 0.0, "description": "Weight for recency signal (default: 0.3)" },
                    "frequency_weight": { "type": "number", "minimum": 0.0, "description": "Weight for access frequency signal (default: 0.2)" },
                    "graph_degree_weight": { "type": "number", "minimum": 0.0, "description": "Weight for graph connectivity signal (default: 0.3)" },
                    "base_importance_weight": { "type": "number", "minimum": 0.0, "description": "Weight for base importance (default: 0.2)" }
                }
            }),
        ),
        tool_def(
            "memory_dump_state",
            "Export a full engine snapshot to a file. Returns the output file path.",
            json!({
                "type": "object",
                "properties": {
                    "format": { "type": "string", "enum": ["json", "sqlite"], "default": "json", "description": "Output format" },
                    "path": { "type": "string", "description": "Output file path. Defaults to temp directory with timestamp." }
                }
            }),
        ),
        tool_def(
            "memory_pin_fact",
            "Pin a fact to make it unforgettable (immune to forget/prune).",
            json!({
                "type": "object",
                "properties": {
                    "fact_id": { "type": "integer", "description": "Fact ID to pin" }
                },
                "required": ["fact_id"]
            }),
        ),
        tool_def(
            "memory_unpin_fact",
            "Unpin a fact to allow forgetting.",
            json!({
                "type": "object",
                "properties": {
                    "fact_id": { "type": "integer", "description": "Fact ID to unpin" }
                },
                "required": ["fact_id"]
            }),
        ),
        // ----- P2 tools (debugging / operator) -----
        tool_def(
            "memory_replay_events",
            "Replay events from the append-only event log with filtering. Debugging tool for inspecting consolidation/forgetting/conflict decisions.",
            json!({
                "type": "object",
                "properties": {
                    "since": { "type": "string", "format": "date-time", "description": "Temporal lower bound (inclusive)" },
                    "until": { "type": "string", "format": "date-time", "description": "Temporal upper bound (inclusive)" },
                    "id_range_start": { "type": "integer", "minimum": 0, "description": "Event ID lower bound (inclusive). Must provide both start and end." },
                    "id_range_end": { "type": "integer", "minimum": 0, "description": "Event ID upper bound (inclusive). Must provide both start and end." },
                    "session_id": { "type": "string", "description": "Filter by session identifier" },
                    "event_type": { "type": "string", "enum": ["Interaction", "ToolCall", "MemoryOp", "SystemEvent"] },
                    "limit": { "type": "integer", "minimum": 0, "default": 100, "description": "Maximum events to return (0 = no limit)" },
                    "upcast": { "type": "boolean", "default": false, "description": "Apply event payload upcasting" },
                    "order": { "type": "string", "enum": ["insertion", "timestamp"], "default": "insertion" },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                }
            }),
        ),
        tool_def(
            "memory_fact_history",
            "Get the temporal timeline for a single fact — created, became valid, became invalid, expired.",
            json!({
                "type": "object",
                "properties": {
                    "fact_id": { "type": "integer", "description": "Fact ID to inspect" },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                },
                "required": ["fact_id"]
            }),
        ),
        tool_def(
            "memory_bootstrap_session",
            "Bulk import facts from a Claude Code JSONL session log. Requires embedding provider.",
            json!({
                "type": "object",
                "properties": {
                    "jsonl_data": { "type": "string", "maxLength": 52_428_800, "description": "Raw JSONL session log content. The server enforces a hard 50 MB (52,428,800-byte) limit; the schema maxLength is an approximate character-count hint (JSON Schema has no byte-length keyword), so the authoritative cap is byte-based." },
                    "scope": { "type": "string", "description": "Scope path for imported facts" },
                    "max_turns": { "type": "integer", "minimum": 0, "default": 0, "description": "Max turns to process (0 = unlimited)" },
                    "skip_existing": { "type": "boolean", "default": true, "description": "Skip sessions already bootstrapped" }
                },
                "required": ["jsonl_data"]
            }),
        ),
        // Phase 5a: Outcome tracking
        tool_def(
            "memory_record_outcome",
            "Record a positive, negative, or neutral outcome signal for a fact. Used by consumers to provide feedback on fact usefulness.",
            json!({
                "type": "object",
                "properties": {
                    "fact_id": { "type": "integer", "description": "Fact ID to record outcome for" },
                    "outcome": { "type": "string", "enum": ["Positive", "Negative", "Neutral"], "description": "Outcome signal" }
                },
                "required": ["fact_id", "outcome"]
            }),
        ),
        tool_def(
            "memory_outcome_counts",
            "Get aggregated outcome counts (positive/negative/neutral) for a fact.",
            json!({
                "type": "object",
                "properties": {
                    "fact_id": { "type": "integer", "description": "Fact ID to query outcome counts for" }
                },
                "required": ["fact_id"]
            }),
        ),
        // Activity stream + session lifecycle (#224)
        tool_def(
            "memory_record_activity",
            "Record a tool invocation activity. Server-side filtering deduplicates, ignores formatting, and can promote significant actions to facts.",
            json!({
                "type": "object",
                "properties": {
                    "tool": { "type": "string", "description": "Tool name that was invoked" },
                    "args": { "description": "Tool arguments (arbitrary JSON)" },
                    "result": { "type": "string", "description": "Tool result summary (truncated at 512 chars)" },
                    "session_id": { "type": "string", "description": "Current session ID" },
                    "timestamp": { "type": "string", "format": "date-time", "description": "ISO 8601 timestamp. Defaults to now." },
                    "scope": { "type": "string", "description": "Scope path for the activity" },
                    "outcome_class": { "type": "string", "description": "Outcome class (e.g. 'success', 'error', 'test_failure'). Default: 'success'" }
                },
                "required": ["tool", "session_id"]
            }),
        ),
        tool_def(
            "memory_checkpoint_session",
            "Checkpoint the current session (last-write-wins). Called by Stop hook.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "Session ID to checkpoint" },
                    "scope": { "type": "string", "description": "Scope path (e.g. 'project:memory-engine')" },
                    "summary": { "type": "string", "description": "Free-form session summary" },
                    "metadata": { "description": "Arbitrary JSON metadata" }
                },
                "required": ["session_id"]
            }),
        ),
        tool_def(
            "memory_load_context",
            "Load active project context for session start. Returns recent activities, last checkpoint, and relevant facts.",
            json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "Scope path (e.g. 'project:memory-engine')" },
                    "activity_limit": { "type": "integer", "default": 20, "description": "Max recent activities to return" },
                    "fact_limit": { "type": "integer", "default": 10, "description": "Max relevant facts to return" },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                },
                "required": ["scope"]
            }),
        ),
        // Phase 5a: Cognitive pipeline (dream cycle) tools (#225)
        tool_def(
            "memory_dream_cycle",
            "Run the dream-cycle cognitive pipeline (cluster → promote → rescore → quarantine) and return a delta-based CycleReport. With apply=true (default) the report is applied immediately; with apply=false it is returned unapplied for review and applied later via memory_apply_cycle_report. Does NOT run consolidation (use memory_consolidate for that).",
            json!({
                "type": "object",
                "properties": {
                    "apply": { "type": "boolean", "default": true, "description": "Apply the produced report immediately. If false, return the unapplied report for review." }
                }
            }),
        ),
        tool_def(
            "memory_apply_cycle_report",
            "Apply a CycleReport (as returned by memory_dream_cycle with apply=false) to the store, returning an ApplyResult. The whole report is validated before any mutation; a malformed or stale report is rejected without changing the store.",
            json!({
                "type": "object",
                "properties": {
                    "report": { "type": "object", "description": "A CycleReport JSON object produced by memory_dream_cycle (apply=false)." }
                },
                "required": ["report"]
            }),
        ),
        tool_def(
            "memory_get_recent_insights",
            "Return recent model-logged insights (facts flushed via memory_flush_insights) within a project scope subtree, newest-first.",
            json!({
                "type": "object",
                "properties": {
                    "project_path": { "type": "string", "description": "Scope path of the project (e.g. 'project:memory-engine'). Insights anywhere in this subtree are returned." },
                    "limit": { "type": "integer", "minimum": 1, "default": 20, "description": "Max insights to return (newest-first)." },
                    "depth": { "type": "string", "enum": ["sparse", "standard", "full"], "default": "standard" }
                },
                "required": ["project_path"]
            }),
        ),
    ]
}

fn tool_def(name: &'static str, description: &'static str, schema: Value) -> Tool {
    let schema_obj: Map<String, Value> = match schema {
        Value::Object(m) => m,
        _ => unreachable!("schema must be an object"),
    };
    Tool::new(name, description, Arc::new(schema_obj))
}

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
    let args = &args;
    let embedder = embedder.as_ref();
    match name {
        "memory_ingest" => handle_ingest(args, engine).await,
        "memory_add_fact" => handle_add_fact(args, engine, embedder, embed_dim).await,
        "memory_query" => handle_query(args, engine, embedder, embed_dim).await,
        "memory_resume_context" => handle_resume_context(args, engine).await,
        "memory_list_due" => handle_list_due(args, engine).await,
        "memory_next_due_time" => handle_next_due_time(args, engine).await,
        "memory_explain_fact" => handle_explain_fact(args, engine).await,
        "memory_get_fact" => handle_get_fact(args, engine).await,
        "memory_statistics" => handle_statistics(engine).await,
        "memory_flush_insights" => handle_flush_insights(args, engine, embedder).await,
        // P1 tools
        "memory_consolidate" => {
            handle_consolidate(args, engine, embedder, summary_gen.as_ref()).await
        }
        "memory_forget" => handle_forget(args, engine).await,
        "memory_dump_state" => handle_dump_state(args, engine).await,
        "memory_pin_fact" => handle_pin_fact(args, engine).await,
        "memory_unpin_fact" => handle_unpin_fact(args, engine).await,
        // P2 tools
        "memory_replay_events" => handle_replay_events(args, engine).await,
        "memory_fact_history" => handle_fact_history(args, engine).await,
        "memory_bootstrap_session" => handle_bootstrap_session(args, engine, embedder).await,
        // Phase 5a: Outcome tracking
        "memory_record_outcome" => handle_record_outcome(args, engine).await,
        "memory_outcome_counts" => handle_outcome_counts(args, engine).await,
        // Activity stream + session lifecycle (#224)
        "memory_record_activity" => {
            handle_record_activity(args, engine, embedder, filter_config).await
        }
        "memory_checkpoint_session" => handle_checkpoint_session(args, engine).await,
        "memory_load_context" => handle_load_context(args, engine).await,
        // Phase 5a: Cognitive pipeline (dream cycle) (#225)
        "memory_dream_cycle" => handle_dream_cycle(args, engine).await,
        "memory_apply_cycle_report" => handle_apply_cycle_report(args, engine).await,
        "memory_get_recent_insights" => handle_get_recent_insights(args, engine).await,
        _ => Err(ErrorData::invalid_params(
            format!("unknown tool: {name}"),
            None,
        )),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_str(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(String::from)
}

fn get_i64(args: &Map<String, Value>, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

fn get_f64(args: &Map<String, Value>, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn get_bool(args: &Map<String, Value>, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Read an optional non-negative integer parameter.
///
/// Distinguishes *absent* (`Ok(None)`) from *present-but-invalid* (`Err`). A
/// present-but-negative value is rejected with `invalid_params` rather than
/// silently dropped (#339): dropping it would let the engine fall back to its
/// own default — e.g. returning more results than an untrusted caller asked for.
fn get_usize(args: &Map<String, Value>, key: &str) -> Result<Option<usize>, ErrorData> {
    get_i64(args, key).map_or(Ok(None), |v| {
        usize::try_from(v).map(Some).map_err(|_| {
            ErrorData::invalid_params(format!("{key} must be a non-negative integer"), None)
        })
    })
}

fn get_datetime(args: &Map<String, Value>, key: &str) -> Result<Option<DateTime<Utc>>, ErrorData> {
    get_str(args, key).map_or(Ok(None), |s| {
        s.parse::<DateTime<Utc>>()
            .map(Some)
            .map_err(|e| ErrorData::invalid_params(format!("invalid {key}: {e}"), None))
    })
}

fn get_depth(args: &Map<String, Value>) -> Result<Depth, ErrorData> {
    match args.get("depth") {
        None | Some(Value::Null) => Ok(Depth::default()),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| ErrorData::invalid_params(format!("invalid depth: {e}"), None)),
    }
}

/// Parse an embedding from a JSON value, returning an error if present but malformed.
///
/// #294 (CWE-400/770 pre-allocation DoS): the array's length is checked against
/// `expected_dim` BEFORE `serde_json::from_value` materializes a `Vec<f32>`, so a
/// hostile client cannot force the server to allocate an arbitrarily large vector
/// only to reject it afterward. A wrong-length array is rejected on its length
/// alone — this doubles as the query-path dimension check that previously existed
/// only on the add-fact path.
fn parse_embedding(
    args: &Map<String, Value>,
    expected_dim: usize,
) -> Result<Option<Vec<f32>>, ErrorData> {
    match args.get("embedding") {
        None | Some(Value::Null) => Ok(None),
        Some(v @ Value::Array(arr)) => {
            // Length gate FIRST: reject the wrong-dimension array before allocating it.
            if arr.len() != expected_dim {
                return Err(ValidationError::EmbeddingDimension {
                    expected: expected_dim,
                    actual: arr.len(),
                }
                .into());
            }
            // Deserialize from the borrowed `Value` — no `arr.clone()` of the whole
            // JSON array. The length gate above still runs before any `Vec<f32>`
            // allocation, so the pre-alloc DoS guard is preserved (#498
            // `mcp/performance-parse-embedding-clone`).
            <Vec<f32> as serde::Deserialize>::deserialize(v)
                .map(Some)
                .map_err(|e| ErrorData::invalid_params(format!("invalid embedding: {e}"), None))
        }
        // Present but not an array (e.g. a string or number): malformed input.
        Some(v) => Err(ErrorData::invalid_params(
            format!("invalid embedding: expected an array of numbers, got {v}"),
            None,
        )),
    }
}

/// Parse the caller-declared embedding identity that MUST accompany a pre-computed
/// `embedding` (#615, §Design.3).
///
/// `model` and `provider` are **required** (the identity-critical pair); `dim` is the
/// vector length the caller submitted; `matryoshka_base_dim` and `element_type` are
/// optional, defaulting to no-truncation / `"float32"`. The declared tuple is compared
/// (full `Eq`) against the store's recorded identity by the engine, closing the
/// same-dimension foreign-vector hole — a vector from a different model can no longer be
/// slipped into the store's vector space unchecked.
fn parse_declared_fingerprint(
    args: &Map<String, Value>,
    dim: usize,
) -> Result<EmbeddingFingerprint, ErrorData> {
    let model = get_str(args, "model").ok_or_else(|| {
        ErrorData::invalid_params(
            "a pre-computed `embedding` requires a declared `model` (the model that produced it)",
            None,
        )
    })?;
    let provider = get_str(args, "provider").ok_or_else(|| {
        ErrorData::invalid_params(
            "a pre-computed `embedding` requires a declared `provider` (e.g. \"tei\", \"ollama\")",
            None,
        )
    })?;
    let matryoshka_base_dim = match args.get("matryoshka_base_dim") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        "matryoshka_base_dim must be a non-negative integer",
                        None,
                    )
                })?,
        ),
    };
    // A present-but-non-string `element_type` is rejected, not silently ignored — a
    // malformed value must not fall back to the "float32" default and slip past the
    // full-tuple identity check (consistent with matryoshka_base_dim's rejection).
    let element_type = match args.get("element_type") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(ErrorData::invalid_params(
                "element_type must be a string (e.g. \"float32\")",
                None,
            ));
        }
    };
    let mut fp = match matryoshka_base_dim {
        Some(base) => EmbeddingFingerprint::with_matryoshka(model, provider, dim, base),
        None => EmbeddingFingerprint::new(model, provider, dim),
    };
    if let Some(element_type) = element_type {
        fp.element_type = element_type;
    }
    Ok(fp)
}

fn parse_search_mode(s: &str) -> Result<SearchMode, ErrorData> {
    match s {
        "fts" => Ok(SearchMode::Fts),
        "vector" => Ok(SearchMode::Vector),
        "hybrid" => Ok(SearchMode::Hybrid),
        other => Err(ErrorData::invalid_params(
            format!("unknown search mode: {other}"),
            None,
        )),
    }
}

/// Parse an MCP `event_type` tool parameter into the core [`EventType`].
///
/// Delegates to core's canonical [`EventType::from_str`] (the single source of
/// truth, #353/#678), so casing is reconciled across surfaces: the JSON schemas
/// advertise `PascalCase` (`"Interaction"`), but `snake_case` is also accepted.
///
/// One MCP-specific narrowing: [`EventType::OutcomeSignal`] is **rejected** here.
/// It is a system-generated event (emitted by `record_outcome`), not a
/// user-ingestible type — the `ingest` / `replay` JSON schemas deliberately omit
/// it. The core parser is complete (it accepts every variant); this boundary gate
/// preserves the schema contract without re-implementing the variant mapping.
fn parse_event_type(s: &str) -> Result<EventType, ValidationError> {
    // The system-only-reject arm and the unparseable arm share a body, but keeping
    // them separate is deliberate: it documents the two distinct rejection reasons
    // and keeps the `EventType` match exhaustive (no `Ok(_)` catch-all), so a new
    // variant forces a deliberate allow/reject decision here at compile time.
    #[allow(clippy::match_same_arms)]
    match s.parse::<EventType>() {
        // User-facing types — accepted at the MCP ingest/replay boundary. These are
        // exactly the variants the `ingest` / `replay` JSON schemas advertise.
        Ok(
            et @ (EventType::Interaction
            | EventType::ToolCall
            | EventType::MemoryOp
            | EventType::SystemEvent),
        ) => Ok(et),
        // System-generated only — emitted internally by `record_outcome`, never
        // user-ingestible; the schemas deliberately omit it.
        Ok(EventType::OutcomeSignal) => Err(ValidationError::UnknownEventType(s.to_owned())),
        // NOTE: intentionally NO catch-all `Ok(_)`. Adding a new `EventType` variant
        // must force a deliberate allow/reject decision here — the compiler flags
        // the non-exhaustive match instead of silently making it user-ingestible.
        Err(_) => Err(ValidationError::UnknownEventType(s.to_owned())),
    }
}

/// Parse an MCP `fact_type` tool parameter into the core [`FactType`].
///
/// Delegates to core's canonical [`FactType::from_str`] (the single source of
/// truth shared with the CLI), so casing is reconciled across surfaces: the JSON
/// schemas advertise `PascalCase` (`"Episodic"`), but `snake_case` is also accepted.
fn parse_fact_type(s: &str) -> Result<FactType, ValidationError> {
    s.parse()
        .map_err(|_| ValidationError::UnknownFactType(s.to_owned()))
}

/// Like `get_f64`, but returns a validation error if the key is present with a non-numeric type.
/// Prevents silent fallback to defaults on type mismatches (e.g., `"half_life_days": "1"`).
fn require_f64_if_present(args: &Map<String, Value>, key: &str) -> Result<Option<f64>, ErrorData> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| {
            ErrorData::invalid_params(format!("{key} must be a number, got {v}"), None)
        }),
    }
}

fn ok_json(value: Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![Content::json(value)?]))
}

/// Serialize an engine-produced value into a tool result. A serde failure maps to an
/// internal error (the value is engine-produced, so failure is a server bug).
#[must_use = "the serialized tool result must be returned to the caller"]
fn ok_serialized<T: serde::Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let v = serde_json::to_value(value)
        .map_err(|e| ErrorData::internal_error(format!("serialize: {e}"), None))?;
    ok_json(v)
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

async fn handle_ingest(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let event_type_str = get_str(args, "event_type")
        .ok_or_else(|| ErrorData::invalid_params("missing event_type", None))?;
    let event_type = parse_event_type(&event_type_str)?;
    let payload = args.get("payload").cloned().unwrap_or_else(|| json!({}));
    let source =
        get_str(args, "source").ok_or_else(|| ErrorData::invalid_params("missing source", None))?;
    let session_id = get_str(args, "session_id");
    let timestamp = get_datetime(args, "timestamp")?.unwrap_or_else(Utc::now);

    let scope_id = match get_str(args, "scope") {
        Some(path) => engine
            .ensure_scope_path(&path)
            .await
            .map_err(to_mcp_error)?,
        None => 1, // root scope
    };

    let event = NewEvent {
        timestamp,
        event_type,
        payload,
        source,
        session_id,
        scope_id,
        origin_node_id: "mcp".to_owned(),
        sequence_id: 0,
        created_at: None,
    };

    let event_id = engine.ingest(&event).await.map_err(to_mcp_error)?;
    ok_json(json!({ "event_id": event_id }))
}

async fn handle_add_fact(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
    embed_dim: usize,
) -> Result<CallToolResult, ErrorData> {
    let content = get_str(args, "content")
        .ok_or_else(|| ErrorData::invalid_params("missing content", None))?;
    let fact_type = match get_str(args, "fact_type") {
        Some(s) => parse_fact_type(&s)?,
        None => FactType::Semantic,
    };
    let source_event_id = get_i64(args, "source_event_id");
    let scope = get_str(args, "scope");

    // Validate importance range
    let importance = get_f64(args, "base_importance");
    if let Some(imp) = importance
        && !(0.0..=1.0).contains(&imp)
    {
        return Err(ValidationError::ImportanceOutOfRange(imp).into());
    }

    // Validate temporal consistency
    let t_valid = get_datetime(args, "t_valid")?;
    let t_invalid = get_datetime(args, "t_invalid")?;
    if let (Some(tv), Some(ti)) = (t_valid, t_invalid)
        && tv >= ti
    {
        return Err(ValidationError::TemporalInconsistency.into());
    }

    let pinned = get_bool(args, "pinned");
    let metadata = args.get("metadata").cloned();

    // Pre-computed embedding or server-side embedding. `parse_embedding` rejects a
    // wrong-`embed_dim` array up front (#294), so no separate length check is needed.
    let pre_computed = parse_embedding(args, embed_dim)?;

    let req = AddFactRequest {
        content,
        fact_type,
        source_event_id,
        scope,
        opts: Some(AddFactOptions {
            base_importance: importance,
            metadata,
            t_valid,
            t_invalid,
            pinned,
            ..Default::default()
        }),
    };

    let fact_id = if let Some(emb) = pre_computed {
        // Pre-computed vector: the caller declares the model that produced it (#615,
        // §Design.3). record_if_absent records that declared identity on a fresh store or
        // compares it (full tuple) against the stored one, rejecting a foreign vector —
        // closing the same-dim hole the old passthrough sentinel left open.
        let declared = parse_declared_fingerprint(args, emb.len())?;
        engine
            .add_fact_precomputed(&req, emb, &declared, None)
            .await
            .map_err(to_mcp_error)?
    } else {
        let emb = embedder.ok_or(ValidationError::NoEmbeddingProvider)?;
        // The async engine takes the provider as an owned `Arc<dyn EmbeddingProvider>`
        // (#631 §1.2) so it can clone it into the `spawn_blocking` embed offload.
        let provider = Arc::clone(emb) as Arc<dyn EmbeddingProvider>;
        engine
            .add_fact(&req, provider, None)
            .await
            .map_err(to_mcp_error)?
    };

    ok_json(json!({ "fact_id": fact_id }))
}

// Slightly over the 100-line soft cap after the #631 cutover added the `embed_query`
// spawn_blocking offload; the handler is a single linear request→embed→query→format flow
// that reads worse split across helpers.
#[allow(clippy::too_many_lines)]
async fn handle_query(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
    embed_dim: usize,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(args)?;

    // Validate the cheap scalar param up front, before the (potentially network-bound)
    // query embedding below — a malformed `limit` should be rejected without first
    // burning an `embed_query` call whose result we would only discard.
    let limit = get_usize(args, "limit")?;

    let mut query = memory_engine::MemoryQuery::new();

    // Parse and validate search mode (if explicit)
    let explicit_mode = match get_str(args, "mode") {
        Some(s) => Some(parse_search_mode(&s)?),
        None => None,
    };

    // Parse embedding (with proper error on malformed input). A wrong-length array
    // is rejected before allocation against the store's `embed_dim` (#294) — this
    // also closes the query path's previously-missing dimension check.
    let pre_emb = parse_embedding(args, embed_dim)?;

    // §Design.3 (#615): a pre-computed query embedding must declare the model that
    // produced it; verify that declaration against the store's identity before using the
    // vector, so a foreign-vector-space query can't silently mis-retrieve. Covers both
    // use sites below (with-text hybrid/vector, and embedding-only).
    if let Some(ref emb) = pre_emb {
        let declared = parse_declared_fingerprint(args, emb.len())?;
        engine
            .verify_embedding_fingerprint(&declared)
            .await
            .map_err(to_mcp_error)?;
    }

    if let Some(text) = get_str(args, "text") {
        query = query.text(text.clone());

        // Determine effective mode for embedding decision
        // None defaults to true: try to provide embedding for hybrid if possible
        let needs_embedding = match explicit_mode {
            Some(SearchMode::Fts) => false,
            Some(SearchMode::Vector | SearchMode::Hybrid) | None => true,
        };

        if needs_embedding {
            if let Some(emb) = pre_emb {
                query = query.embedding(emb);
            } else if let Some(emb_provider) = embedder {
                // Query path uses embed_query, applying the asymmetric instruction prefix
                // for models like Qwen (#618). add_fact stays on the document `embed`: it
                // passes the provider to engine.add_fact, which calls embed() internally.
                //
                // This is the ONE consumer-trait call made at the MCP layer rather than
                // inside the engine ([`MemoryQuery`] carries a pre-computed vector, so the
                // engine never embeds the query). It MUST be offloaded onto the blocking
                // pool: a `reqwest::blocking` provider spins up — and on drop tears down —
                // an internal runtime, which panics ("cannot drop a runtime …") if run
                // inline on the async executor thread. `spawn_blocking` keeps it off-runtime,
                // mirroring the engine's own consumer-trait offload (#631 §1.2).
                let provider = Arc::clone(emb_provider);
                let text = text.clone();
                let emb = tokio::task::spawn_blocking(move || provider.embed_query(&text))
                    .await
                    .map_err(|e| {
                        ErrorData::internal_error(format!("query embed task join error: {e}"), None)
                    })?
                    .map_err(to_mcp_error)?;
                query = query.embedding(emb);
            } else if let Some(mode) = explicit_mode {
                // User explicitly asked for vector/hybrid but no embedder available
                return Err(ErrorData::invalid_params(
                    format!(
                        "mode '{mode:?}' requires an embedding provider or pre-computed embedding"
                    ),
                    None,
                ));
            }
            // If no explicit mode and no embedder, let engine infer FTS-only
        }
    } else if let Some(emb) = pre_emb {
        query = query.embedding(emb);
    }

    // Only set search_mode when user explicitly requested one.
    // Otherwise let the engine infer from available data (text, embedding, both).
    if let Some(mode) = explicit_mode {
        query = query.search_mode(mode);
    }

    // Scope
    if let Some(scope) = get_str(args, "scope") {
        let scope_mode = get_str(args, "scope_mode").unwrap_or_else(|| "subtree".to_owned());
        query = match scope_mode.as_str() {
            "subtree" => query.scope_subtree(scope),
            "exact" => query.scope_exact(scope),
            "ancestors" => query.scope_ancestors(scope),
            "inherited" => query.scope_inherited(scope),
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown scope_mode: {other}"),
                    None,
                ));
            }
        };
    }

    // Temporal filters — reject one-sided periods
    let period_start = get_datetime(args, "period_start")?;
    let period_end = get_datetime(args, "period_end")?;
    match (period_start, period_end) {
        (Some(start), Some(end)) => {
            query = query.period(start, end);
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(ErrorData::invalid_params(
                "both period_start and period_end must be provided, or neither",
                None,
            ));
        }
        (None, None) => {}
    }

    if let Some(ft) = get_str(args, "fact_type") {
        query = query.fact_type(parse_fact_type(&ft)?);
    }
    if let Some(min) = get_f64(args, "min_importance") {
        query = query.min_importance_score(min);
    }
    if get_bool(args, "pinned_only").unwrap_or(false) {
        query = query.pinned_only();
    }
    if let Some(limit) = limit {
        query = query.limit(limit);
    }
    if get_bool(args, "include_expired_probe").unwrap_or(false) {
        query = query.include_expired_probe();
    }

    let response = engine.execute_query(&query).await.map_err(to_mcp_error)?;

    let shaped: Vec<Value> = response
        .results
        .iter()
        .map(|r| depth::shape_search_result(r, depth_level, None))
        .collect::<Result<_, _>>()?;

    let diagnostics = depth::shape_diagnostics(&response.diagnostics, depth_level);

    ok_json(json!({ "results": shaped, "count": shaped.len(), "diagnostics": diagnostics }))
}

async fn handle_resume_context(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(args)?;

    let config = ResumeConfig {
        scope_path: get_str(args, "scope"),
        now: Some(Utc::now()),
        pinned_cap: get_usize(args, "pinned_cap")?.unwrap_or(50),
        high_importance_cap: get_usize(args, "high_importance_cap")?.unwrap_or(20),
        high_importance_min: get_f64(args, "high_importance_min").unwrap_or(0.7),
        due_cap: get_usize(args, "due_cap")?.unwrap_or(10),
        recent_cap: get_usize(args, "recent_cap")?.unwrap_or(10),
    };

    let ctx = engine.resume_context(&config).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_resume_context(&ctx, depth_level);

    ok_json(shaped)
}

async fn handle_list_due(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(args)?;
    let scope = get_str(args, "scope");

    let facts = engine
        .list_due(Utc::now(), scope.as_deref())
        .await
        .map_err(to_mcp_error)?;

    let shaped: Vec<Value> = facts
        .iter()
        .map(|f| depth::shape_fact(f, depth_level, None))
        .collect();

    ok_json(json!({ "facts": shaped, "count": shaped.len() }))
}

async fn handle_next_due_time(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let scope = get_str(args, "scope");
    let next = engine
        .next_due_time(scope.as_deref())
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({ "next_due": next }))
}

async fn handle_explain_fact(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(args)?;

    let explanation: FactExplanation = engine.explain_fact(fact_id).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_explanation(&explanation, depth_level)?;

    ok_json(shaped)
}

async fn handle_get_fact(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(args)?;

    let fact = engine.get_fact(fact_id).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_fact(&fact, depth_level, None);

    ok_json(shaped)
}

async fn handle_statistics(engine: &MemoryEngine) -> Result<CallToolResult, ErrorData> {
    let stats = engine.statistics().await.map_err(to_mcp_error)?;
    let value = serde_json::to_value(&stats)
        .map_err(|e| ErrorData::internal_error(format!("serialize stats: {e}"), None))?;
    ok_json(value)
}

async fn handle_flush_insights(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
) -> Result<CallToolResult, ErrorData> {
    let insights = args
        .get("insights")
        .and_then(Value::as_array)
        .ok_or_else(|| ErrorData::invalid_params("missing insights array", None))?;

    // #266 (CWE-400/770): cap the untrusted array length before materializing per-entry
    // `AddFactRequest`s. The bound is generous — a real pre-compaction flush carries at
    // most a few hundred insights — so it rejects only absurd input, never legitimate use.
    if insights.len() > MAX_FLUSH_INSIGHTS {
        return Err(ErrorData::invalid_params(
            format!(
                "too many insights: {} exceeds the cap of {MAX_FLUSH_INSIGHTS}",
                insights.len()
            ),
            None,
        ));
    }

    let emb = embedder.ok_or_else(|| {
        ErrorData::invalid_params(
            "embedding provider not configured — required for flush_insights",
            None,
        )
    })?;

    // --- Phase 1: Parse + validate all insights upfront ---
    let mut entries: Vec<AddFactRequest> = Vec::new();
    let mut entry_indices: Vec<usize> = Vec::new(); // original index for each valid entry
    let mut failed: Vec<Value> = Vec::new();

    for (i, insight) in insights.iter().enumerate() {
        let Some(obj) = insight.as_object() else {
            failed.push(json!({ "index": i, "error": "not an object" }));
            continue;
        };

        let Some(content) = get_str(obj, "content") else {
            failed.push(json!({ "index": i, "error": "missing content" }));
            continue;
        };

        let fact_type = match get_str(obj, "fact_type") {
            Some(s) => match parse_fact_type(&s) {
                Ok(ft) => ft,
                Err(e) => {
                    failed.push(json!({ "index": i, "error": e.to_string() }));
                    continue;
                }
            },
            None => FactType::Semantic,
        };

        let scope = get_str(obj, "scope");
        let importance = get_f64(obj, "base_importance");

        if let Some(imp) = importance
            && !(0.0..=1.0).contains(&imp)
        {
            failed.push(json!({ "index": i, "error": format!("importance must be in [0.0, 1.0], got {imp}") }));
            continue;
        }

        // Metadata must be absent/null or an object — a present non-object (e.g.
        // `"metadata": "foo"`) is a malformed entry, rejected into `failed` rather than
        // silently coerced (which would also drop the load-bearing insight marker). The
        // marker is then always stamped onto a valid object, so a flushed insight is
        // never invisible to get_recent_insights.
        let mut metadata = match obj.get("metadata") {
            None | Some(Value::Null) => serde_json::Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(v) => {
                failed.push(
                    json!({ "index": i, "error": format!("metadata must be an object, got {v}") }),
                );
                continue;
            }
        };
        metadata.insert("source".to_owned(), json!("pre_compaction_flush"));
        // Insight marker read by memory_get_recent_insights (shared INSIGHT_MARKER_KEY).
        metadata.insert(
            INSIGHT_MARKER_KEY.to_owned(),
            json!({ "flushed_at": Utc::now().to_rfc3339() }),
        );

        let opts = AddFactOptions {
            base_importance: importance,
            metadata: Some(Value::Object(metadata)),
            ..Default::default()
        };

        entries.push(AddFactRequest {
            content,
            fact_type,
            source_event_id: None,
            scope,
            opts: Some(opts),
        });
        entry_indices.push(i);
    }

    // --- Phase 2: Batch insert ---
    let fact_ids = if entries.is_empty() {
        Vec::new()
    } else {
        // The async engine takes the provider as an owned `Arc<dyn EmbeddingProvider>`
        // (#631 §1.2) so it can clone it into the `spawn_blocking` embed offload.
        let provider = Arc::clone(emb) as Arc<dyn EmbeddingProvider>;
        match engine.add_facts_batch(&entries, provider, None).await {
            Ok(ids) => ids,
            Err(e) => {
                // Batch failed — all valid entries become failures
                for &original_idx in &entry_indices {
                    failed.push(json!({ "index": original_idx, "error": e.to_string() }));
                }
                Vec::new()
            }
        }
    };

    ok_json(json!({
        "fact_ids": fact_ids,
        "added": fact_ids.len(),
        "failed": failed,
        "failed_count": failed.len(),
    }))
}

// ---------------------------------------------------------------------------
// P1 tool handlers
// ---------------------------------------------------------------------------

/// Parse + validate the `memory_consolidate` tuning args into a [`ConsolidationConfig`].
///
/// Extracted from the handler so the range checks (thresholds in `[0,1]`, cluster
/// size floor) are unit-testable directly: the handler short-circuits on a missing
/// provider *before* it would run, so an integration test with no providers can
/// never reach this validation (#344 review).
fn parse_consolidate_config(args: &Map<String, Value>) -> Result<ConsolidationConfig, ErrorData> {
    // `require_f64_if_present` rejects a present-but-wrong-type value (e.g.
    // `"0.95"`) instead of silently falling back to the default — `get_f64` would
    // return None on a type mismatch and hide the bad input. f64→f32 narrowing is
    // intentional: thresholds are similarity scores in [0,1], within f32's range.
    #[allow(clippy::cast_possible_truncation)]
    let dedup_threshold = require_f64_if_present(args, "dedup_threshold")?.unwrap_or(0.92) as f32;
    if !(0.0..=1.0).contains(&dedup_threshold) {
        return Err(ErrorData::invalid_params(
            format!("dedup_threshold must be in [0.0, 1.0], got {dedup_threshold}"),
            None,
        ));
    }

    // Clustering threshold is configurable symmetrically with dedup (#344); looser
    // than dedup by default.
    #[allow(clippy::cast_possible_truncation)]
    let cluster_threshold =
        require_f64_if_present(args, "cluster_threshold")?.unwrap_or(0.85) as f32;
    if !(0.0..=1.0).contains(&cluster_threshold) {
        return Err(ErrorData::invalid_params(
            format!("cluster_threshold must be in [0.0, 1.0], got {cluster_threshold}"),
            None,
        ));
    }

    let min_cluster_size = get_usize(args, "min_cluster_size")?.unwrap_or(3);
    if min_cluster_size < 2 {
        return Err(ErrorData::invalid_params(
            format!("min_cluster_size must be >= 2, got {min_cluster_size}"),
            None,
        ));
    }

    Ok(ConsolidationConfig::builder()
        .dedup_threshold(dedup_threshold)
        .cluster_threshold(cluster_threshold)
        .min_cluster_size(min_cluster_size)
        .build())
}

async fn handle_consolidate(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
    summary_gen: Option<&Arc<dyn SummaryGenerator + Send + Sync>>,
) -> Result<CallToolResult, ErrorData> {
    let generator = summary_gen.ok_or(ValidationError::NoSummaryProvider)?;
    // Issue #116: summaries are embedded via the EmbeddingProvider, not the
    // SummaryGenerator, so consolidation now requires an embedder too.
    let embedder = embedder.ok_or(ValidationError::NoEmbeddingProvider)?;

    let config = parse_consolidate_config(args)?;

    // The async engine takes both consumer traits as owned `Arc<dyn _>` (#631 §1.2) so it
    // can clone them into the lock-free `spawn_blocking` summarize/embed offload.
    let generator = Arc::clone(generator) as Arc<dyn SummaryGenerator>;
    let embedder = Arc::clone(embedder) as Arc<dyn EmbeddingProvider>;
    let stats = engine
        .consolidate(generator, embedder, &config)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "duplicates_removed": stats.duplicates_removed,
        "clusters_created": stats.clusters_created,
        "global_summaries": stats.global_summaries,
    }))
}

async fn handle_forget(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let mut policy = ForgetPolicy::default();

    if let Some(v) = require_f64_if_present(args, "half_life_days")? {
        policy.half_life_days = v;
    }
    if let Some(v) = require_f64_if_present(args, "min_importance")? {
        policy.min_importance = v;
    }
    if let Some(v) = require_f64_if_present(args, "recency_weight")? {
        policy.recency_weight = v;
    }
    if let Some(v) = require_f64_if_present(args, "frequency_weight")? {
        policy.frequency_weight = v;
    }
    if let Some(v) = require_f64_if_present(args, "graph_degree_weight")? {
        policy.graph_degree_weight = v;
    }
    if let Some(v) = require_f64_if_present(args, "base_importance_weight")? {
        policy.base_importance_weight = v;
    }

    // Parse per-FactType half-life overrides: {"Episodic": 30.0, "Procedural": 365.0}
    if let Some(val) = args.get("half_life_overrides") {
        match val {
            Value::Object(overrides) => {
                let mut map = HashMap::new();
                for (key, v) in overrides {
                    let ft = parse_fact_type(key)?;
                    let hl = v.as_f64().ok_or_else(|| {
                        ValidationError::Other(format!(
                            "half_life_overrides[\"{key}\"] must be a number"
                        ))
                    })?;
                    map.insert(ft, hl);
                }
                policy.half_life_overrides = map;
            }
            Value::Null => {} // explicitly null — use default
            _ => {
                return Err(ValidationError::Other(
                    "half_life_overrides must be a JSON object".to_owned(),
                )
                .into());
            }
        }
    }

    policy.validate().map_err(to_mcp_error)?;

    let stats = engine.forget(&policy).await.map_err(to_mcp_error)?;

    ok_json(json!({
        "facts_expired": stats.facts_expired,
        "facts_evaluated": stats.facts_evaluated,
    }))
}

/// Monotonic counter making default dump paths unique within a process, so
/// concurrent dumps (e.g. parallel tests) never collide on the timestamp.
static NEXT_DUMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Assemble a default dump filename from its already-resolved components.
///
/// Pure (no clock, no process state, no atomic): given a fixed `timestamp`,
/// `pid`, and `seq`, it always yields the same `memory-dump-<ts>-<pid>-<seq>.<ext>`
/// name. The atomic counter (`seq`) is the load-bearing uniqueness guard for
/// same-process dumps; the timestamp only keeps names time-ordered and the pid
/// disambiguates across processes. Factored out so a test can hold `timestamp`
/// and `pid` constant and prove that `seq` alone makes the names distinct — if
/// the counter were dropped the names would collide, which the timestamp would
/// otherwise mask on a host with a fine-grained clock.
fn default_dump_name(timestamp: &str, pid: u32, seq: u64, ext: &str) -> String {
    format!("memory-dump-{timestamp}-{pid}-{seq}.{ext}")
}

/// Build a collision-safe default dump path inside `base_dir`.
///
/// The filename combines a nanosecond timestamp, the process id, and a
/// process-global monotonic counter:
/// `memory-dump-<ts>-<pid>-<seq>.<ext>`. The atomic counter guarantees
/// uniqueness for any two dumps within the same process (the case that made
/// `test_dump_state_json` flaky under parallel `cargo test`), while the pid
/// disambiguates across processes and the nanosecond timestamp keeps names
/// time-ordered. Naming is delegated to [`default_dump_name`] so the uniqueness
/// invariant can be tested deterministically without wall-clock timing.
fn default_dump_path(base_dir: &std::path::Path, ext: &str) -> PathBuf {
    let seq = NEXT_DUMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%9f").to_string();
    let pid = std::process::id();
    base_dir.join(default_dump_name(&timestamp, pid, seq, ext))
}

/// Validate and resolve a client-supplied dump destination, confining it to the
/// system temp directory.
///
/// Without this, an MCP client could direct a dump at an arbitrary path and
/// overwrite host files. The hardening closes three lenses on one flaw
/// (issues #296 / #354 / #414):
///
/// - **CWE-22 (path traversal):** both the temp root and the target are
///   canonicalized, so the `starts_with` check compares fully-resolved paths.
///   Canonicalizing the temp root also stops *false rejects* on platforms where
///   the temp dir is itself a symlink (e.g. macOS `/tmp -> /private/tmp`).
/// - **CWE-59 (symlink-leaf follow):** the *full* target is resolved — not just
///   its parent — and a leaf that is itself a symlink is rejected outright via
///   `symlink_metadata` (lstat, which does not follow the link). A parent-only
///   guard would wave through a leaf symlink that escapes temp, and the
///   downstream `File::create`/`VACUUM INTO` would follow it.
/// - **CWE-367 (TOCTOU):** the *resolved* path is returned and handed to the
///   engine, so the value that is validated is the value that is opened — the
///   original unresolved path is never used past this point. The lib then opens
///   the destination with `O_NOFOLLOW` to fail atomically if a symlink *leaf* is
///   raced into place between this check and the write.
///
///   **Residual (tracked in #851):** `O_NOFOLLOW` guards only the leaf, so a
///   *parent directory* component swapped to a symlink after this check is still
///   followed by the open. The default dump path's only parent is the temp root
///   (sticky-bit-protected), so exposure is limited to a client-supplied
///   *multi-level* path with an attacker-writable intermediate dir. The airtight
///   fix is fd-relative opens (`openat`/`cap-std`), deferred to #851.
fn validate_dump_path(p: &std::path::Path) -> Result<PathBuf, ErrorData> {
    // Make the client path absolute FIRST, resolving it against the process cwd.
    // `std::path::absolute` is purely lexical — it does NOT touch the filesystem
    // (no canonicalization, no symlink resolution), it just guarantees a parent
    // component exists. Without it, a bare leaf like `"dump.json"` has
    // `parent() == Some("")`, and `canonicalize("")` fails with a confusing
    // "No such file or directory" instead of the intended containment rejection.
    // A cwd-relative path that resolves outside temp is still rejected by the
    // `starts_with` check below — that is the correct outcome.
    let p = std::path::absolute(p)
        .map_err(|e| ValidationError::Other(format!("cannot resolve dump path: {e}")))?;
    let p = p.as_path();

    // Canonicalize the temp root so the containment check compares resolved
    // paths on both sides. Fall back to the raw value if canonicalize fails
    // (e.g. a platform that does not pre-create the temp dir).
    let temp = std::env::temp_dir();
    let canonical_temp = std::fs::canonicalize(&temp).unwrap_or(temp);

    // Reject a leaf that is itself a symlink. `symlink_metadata` (lstat) does
    // NOT follow the link, so this distinguishes a malicious leaf symlink
    // (which a later `File::create`/`VACUUM INTO` would follow out of the jail)
    // from a benign regular file. A non-existent leaf is fine — the common case.
    match std::fs::symlink_metadata(p) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(
                ValidationError::Other("dump path must not be a symlink".to_owned()).into(),
            );
        }
        Ok(_) | Err(_) => {} // regular file / absent → resolve below
    }

    // Resolve the FULL target with parent symlinks collapsed. If the leaf
    // exists, canonicalize the whole path; otherwise canonicalize the parent
    // (resolving any symlinked components) and rejoin the leaf name.
    let resolved = if p.exists() {
        std::fs::canonicalize(p)
            .map_err(|e| ValidationError::Other(format!("cannot resolve dump path: {e}")))?
    } else {
        let parent = p.parent().ok_or_else(|| {
            ValidationError::Other("dump path has no parent directory".to_owned())
        })?;
        let file_name = p
            .file_name()
            .ok_or_else(|| ValidationError::Other("dump path has no file name".to_owned()))?;
        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|e| ValidationError::Other(format!("cannot resolve dump path parent: {e}")))?;
        canonical_parent.join(file_name)
    };

    if !resolved.starts_with(&canonical_temp) {
        return Err(ValidationError::Other(format!(
            "dump path must be within the temp directory ({})",
            canonical_temp.display()
        ))
        .into());
    }

    Ok(resolved)
}

async fn handle_dump_state(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let format_str = get_str(args, "format").unwrap_or_else(|| "json".to_owned());

    let ext = match format_str.as_str() {
        "json" => "json",
        "sqlite" => "db",
        other => {
            return Err(ValidationError::Other(format!("unsupported dump format: {other}")).into());
        }
    };

    let path = match get_str(args, "path") {
        Some(p) => validate_dump_path(&PathBuf::from(p))?,
        None => default_dump_path(&std::env::temp_dir(), ext),
    };

    let dump_format = match format_str.as_str() {
        "json" => DumpFormat::Json(path.clone()),
        "sqlite" => DumpFormat::Sqlite(path.clone()),
        _ => unreachable!(), // validated above
    };

    engine
        .dump_state(&dump_format)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({ "path": path.display().to_string() }))
}

async fn handle_pin_fact(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    engine.pin_fact(fact_id).await.map_err(to_mcp_error)?;

    ok_json(json!({ "fact_id": fact_id, "pinned": true }))
}

async fn handle_unpin_fact(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    engine.unpin_fact(fact_id).await.map_err(to_mcp_error)?;

    ok_json(json!({ "fact_id": fact_id, "pinned": false }))
}

// ---------------------------------------------------------------------------
// P2 tool handlers (debugging / operator)
// ---------------------------------------------------------------------------

fn parse_replay_order(s: &str) -> Result<ReplayOrder, ErrorData> {
    match s {
        "insertion" => Ok(ReplayOrder::InsertionOrder),
        "timestamp" => Ok(ReplayOrder::TimestampOrder),
        other => Err(ErrorData::invalid_params(
            format!("unknown replay order: {other}"),
            None,
        )),
    }
}

async fn handle_replay_events(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(args)?;

    let since = get_datetime(args, "since")?;
    let until = get_datetime(args, "until")?;

    // Ordering validation: when both bounds are provided, since must not exceed until.
    // Either bound may be omitted independently (open-ended range).
    if let (Some(s), Some(u)) = (since, until)
        && s > u
    {
        return Err(ErrorData::invalid_params("since must be <= until", None));
    }

    let id_start = get_i64(args, "id_range_start");
    let id_end = get_i64(args, "id_range_end");
    let id_range = match (id_start, id_end) {
        (Some(s), Some(e)) => {
            if s > e {
                return Err(ErrorData::invalid_params(
                    "id_range_start must be <= id_range_end",
                    None,
                ));
            }
            Some((s, e))
        }
        (None, None) => None,
        _ => {
            return Err(ErrorData::invalid_params(
                "both id_range_start and id_range_end must be provided, or neither",
                None,
            ));
        }
    };

    let session_id = get_str(args, "session_id");
    let event_type = match get_str(args, "event_type") {
        Some(s) => Some(parse_event_type(&s)?),
        None => None,
    };
    // limit semantics (intentional, #319): `0` means UNBOUNDED (no cap), `absent`
    // defaults to a cap of 100. `0` is the explicit opt-out escape hatch for a full
    // replay; it is NOT clamped to a hidden ceiling here. The unbounded path is gated
    // by the caller's own filters (since/until/id_range/session) and downstream
    // streaming, so a deliberate `0` does not itself constitute the trust-boundary
    // DoS surface the array/byte caps above address.
    let limit = match get_usize(args, "limit")? {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(100),
    };
    let upcast = get_bool(args, "upcast").unwrap_or(false);
    let order = match get_str(args, "order") {
        Some(s) => parse_replay_order(&s)?,
        None => ReplayOrder::InsertionOrder,
    };

    let mut filter = ReplayFilter::default();
    filter.since = since;
    filter.until = until;
    filter.id_range = id_range;
    filter.session_id = session_id;
    filter.event_type = event_type;
    filter.limit = limit;
    filter.upcast = upcast;
    filter.order = order;

    let events = engine.replay_events(&filter).await.map_err(to_mcp_error)?;

    let shaped: Vec<Value> = events
        .iter()
        .map(|e| depth::shape_event(e, depth_level, None))
        .collect();

    ok_json(json!({ "events": shaped, "count": shaped.len() }))
}

async fn handle_fact_history(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(args)?;

    let history = engine.fact_history(fact_id).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_fact_history(&history, depth_level)?;

    ok_json(shaped)
}

async fn handle_bootstrap_session(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
) -> Result<CallToolResult, ErrorData> {
    // Borrow the raw JSONL out of `args` — no owned `String` yet. The byte-length
    // cap is enforced on the BORROW so an over-cap payload is rejected *before* the
    // large `.to_owned()` allocation the cap is meant to prevent (Codex #834:1571).
    let jsonl_data = args
        .get("jsonl_data")
        .and_then(Value::as_str)
        .ok_or_else(|| ErrorData::invalid_params("missing jsonl_data", None))?;

    // #267/#355 (CWE-400/770): cap the raw JSONL byte length before `into_bytes()` /
    // `Cursor` feed it to the bootstrap pipeline. 50 MB is generous — far above any
    // realistic Claude Code session log — so it rejects only pathological payloads.
    // (The library-level `max_session_bytes`/`max_entries` of #293 still apply inside
    // the pipeline; this is the outer trust-boundary guard before that work begins.)
    if jsonl_data.len() > MAX_BOOTSTRAP_BYTES {
        return Err(ErrorData::invalid_params(
            format!(
                "jsonl_data too large: {} bytes exceeds the cap of {MAX_BOOTSTRAP_BYTES} bytes",
                jsonl_data.len()
            ),
            None,
        ));
    }
    // Only now — after the cap passes — materialize the owned `String` the pipeline
    // consumes via `into_bytes()` below.
    let jsonl_data = jsonl_data.to_owned();

    let emb = embedder.ok_or_else(|| {
        ErrorData::invalid_params(
            "embedding provider not configured — required for bootstrap_session",
            None,
        )
    })?;

    // Redaction is always on for the live MCP path (#45/#51 — no bypass). The
    // author-seeded denylist is a backfill-time concern (its env var is normally
    // unset here → signatures-only). An UNSET var yields Ok(empty) and is fine to
    // stay silent about; a SET-but-unreadable file is a misconfiguration we
    // surface (warn) while still proceeding signatures-only rather than failing a
    // live bootstrap. Mirrors the CLI, which logs the literal count loudly.
    let denylist = match memory_engine::bootstrap::load_secret_denylist() {
        Ok(d) => {
            if !d.is_empty() {
                tracing::info!(literals = d.len(), "redaction denylist loaded");
            }
            d
        }
        Err(e) => {
            tracing::warn!(error = %e, "denylist file set but unreadable; bootstrap proceeding signatures-only");
            Vec::new()
        }
    };
    let config = BootstrapConfig {
        scope: get_str(args, "scope"),
        max_turns: get_usize(args, "max_turns")?.unwrap_or(0),
        skip_existing: get_bool(args, "skip_existing").unwrap_or(true),
        redact: true,
        denylist,
        // #293 hostile-input caps (max_session_bytes / max_entries): the MCP
        // bootstrap tool exposes no schema for them, so inherit the library
        // defaults. This wires the caps into the `memory_bootstrap` tool — the
        // real untrusted-input entry point — rather than leaving it uncapped.
        ..BootstrapConfig::default()
    };

    let reader = Cursor::new(jsonl_data.into_bytes());
    // The async engine takes the provider and extractor as owned `Arc<dyn _>` (#631 §1.2):
    // the per-session savepoint pipeline runs below the `StorageBackend` seam and clones
    // them into the blocking offload, so both must outlive the borrowed handler scope.
    let provider = Arc::clone(emb) as Arc<dyn EmbeddingProvider>;
    let extractor: Arc<dyn memory_engine::bootstrap::SessionExtractor> = Arc::new(KeywordExtractor);

    let report = engine
        .bootstrap_session(reader, provider, extractor, &config, None)
        .await
        .map_err(to_mcp_error)?;

    let value = serde_json::to_value(&report)
        .map_err(|e| ErrorData::internal_error(format!("serialize report: {e}"), None))?;
    ok_json(value)
}

// ---------------------------------------------------------------------------
// Phase 5a: Outcome tracking handlers
// ---------------------------------------------------------------------------

/// Parse an MCP `outcome` tool parameter into the core [`Outcome`].
///
/// Delegates to core's canonical [`Outcome::from_str`] (the single source of
/// truth, #353/#678), so casing is reconciled across surfaces: the JSON schema
/// advertises `PascalCase` (`"Positive"`), but lowercase is also accepted.
fn parse_outcome(s: &str) -> Result<Outcome, ErrorData> {
    s.parse().map_err(|_| {
        ErrorData::invalid_params(
            format!("invalid outcome: {s} (expected Positive, Negative, or Neutral)"),
            None,
        )
    })
}

async fn handle_record_outcome(
    args: &Map<String, Value>,
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

async fn handle_outcome_counts(
    args: &Map<String, Value>,
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

// ---------------------------------------------------------------------------
// Activity stream + session lifecycle (#224)
// ---------------------------------------------------------------------------

async fn handle_record_activity(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
    filter_config: &memory_engine::ActivityFilterConfig,
) -> Result<CallToolResult, ErrorData> {
    let tool_name =
        get_str(args, "tool").ok_or_else(|| ErrorData::invalid_params("missing tool", None))?;
    let session_id = get_str(args, "session_id")
        .ok_or_else(|| ErrorData::invalid_params("missing session_id", None))?;
    let tool_args = args.get("args").cloned().unwrap_or_else(|| json!({}));
    let result_summary = get_str(args, "result");
    let timestamp = get_datetime(args, "timestamp")?.unwrap_or_else(Utc::now);
    let scope = get_str(args, "scope");
    // `OutcomeClass::from_str` is infallible (the open `Other` arm captures any
    // value), so an arbitrary JSON string maps cleanly; `None` defers to the
    // engine's `OutcomeClass::Success` default.
    let outcome_class = get_str(args, "outcome_class").map(|s| {
        let Ok(class) = s.parse::<memory_engine::OutcomeClass>();
        class
    });

    let req = memory_engine::RecordActivityRequest {
        tool_name,
        args: tool_args,
        result: result_summary,
        session_id,
        timestamp,
        scope_path: scope,
        outcome_class,
    };

    // The async engine takes an owned `Option<Arc<dyn EmbeddingProvider>>` (#631 §1.2):
    // promotion-to-fact embeds via the provider on the blocking offload, so it must own
    // a clone rather than borrow the handler's reference.
    let provider = embedder.map(|e| Arc::clone(e) as Arc<dyn EmbeddingProvider>);
    let result = engine
        .record_activity(&req, provider, filter_config)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "activity_id": result.activity_id,
        "was_deduplicated": result.was_deduplicated,
        "promoted_fact_id": result.promoted_fact_id,
        "status": result.status.to_string(),
    }))
}

async fn handle_checkpoint_session(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let session_id = get_str(args, "session_id")
        .ok_or_else(|| ErrorData::invalid_params("missing session_id", None))?;
    let scope = get_str(args, "scope");
    let summary = get_str(args, "summary");
    let metadata = args.get("metadata").cloned();

    engine
        .checkpoint_session(&session_id, scope.as_deref(), summary.as_deref(), metadata)
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "session_id": session_id,
        "checkpointed": true,
    }))
}

async fn handle_load_context(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let scope =
        get_str(args, "scope").ok_or_else(|| ErrorData::invalid_params("missing scope", None))?;
    let activity_limit = get_usize(args, "activity_limit")?.unwrap_or(20);
    let fact_limit = get_usize(args, "fact_limit")?.unwrap_or(10);
    let depth_level = get_depth(args)?;

    let ctx = engine
        .load_context(&scope, activity_limit, fact_limit)
        .await
        .map_err(to_mcp_error)?;

    ok_json(depth::shape_project_context(&ctx, depth_level))
}

// ---------------------------------------------------------------------------
// Phase 5a: Cognitive pipeline (dream cycle) handlers (#225)
// ---------------------------------------------------------------------------

/// Run the dream-cycle pipeline, optionally applying the report (default: apply).
async fn handle_dream_cycle(
    args: &Map<String, Value>,
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
async fn handle_apply_cycle_report(
    args: &Map<String, Value>,
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
async fn handle_get_recent_insights(
    args: &Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let project_path = get_str(args, "project_path")
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

#[cfg(test)]
mod tests {
    use super::{
        default_dump_name, default_dump_path, get_datetime, get_usize, parse_consolidate_config,
        parse_embedding, parse_event_type, parse_fact_type, parse_outcome, require_f64_if_present,
        validate_dump_path,
    };
    use memory_engine::types::{EventType, FactType, Outcome};
    use serde_json::json;
    use std::collections::HashSet;

    /// Build an argument map carrying only an `embedding` value.
    fn emb_args(embedding: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("embedding".to_owned(), embedding);
        m
    }

    #[test]
    fn parse_embedding_absent_is_none() {
        let m = serde_json::Map::new();
        assert!(parse_embedding(&m, 8).unwrap().is_none());
    }

    #[test]
    fn parse_embedding_null_is_none() {
        let args = emb_args(json!(null));
        assert!(parse_embedding(&args, 8).unwrap().is_none());
    }

    #[test]
    fn parse_embedding_correct_length_round_trips() {
        // A correctly-sized array deserializes verbatim.
        let v: Vec<f32> = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7].to_vec();
        let args = emb_args(json!(v));
        let got = parse_embedding(&args, 8).unwrap().expect("present");
        assert_eq!(got.len(), 8);
        for (a, b) in got.iter().zip(v.iter()) {
            assert!((a - b).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn parse_embedding_rejects_wrong_length() {
        // #294: a wrong-length array is rejected on its *length* BEFORE any
        // `Vec<f32>` materialization (pre-alloc DoS guard, CWE-400/770). This
        // test only verifies *that* a wrong-length array is rejected with an
        // informative message; the BEFORE-alloc ordering is enforced by the code
        // (length check precedes the `Vec` build) and documented there.
        let args = emb_args(json!(vec![0.1_f32; 16]));
        let err = parse_embedding(&args, 8).unwrap_err();
        assert!(
            err.message.contains("expected 8") && err.message.contains("got 16"),
            "error should name expected vs got: {}",
            err.message
        );
    }

    #[test]
    fn parse_embedding_non_array_rejected() {
        // A present-but-non-array value is malformed input, not a silent None.
        let args = emb_args(json!("not-an-array"));
        assert!(parse_embedding(&args, 8).is_err());
    }

    #[test]
    fn parse_embedding_non_numeric_element_rejected() {
        // #317: a correctly-sized array whose elements are not all numeric passes
        // the length gate but fails the `Vec<f32>` deserialization — it must be a
        // typed error, not a silent coercion.
        let args = emb_args(json!([0.0, "oops", 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]));
        let err = parse_embedding(&args, 8).unwrap_err();
        assert!(err.message.contains("invalid embedding"), "{}", err.message);
    }

    // --- parse_event_type (#317) ---

    #[test]
    fn parse_event_type_accepts_known_variants() {
        assert_eq!(
            parse_event_type("Interaction").unwrap(),
            EventType::Interaction
        );
        assert_eq!(parse_event_type("ToolCall").unwrap(), EventType::ToolCall);
        assert_eq!(parse_event_type("MemoryOp").unwrap(), EventType::MemoryOp);
        assert_eq!(
            parse_event_type("SystemEvent").unwrap(),
            EventType::SystemEvent
        );
    }

    // `parse_event_type_rejects_unknown_preserving_token` lives in the #353 block above.

    // --- parse_outcome (#317) ---

    #[test]
    fn parse_outcome_accepts_known_variants() {
        assert_eq!(parse_outcome("Positive").unwrap(), Outcome::Positive);
        assert_eq!(parse_outcome("Negative").unwrap(), Outcome::Negative);
        assert_eq!(parse_outcome("Neutral").unwrap(), Outcome::Neutral);
    }

    // `parse_outcome_rejects_unknown_preserving_token` lives in the #353 block above.

    // --- get_datetime (#317) ---

    #[test]
    fn get_datetime_absent_is_ok_none() {
        let args = cfg_args(&[]);
        assert!(get_datetime(&args, "timestamp").unwrap().is_none());
    }

    #[test]
    fn get_datetime_valid_iso_round_trips() {
        let args = cfg_args(&[("timestamp", json!("2026-06-26T12:00:00Z"))]);
        let dt = get_datetime(&args, "timestamp").unwrap().expect("present");
        assert_eq!(dt.to_rfc3339(), "2026-06-26T12:00:00+00:00");
    }

    #[test]
    fn get_datetime_malformed_non_empty_rejected() {
        // A present, non-empty, but unparseable ISO string is malformed input — it
        // must surface as an invalid-params error rather than silently defaulting.
        let args = cfg_args(&[("timestamp", json!("not-a-timestamp"))]);
        let err = get_datetime(&args, "timestamp").unwrap_err();
        assert!(err.message.contains("invalid timestamp"), "{}", err.message);
    }

    // --- require_f64_if_present (#317) ---

    #[test]
    fn require_f64_if_present_absent_is_ok_none() {
        let args = cfg_args(&[]);
        assert!(
            require_f64_if_present(&args, "half_life_days")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn require_f64_if_present_numeric_value_passes() {
        let args = cfg_args(&[("half_life_days", json!(42.0))]);
        let v = require_f64_if_present(&args, "half_life_days").unwrap();
        assert_eq!(v, Some(42.0));
    }

    #[test]
    fn require_f64_if_present_string_value_rejected() {
        // The exact regression this guard exists for: a numeric-looking *string*
        // (`"1"`) must be rejected, not silently coerced or dropped to the default.
        let args = cfg_args(&[("half_life_days", json!("1"))]);
        let err = require_f64_if_present(&args, "half_life_days").unwrap_err();
        assert!(
            err.message.contains("half_life_days must be a number"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_fact_type_accepts_schema_pascal_case() {
        // The JSON schemas advertise PascalCase tokens — these must still parse.
        assert_eq!(parse_fact_type("Episodic").unwrap(), FactType::Episodic);
        assert_eq!(parse_fact_type("Semantic").unwrap(), FactType::Semantic);
        assert_eq!(parse_fact_type("Procedural").unwrap(), FactType::Procedural);
    }

    #[test]
    fn parse_fact_type_reconciles_cli_snake_case() {
        // After delegating to core's canonical FromStr, the MCP surface also
        // accepts the CLI's snake_case casing (#678 reconciliation).
        assert_eq!(parse_fact_type("episodic").unwrap(), FactType::Episodic);
        assert_eq!(parse_fact_type("semantic").unwrap(), FactType::Semantic);
    }

    #[test]
    fn parse_fact_type_rejects_unknown_preserving_token() {
        let err = parse_fact_type("wisdom").unwrap_err();
        // ValidationError is a thiserror enum; the offending token is preserved
        // in its Display string.
        assert!(err.to_string().contains("wisdom"), "{err}");
    }

    #[test]
    fn parse_event_type_accepts_schema_pascal_case() {
        // The JSON schemas advertise these four PascalCase tokens — all must parse.
        assert_eq!(
            parse_event_type("Interaction").unwrap(),
            EventType::Interaction
        );
        assert_eq!(parse_event_type("ToolCall").unwrap(), EventType::ToolCall);
        assert_eq!(parse_event_type("MemoryOp").unwrap(), EventType::MemoryOp);
        assert_eq!(
            parse_event_type("SystemEvent").unwrap(),
            EventType::SystemEvent
        );
    }

    #[test]
    fn parse_event_type_reconciles_snake_case() {
        // After delegating to core's canonical FromStr, the MCP surface also
        // accepts the snake_case casing (#353/#678 reconciliation).
        assert_eq!(parse_event_type("tool_call").unwrap(), EventType::ToolCall);
        assert_eq!(
            parse_event_type("interaction").unwrap(),
            EventType::Interaction
        );
    }

    #[test]
    fn parse_event_type_rejects_outcome_signal() {
        // OutcomeSignal is a system-generated event, deliberately omitted from the
        // ingest/replay JSON schemas. Even though core's FromStr parses it, the MCP
        // boundary must keep rejecting it (with the token preserved).
        let err = parse_event_type("OutcomeSignal").unwrap_err();
        assert!(err.to_string().contains("OutcomeSignal"), "{err}");
    }

    #[test]
    fn parse_event_type_rejects_unknown_preserving_token() {
        let err = parse_event_type("WisdomOp").unwrap_err();
        assert!(err.to_string().contains("WisdomOp"), "{err}");
    }

    #[test]
    fn parse_outcome_accepts_schema_pascal_case() {
        // The JSON schema advertises PascalCase tokens — these must parse.
        assert_eq!(parse_outcome("Positive").unwrap(), Outcome::Positive);
        assert_eq!(parse_outcome("Negative").unwrap(), Outcome::Negative);
        assert_eq!(parse_outcome("Neutral").unwrap(), Outcome::Neutral);
    }

    #[test]
    fn parse_outcome_reconciles_lowercase() {
        // After delegating to core's canonical FromStr, lowercase also parses.
        assert_eq!(parse_outcome("positive").unwrap(), Outcome::Positive);
        assert_eq!(parse_outcome("neutral").unwrap(), Outcome::Neutral);
    }

    #[test]
    fn parse_outcome_rejects_unknown_preserving_token() {
        let err = parse_outcome("mixed").unwrap_err();
        // ErrorData carries the offending token in its message.
        assert!(err.message.contains("mixed"), "{}", err.message);
    }

    #[test]
    fn get_usize_rejects_negative_value() {
        // #339: a present-but-negative integer must be an ERROR, not silently
        // dropped (which would let the engine apply its own default and return
        // more results than the untrusted caller asked for).
        let err = get_usize(&cfg_args(&[("limit", json!(-1))]), "limit").unwrap_err();
        assert!(
            err.message.contains("limit must be a non-negative integer"),
            "{}",
            err.message
        );
    }

    #[test]
    fn get_usize_accepts_non_negative_value() {
        let v = get_usize(&cfg_args(&[("limit", json!(7))]), "limit").unwrap();
        assert_eq!(v, Some(7));

        // Zero is a valid non-negative usize (callers ascribe their own meaning,
        // e.g. replay's 0 = "no limit").
        let z = get_usize(&cfg_args(&[("limit", json!(0))]), "limit").unwrap();
        assert_eq!(z, Some(0));
    }

    #[test]
    fn get_usize_absent_key_is_ok_none() {
        // Absent must be distinguished from present-but-invalid: Ok(None), so the
        // caller can fall back to its default.
        let v = get_usize(&cfg_args(&[]), "limit").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn parse_consolidate_config_rejects_negative_min_cluster_size() {
        // Dispatch-level (#339): a negative `min_cluster_size` routed through a
        // real handler config-parse path must surface as an invalid-params error,
        // not be silently coerced to the default.
        let err =
            parse_consolidate_config(&cfg_args(&[("min_cluster_size", json!(-5))])).unwrap_err();
        assert!(
            err.message
                .contains("min_cluster_size must be a non-negative integer"),
            "{}",
            err.message
        );
    }

    /// Build a `memory_consolidate` argument map from key/value pairs.
    fn cfg_args(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn parse_consolidate_config_rejects_out_of_range_dedup_threshold() {
        let err =
            parse_consolidate_config(&cfg_args(&[("dedup_threshold", json!(2.0))])).unwrap_err();
        assert!(
            err.message
                .contains("dedup_threshold must be in [0.0, 1.0]"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_rejects_out_of_range_cluster_threshold() {
        // #344: this is the path the provider-less integration test could not reach.
        let err =
            parse_consolidate_config(&cfg_args(&[("cluster_threshold", json!(2.0))])).unwrap_err();
        assert!(
            err.message
                .contains("cluster_threshold must be in [0.0, 1.0]"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_rejects_tiny_min_cluster_size() {
        let err =
            parse_consolidate_config(&cfg_args(&[("min_cluster_size", json!(1))])).unwrap_err();
        assert!(
            err.message.contains("min_cluster_size must be >= 2"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_rejects_wrong_type_threshold() {
        // A present-but-wrong-type value must be REJECTED, not silently defaulted
        // (gemini + codex review): `"0.95"` (string) is not a number.
        let err = parse_consolidate_config(&cfg_args(&[("cluster_threshold", json!("0.95"))]))
            .unwrap_err();
        assert!(
            err.message.contains("cluster_threshold must be a number"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_consolidate_config_applies_defaults_and_overrides() {
        // Empty args → MCP-level defaults (dedup 0.92, cluster 0.85, min 3).
        let cfg = parse_consolidate_config(&cfg_args(&[])).unwrap();
        assert!((cfg.dedup_threshold - 0.92).abs() < f32::EPSILON);
        assert!((cfg.cluster_threshold - 0.85).abs() < f32::EPSILON);
        assert_eq!(cfg.min_cluster_size, 3);

        // Provided values flow through to the config.
        let cfg = parse_consolidate_config(&cfg_args(&[
            ("dedup_threshold", json!(0.7)),
            ("cluster_threshold", json!(0.6)),
            ("min_cluster_size", json!(5)),
        ]))
        .unwrap();
        assert!((cfg.dedup_threshold - 0.7).abs() < f32::EPSILON);
        assert!((cfg.cluster_threshold - 0.6).abs() < f32::EPSILON);
        assert_eq!(cfg.min_cluster_size, 5);
    }

    /// Regression for #546: with a *frozen* timestamp and pid, the only thing
    /// that can keep default dump names distinct is the process-global atomic
    /// counter (`seq`). This pins the clock so the test isolates the counter as
    /// the load-bearing collision guard — it fails the moment `seq` is dropped
    /// from the filename, even on a host with a fine-grained clock that would
    /// otherwise mask the regression by advancing the nanosecond timestamp
    /// between calls.
    #[test]
    fn default_dump_names_are_distinguished_by_seq_alone() {
        // Constant timestamp + pid: zero entropy from the clock or process id.
        let frozen_ts = "20260616T000000000000000";
        let frozen_pid = 4242_u32;

        let n = 1024_u64;
        let names: HashSet<_> = (0..n)
            .map(|seq| default_dump_name(frozen_ts, frozen_pid, seq, "json"))
            .collect();

        assert_eq!(
            names.len() as u64,
            n,
            "names collided with frozen ts+pid: {} unique of {n} \
             (the atomic seq counter is not making paths distinct)",
            names.len()
        );

        // Every seq in 0..n must be present exactly once, proving the counter —
        // not the timestamp — supplies the distinctness.
        for seq in 0..n {
            let expected = format!("memory-dump-{frozen_ts}-{frozen_pid}-{seq}.json");
            assert!(
                names.contains(&expected),
                "missing seq segment {seq}: {expected}"
            );
        }
    }

    /// End-to-end smoke check that the live `default_dump_path` (real clock,
    /// real pid, real atomic) produces well-formed, base-rooted, unique paths.
    /// Uniqueness here may be aided by the clock — the discriminating guarantee
    /// is proven by `default_dump_names_are_distinguished_by_seq_alone`.
    #[test]
    fn default_dump_paths_are_unique_within_process() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let n = 1024;
        let paths: HashSet<_> = (0..n).map(|_| default_dump_path(base, "json")).collect();

        assert_eq!(
            paths.len(),
            n,
            "default dump paths collided: {} unique of {n}",
            paths.len()
        );
        for p in &paths {
            assert!(p.starts_with(base));
            assert_eq!(p.extension().and_then(|e| e.to_str()), Some("json"));
            let name = p.file_name().and_then(|n| n.to_str()).unwrap();
            assert!(name.starts_with("memory-dump-"), "unexpected name: {name}");
        }
    }

    // --- Property-based tests (#471) ---

    proptest::proptest! {
        /// Round-trip: any fixed-dimension `Vec<f32>` of finite values serializes to
        /// a JSON array, parses back through `parse_embedding(args, D)`, and equals
        /// the input bit-for-bit. serde_json widens each f32 to f64 for the JSON text
        /// and narrows on the way back; that round-trip is exact for every finite f32
        /// (f64 represents all f32 values losslessly), so a strict equality check is
        /// the right contract. Non-finite values are excluded — serde_json cannot
        /// represent NaN/±inf as JSON numbers (they would serialize to null).
        #[test]
        fn parse_embedding_round_trips_fixed_dim(
            v in proptest::collection::vec(proptest::num::f32::NORMAL | proptest::num::f32::ZERO | proptest::num::f32::SUBNORMAL, 8..=8)
        ) {
            let args = emb_args(json!(v));
            let got = parse_embedding(&args, 8)
                .expect("a finite, correctly-sized array must parse")
                .expect("present");
            proptest::prop_assert_eq!(got, v);
        }
    }

    /// Finding-1 regression (Gemini #836): a client path with no directory
    /// component (a bare leaf such as `"dump.json"`) must NOT trip the confusing
    /// `canonicalize("")` parent failure. `validate_dump_path` makes the path
    /// absolute against cwd *first* (`std::path::absolute`, purely lexical), so a
    /// bare leaf gains a real parent and is then judged by the *containment*
    /// check — accepted when cwd is inside temp, rejected (with the temp-dir
    /// error) when it is not. Both branches are asserted under one `set_current_dir`
    /// guard because cwd is process-global; this is the only test that mutates it.
    #[test]
    fn validate_dump_path_handles_bare_relative_leaf() {
        // Canonicalize temp so the asserted prefix matches `validate_dump_path`'s
        // own canonical comparison (e.g. macOS `/tmp -> /private/tmp`).
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let outside = std::env::current_dir().expect("cargo runs tests from the crate dir");
        debug_assert!(
            !outside.starts_with(&temp),
            "test precondition: the crate dir must be outside temp"
        );

        let saved = std::env::current_dir().ok();

        // (a) cwd OUTSIDE temp → the bare leaf resolves outside the jail and is
        //     rejected by containment, with the temp-directory error (NOT a
        //     parent-resolution error).
        std::env::set_current_dir(&outside).unwrap();
        let rejected = validate_dump_path(std::path::Path::new("dump.json"));

        // (b) cwd INSIDE temp → the same bare leaf resolves into the jail and is
        //     accepted, resolving to a path under temp.
        std::env::set_current_dir(&temp).unwrap();
        let accepted = validate_dump_path(std::path::Path::new("relative-dump.json"));

        if let Some(prev) = saved {
            let _ = std::env::set_current_dir(prev);
        }

        let err = rejected.expect_err("a bare leaf under a non-temp cwd must be rejected");
        assert!(
            err.message.contains("temp"),
            "a relative leaf must be rejected by the temp-containment check, not a \
             parent-resolution error; got: {}",
            err.message
        );

        let resolved = accepted.expect("a relative leaf resolving into temp must be accepted");
        assert!(
            resolved.starts_with(&temp),
            "resolved path {} must be inside temp {}",
            resolved.display(),
            temp.display()
        );
    }
}
