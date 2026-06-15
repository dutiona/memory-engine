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
    AddFactOptions, AddFactRequest, EventType, FactType, NewEvent, Outcome,
};
use rmcp::model::{CallToolResult, Content, ErrorData, Tool};
use serde_json::{Map, Value, json};

use crate::depth::{self, Depth};
use crate::embedding::{HttpEmbeddingProvider, PassthroughEmbedder};
use crate::error::{ValidationError, to_mcp_error};

// ---------------------------------------------------------------------------
// Tool definitions (JSON schemas)
// ---------------------------------------------------------------------------

/// Returns all tool definitions (P0, P1, P2, Phase 5) with JSON schemas.
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
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5 },
                    "metadata": { "description": "Arbitrary JSON metadata" },
                    "t_valid": { "type": "string", "format": "date-time", "description": "Real-world validity start (future = scheduled memory)" },
                    "t_invalid": { "type": "string", "format": "date-time", "description": "Real-world validity end" },
                    "pinned": { "type": "boolean", "description": "Make this fact unforgettable" },
                    "embedding": { "type": "array", "items": { "type": "number" }, "description": "Pre-computed embedding (bypasses server-side embedding)" }
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
                    "embedding": { "type": "array", "items": { "type": "number" }, "description": "Pre-computed query embedding" },
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
            "Retrieve tiered cognitive boot context (5-tier: pinned → high-importance → due → recent → kb_stubs).",
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
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "fact_type": { "type": "string", "enum": ["Episodic", "Semantic", "Procedural"], "default": "Semantic" },
                                "scope": { "type": "string" },
                                "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
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
                    "jsonl_data": { "type": "string", "description": "Raw JSONL session log content" },
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
pub fn dispatch(
    name: &str,
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
    summary_gen: Option<&(dyn SummaryGenerator + Send + Sync)>,
    embed_dim: usize,
    filter_config: &memory_engine::ActivityFilterConfig,
) -> Result<CallToolResult, ErrorData> {
    match name {
        "memory_ingest" => handle_ingest(args, engine),
        "memory_add_fact" => handle_add_fact(args, engine, embedder, embed_dim),
        "memory_query" => handle_query(args, engine, embedder),
        "memory_resume_context" => handle_resume_context(args, engine),
        "memory_list_due" => handle_list_due(args, engine),
        "memory_next_due_time" => handle_next_due_time(args, engine),
        "memory_explain_fact" => handle_explain_fact(args, engine),
        "memory_get_fact" => handle_get_fact(args, engine),
        "memory_statistics" => handle_statistics(engine),
        "memory_flush_insights" => handle_flush_insights(args, engine, embedder),
        // P1 tools
        "memory_consolidate" => handle_consolidate(args, engine, embedder, summary_gen),
        "memory_forget" => handle_forget(args, engine),
        "memory_dump_state" => handle_dump_state(args, engine),
        "memory_pin_fact" => handle_pin_fact(args, engine),
        "memory_unpin_fact" => handle_unpin_fact(args, engine),
        // P2 tools
        "memory_replay_events" => handle_replay_events(args, engine),
        "memory_fact_history" => handle_fact_history(args, engine),
        "memory_bootstrap_session" => handle_bootstrap_session(args, engine, embedder),
        // Phase 5a: Outcome tracking
        "memory_record_outcome" => handle_record_outcome(args, engine),
        "memory_outcome_counts" => handle_outcome_counts(args, engine),
        // Activity stream + session lifecycle (#224)
        "memory_record_activity" => handle_record_activity(args, engine, embedder, filter_config),
        "memory_checkpoint_session" => handle_checkpoint_session(args, engine),
        "memory_load_context" => handle_load_context(args, engine),
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

fn get_usize(args: &Map<String, Value>, key: &str) -> Option<usize> {
    get_i64(args, key).and_then(|v| usize::try_from(v).ok())
}

fn get_datetime(args: &Map<String, Value>, key: &str) -> Result<Option<DateTime<Utc>>, ErrorData> {
    match get_str(args, key) {
        Some(s) => s
            .parse::<DateTime<Utc>>()
            .map(Some)
            .map_err(|e| ErrorData::invalid_params(format!("invalid {key}: {e}"), None)),
        None => Ok(None),
    }
}

fn get_depth(args: &Map<String, Value>) -> Result<Depth, ErrorData> {
    match args.get("depth") {
        None | Some(Value::Null) => Ok(Depth::default()),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| ErrorData::invalid_params(format!("invalid depth: {e}"), None)),
    }
}

/// Parse an embedding from a JSON value, returning an error if present but malformed.
fn parse_embedding(args: &Map<String, Value>) -> Result<Option<Vec<f32>>, ErrorData> {
    match args.get("embedding") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value::<Vec<f32>>(v.clone())
            .map(Some)
            .map_err(|e| ErrorData::invalid_params(format!("invalid embedding: {e}"), None)),
    }
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

fn parse_event_type(s: &str) -> Result<EventType, ValidationError> {
    match s {
        "Interaction" => Ok(EventType::Interaction),
        "ToolCall" => Ok(EventType::ToolCall),
        "MemoryOp" => Ok(EventType::MemoryOp),
        "SystemEvent" => Ok(EventType::SystemEvent),
        other => Err(ValidationError::UnknownEventType(other.to_owned())),
    }
}

fn parse_fact_type(s: &str) -> Result<FactType, ValidationError> {
    match s {
        "Episodic" => Ok(FactType::Episodic),
        "Semantic" => Ok(FactType::Semantic),
        "Procedural" => Ok(FactType::Procedural),
        other => Err(ValidationError::UnknownFactType(other.to_owned())),
    }
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

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

fn handle_ingest(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let event_type_str = get_str(&args, "event_type")
        .ok_or_else(|| ErrorData::invalid_params("missing event_type", None))?;
    let event_type = parse_event_type(&event_type_str)?;
    let payload = args.get("payload").cloned().unwrap_or(json!({}));
    let source = get_str(&args, "source")
        .ok_or_else(|| ErrorData::invalid_params("missing source", None))?;
    let session_id = get_str(&args, "session_id");
    let timestamp = get_datetime(&args, "timestamp")?.unwrap_or_else(Utc::now);

    let scope_id = match get_str(&args, "scope") {
        Some(path) => engine.ensure_scope_path(&path).map_err(to_mcp_error)?,
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

    let event_id = engine.ingest(&event).map_err(to_mcp_error)?;
    ok_json(json!({ "event_id": event_id }))
}

fn handle_add_fact(
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
    embed_dim: usize,
) -> Result<CallToolResult, ErrorData> {
    let content = get_str(&args, "content")
        .ok_or_else(|| ErrorData::invalid_params("missing content", None))?;
    let fact_type = match get_str(&args, "fact_type") {
        Some(s) => parse_fact_type(&s)?,
        None => FactType::Semantic,
    };
    let source_event_id = get_i64(&args, "source_event_id");
    let scope = get_str(&args, "scope");

    // Validate importance range
    let importance = get_f64(&args, "importance");
    if let Some(imp) = importance {
        if !(0.0..=1.0).contains(&imp) {
            return Err(ValidationError::ImportanceOutOfRange(imp).into());
        }
    }

    // Validate temporal consistency
    let t_valid = get_datetime(&args, "t_valid")?;
    let t_invalid = get_datetime(&args, "t_invalid")?;
    if let (Some(tv), Some(ti)) = (t_valid, t_invalid) {
        if tv >= ti {
            return Err(ValidationError::TemporalInconsistency.into());
        }
    }

    let pinned = get_bool(&args, "pinned");
    let metadata = args.get("metadata").cloned();

    // Pre-computed embedding or server-side embedding
    let pre_computed = parse_embedding(&args)?;

    if let Some(ref emb) = pre_computed {
        if emb.len() != embed_dim {
            return Err(ValidationError::EmbeddingDimension {
                expected: embed_dim,
                actual: emb.len(),
            }
            .into());
        }
    }

    let req = AddFactRequest {
        content,
        fact_type,
        source_event_id,
        scope,
        opts: Some(AddFactOptions {
            importance,
            metadata,
            t_valid,
            t_invalid,
            pinned,
            ..Default::default()
        }),
    };

    let fact_id = if let Some(emb) = pre_computed {
        let passthrough = PassthroughEmbedder::new(emb);
        engine
            .add_fact(&req, &passthrough, None)
            .map_err(to_mcp_error)?
    } else {
        let emb = embedder.ok_or(ValidationError::NoEmbeddingProvider)?;
        engine.add_fact(&req, emb, None).map_err(to_mcp_error)?
    };

    ok_json(json!({ "fact_id": fact_id }))
}

fn handle_query(
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(&args)?;

    let mut query = memory_engine::MemoryQuery::new();

    // Parse and validate search mode (if explicit)
    let explicit_mode = match get_str(&args, "mode") {
        Some(s) => Some(parse_search_mode(&s)?),
        None => None,
    };

    // Parse embedding (with proper error on malformed input)
    let pre_emb = parse_embedding(&args)?;

    if let Some(text) = get_str(&args, "text") {
        query = query.text(text.clone());

        // Determine effective mode for embedding decision
        let needs_embedding = match explicit_mode {
            Some(SearchMode::Fts) => false,
            Some(SearchMode::Vector | SearchMode::Hybrid) => true,
            None => true, // Default: try to provide embedding for hybrid if possible
        };

        if needs_embedding {
            if let Some(emb) = pre_emb {
                query = query.embedding(emb);
            } else if let Some(emb_provider) = embedder {
                let emb = emb_provider.embed(&text).map_err(to_mcp_error)?;
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
    if let Some(scope) = get_str(&args, "scope") {
        let scope_mode = get_str(&args, "scope_mode").unwrap_or_else(|| "subtree".to_owned());
        query = match scope_mode.as_str() {
            "exact" => query.scope_exact(scope),
            "ancestors" => query.scope_ancestors(scope),
            "inherited" => query.scope_inherited(scope),
            _ => query.scope_subtree(scope),
        };
    }

    // Temporal filters — reject one-sided periods
    let period_start = get_datetime(&args, "period_start")?;
    let period_end = get_datetime(&args, "period_end")?;
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

    if let Some(ft) = get_str(&args, "fact_type") {
        query = query.fact_type(parse_fact_type(&ft)?);
    }
    if let Some(min) = get_f64(&args, "min_importance") {
        query = query.min_importance_score(min);
    }
    if get_bool(&args, "pinned_only").unwrap_or(false) {
        query = query.pinned_only();
    }
    if let Some(limit) = get_usize(&args, "limit") {
        query = query.limit(limit);
    }
    if get_bool(&args, "include_expired_probe").unwrap_or(false) {
        query = query.include_expired_probe();
    }

    let response = engine.execute_query(&query).map_err(to_mcp_error)?;

    let shaped: Vec<Value> = response
        .results
        .iter()
        .map(|r| depth::shape_search_result(r, depth_level, None))
        .collect();

    let diagnostics = depth::shape_diagnostics(&response.diagnostics, depth_level);

    ok_json(json!({ "results": shaped, "count": shaped.len(), "diagnostics": diagnostics }))
}

fn handle_resume_context(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(&args)?;

    let config = ResumeConfig {
        scope_path: get_str(&args, "scope"),
        now: Utc::now(),
        pinned_cap: get_usize(&args, "pinned_cap").unwrap_or(50),
        high_importance_cap: get_usize(&args, "high_importance_cap").unwrap_or(20),
        high_importance_min: get_f64(&args, "high_importance_min").unwrap_or(0.7),
        due_cap: get_usize(&args, "due_cap").unwrap_or(10),
        recent_cap: get_usize(&args, "recent_cap").unwrap_or(10),
    };

    let ctx = engine.resume_context(&config).map_err(to_mcp_error)?;
    let shaped = depth::shape_resume_context(&ctx, depth_level);

    ok_json(shaped)
}

fn handle_list_due(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(&args)?;
    let scope = get_str(&args, "scope");

    let facts = engine
        .list_due(Utc::now(), scope.as_deref())
        .map_err(to_mcp_error)?;

    let shaped: Vec<Value> = facts
        .iter()
        .map(|f| depth::shape_fact(f, depth_level, None))
        .collect();

    ok_json(json!({ "facts": shaped, "count": shaped.len() }))
}

fn handle_next_due_time(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let scope = get_str(&args, "scope");
    let next = engine
        .next_due_time(scope.as_deref())
        .map_err(to_mcp_error)?;

    ok_json(json!({ "next_due": next }))
}

fn handle_explain_fact(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(&args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(&args)?;

    let explanation: FactExplanation = engine.explain_fact(fact_id).map_err(to_mcp_error)?;
    let shaped = depth::shape_explanation(&explanation, depth_level);

    ok_json(shaped)
}

fn handle_get_fact(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(&args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(&args)?;

    let fact = engine.get_fact(fact_id).map_err(to_mcp_error)?;
    let shaped = depth::shape_fact(&fact, depth_level, None);

    ok_json(shaped)
}

fn handle_statistics(engine: &MemoryEngine) -> Result<CallToolResult, ErrorData> {
    let stats = engine.statistics().map_err(to_mcp_error)?;
    let value = serde_json::to_value(&stats)
        .map_err(|e| ErrorData::internal_error(format!("serialize stats: {e}"), None))?;
    ok_json(value)
}

fn handle_flush_insights(
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
) -> Result<CallToolResult, ErrorData> {
    let insights = args
        .get("insights")
        .and_then(Value::as_array)
        .ok_or_else(|| ErrorData::invalid_params("missing insights array", None))?;

    let emb = embedder.ok_or(ErrorData::invalid_params(
        "embedding provider not configured — required for flush_insights",
        None,
    ))?;

    // --- Phase 1: Parse + validate all insights upfront ---
    let mut entries: Vec<AddFactRequest> = Vec::new();
    let mut entry_indices: Vec<usize> = Vec::new(); // original index for each valid entry
    let mut failed: Vec<Value> = Vec::new();

    for (i, insight) in insights.iter().enumerate() {
        let obj = match insight.as_object() {
            Some(o) => o,
            None => {
                failed.push(json!({ "index": i, "error": "not an object" }));
                continue;
            }
        };

        let content = match get_str(obj, "content") {
            Some(c) => c,
            None => {
                failed.push(json!({ "index": i, "error": "missing content" }));
                continue;
            }
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
        let importance = get_f64(obj, "importance");

        if let Some(imp) = importance {
            if !(0.0..=1.0).contains(&imp) {
                failed.push(json!({ "index": i, "error": format!("importance must be in [0.0, 1.0], got {imp}") }));
                continue;
            }
        }

        let mut metadata = obj.get("metadata").cloned().unwrap_or(json!({}));
        if let Value::Object(ref mut m) = metadata {
            m.insert("source".to_owned(), json!("pre_compaction_flush"));
        }

        let opts = AddFactOptions {
            importance,
            metadata: Some(metadata),
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
        match engine.add_facts_batch(&entries, emb, None) {
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

fn handle_consolidate(
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
    summary_gen: Option<&(dyn SummaryGenerator + Send + Sync)>,
) -> Result<CallToolResult, ErrorData> {
    let generator = summary_gen.ok_or(ValidationError::NoSummaryProvider)?;
    // Issue #116: summaries are embedded via the EmbeddingProvider, not the
    // SummaryGenerator, so consolidation now requires an embedder too.
    let embedder = embedder.ok_or(ValidationError::NoEmbeddingProvider)?;

    let dedup_threshold = get_f64(&args, "dedup_threshold").unwrap_or(0.92) as f32;
    if !(0.0..=1.0).contains(&dedup_threshold) {
        return Err(ValidationError::Other(format!(
            "dedup_threshold must be in [0.0, 1.0], got {dedup_threshold}"
        ))
        .into());
    }

    let min_cluster_size = get_usize(&args, "min_cluster_size").unwrap_or(3);
    if min_cluster_size < 2 {
        return Err(ValidationError::Other(format!(
            "min_cluster_size must be >= 2, got {min_cluster_size}"
        ))
        .into());
    }

    let config = ConsolidationConfig {
        dedup_threshold,
        min_cluster_size,
    };

    let stats = engine
        .consolidate(generator, embedder, &config)
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "duplicates_removed": stats.duplicates_removed,
        "clusters_created": stats.clusters_created,
        "global_summaries": stats.global_summaries,
    }))
}

fn handle_forget(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let mut policy = ForgetPolicy::default();

    if let Some(v) = require_f64_if_present(&args, "half_life_days")? {
        policy.half_life_days = v;
    }
    if let Some(v) = require_f64_if_present(&args, "min_importance")? {
        policy.min_importance = v;
    }
    if let Some(v) = require_f64_if_present(&args, "recency_weight")? {
        policy.recency_weight = v;
    }
    if let Some(v) = require_f64_if_present(&args, "frequency_weight")? {
        policy.frequency_weight = v;
    }
    if let Some(v) = require_f64_if_present(&args, "graph_degree_weight")? {
        policy.graph_degree_weight = v;
    }
    if let Some(v) = require_f64_if_present(&args, "base_importance_weight")? {
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

    let stats = engine.forget(&policy).map_err(to_mcp_error)?;

    ok_json(json!({
        "facts_expired": stats.facts_expired,
        "facts_evaluated": stats.facts_evaluated,
    }))
}

fn handle_dump_state(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let format_str = get_str(&args, "format").unwrap_or_else(|| "json".to_owned());

    let ext = match format_str.as_str() {
        "json" => "json",
        "sqlite" => "db",
        other => {
            return Err(ValidationError::Other(format!("unsupported dump format: {other}")).into());
        }
    };

    let path = match get_str(&args, "path") {
        Some(p) => {
            let p = PathBuf::from(p);
            // Security: restrict client-supplied paths to the system temp directory.
            // Without this, an MCP client could overwrite arbitrary files.
            let temp = std::env::temp_dir();
            let canonical = p
                .parent()
                .and_then(|parent| std::fs::canonicalize(parent).ok())
                .unwrap_or_default();
            if !canonical.starts_with(&temp) {
                return Err(ValidationError::Other(format!(
                    "dump path must be within the temp directory ({})",
                    temp.display()
                ))
                .into());
            }
            p
        }
        None => {
            let timestamp = Utc::now().format("%Y%m%dT%H%M%S%3f");
            std::env::temp_dir().join(format!("memory-dump-{timestamp}.{ext}"))
        }
    };

    let dump_format = match format_str.as_str() {
        "json" => DumpFormat::Json(path.clone()),
        "sqlite" => DumpFormat::Sqlite(path.clone()),
        _ => unreachable!(), // validated above
    };

    engine.dump_state(&dump_format).map_err(to_mcp_error)?;

    ok_json(json!({ "path": path.display().to_string() }))
}

fn handle_pin_fact(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(&args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    engine.pin_fact(fact_id).map_err(to_mcp_error)?;

    ok_json(json!({ "fact_id": fact_id, "pinned": true }))
}

fn handle_unpin_fact(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(&args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    engine.unpin_fact(fact_id).map_err(to_mcp_error)?;

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

fn handle_replay_events(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(&args)?;

    let since = get_datetime(&args, "since")?;
    let until = get_datetime(&args, "until")?;

    // Ordering validation: when both bounds are provided, since must not exceed until.
    // Either bound may be omitted independently (open-ended range).
    if let (Some(s), Some(u)) = (since, until) {
        if s > u {
            return Err(ErrorData::invalid_params("since must be <= until", None));
        }
    }

    let id_start = get_i64(&args, "id_range_start");
    let id_end = get_i64(&args, "id_range_end");
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

    let session_id = get_str(&args, "session_id");
    let event_type = match get_str(&args, "event_type") {
        Some(s) => Some(parse_event_type(&s)?),
        None => None,
    };
    // 0 = no limit (unbounded), absent = default cap of 100
    let limit = match get_usize(&args, "limit") {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(100),
    };
    let upcast = get_bool(&args, "upcast").unwrap_or(false);
    let order = match get_str(&args, "order") {
        Some(s) => parse_replay_order(&s)?,
        None => ReplayOrder::InsertionOrder,
    };

    let filter = ReplayFilter {
        since,
        until,
        id_range,
        session_id,
        event_type,
        limit,
        upcast,
        order,
    };

    let events = engine.replay_events(&filter).map_err(to_mcp_error)?;

    let shaped: Vec<Value> = events
        .iter()
        .map(|e| depth::shape_event(e, depth_level, None))
        .collect();

    ok_json(json!({ "events": shaped, "count": shaped.len() }))
}

fn handle_fact_history(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(&args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(&args)?;

    let history = engine.fact_history(fact_id).map_err(to_mcp_error)?;
    let shaped = depth::shape_fact_history(&history, depth_level);

    ok_json(shaped)
}

fn handle_bootstrap_session(
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
) -> Result<CallToolResult, ErrorData> {
    let jsonl_data = get_str(&args, "jsonl_data")
        .ok_or_else(|| ErrorData::invalid_params("missing jsonl_data", None))?;

    let emb = embedder.ok_or(ErrorData::invalid_params(
        "embedding provider not configured — required for bootstrap_session",
        None,
    ))?;

    let config = BootstrapConfig {
        scope: get_str(&args, "scope"),
        max_turns: get_usize(&args, "max_turns").unwrap_or(0),
        skip_existing: get_bool(&args, "skip_existing").unwrap_or(true),
    };

    let reader = Cursor::new(jsonl_data.into_bytes());
    let extractor = KeywordExtractor;

    let report = engine
        .bootstrap_session(reader, emb, &extractor, &config, None)
        .map_err(to_mcp_error)?;

    let value = serde_json::to_value(&report)
        .map_err(|e| ErrorData::internal_error(format!("serialize report: {e}"), None))?;
    ok_json(value)
}

// ---------------------------------------------------------------------------
// Phase 5a: Outcome tracking handlers
// ---------------------------------------------------------------------------

fn parse_outcome(s: &str) -> Result<Outcome, ErrorData> {
    match s {
        "Positive" => Ok(Outcome::Positive),
        "Negative" => Ok(Outcome::Negative),
        "Neutral" => Ok(Outcome::Neutral),
        other => Err(ErrorData::invalid_params(
            format!("invalid outcome: {other} (expected Positive, Negative, or Neutral)"),
            None,
        )),
    }
}

fn handle_record_outcome(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(&args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let outcome_str = get_str(&args, "outcome")
        .ok_or_else(|| ErrorData::invalid_params("missing outcome", None))?;
    let outcome = parse_outcome(&outcome_str)?;

    let event_id = engine
        .record_outcome(fact_id, outcome)
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "event_id": event_id,
        "fact_id": fact_id,
        "outcome": outcome,
    }))
}

fn handle_outcome_counts(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(&args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;

    let counts = engine.get_outcome_counts(fact_id).map_err(to_mcp_error)?;

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

fn handle_record_activity(
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
    filter_config: &memory_engine::ActivityFilterConfig,
) -> Result<CallToolResult, ErrorData> {
    let tool_name =
        get_str(&args, "tool").ok_or_else(|| ErrorData::invalid_params("missing tool", None))?;
    let session_id = get_str(&args, "session_id")
        .ok_or_else(|| ErrorData::invalid_params("missing session_id", None))?;
    let tool_args = args.get("args").cloned().unwrap_or(json!({}));
    let result_summary = get_str(&args, "result");
    let timestamp = get_datetime(&args, "timestamp")?.unwrap_or_else(Utc::now);
    let scope = get_str(&args, "scope");
    let outcome_class = get_str(&args, "outcome_class");

    let req = memory_engine::RecordActivityRequest {
        tool_name,
        args: tool_args,
        result: result_summary,
        session_id,
        timestamp,
        scope_path: scope,
        outcome_class,
    };

    let result = engine
        .record_activity(
            &req,
            embedder.map(|e| e as &dyn EmbeddingProvider),
            filter_config,
        )
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "activity_id": result.activity_id,
        "was_deduplicated": result.was_deduplicated,
        "promoted_fact_id": result.promoted_fact_id,
        "status": result.status.to_string(),
    }))
}

fn handle_checkpoint_session(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let session_id = get_str(&args, "session_id")
        .ok_or_else(|| ErrorData::invalid_params("missing session_id", None))?;
    let scope = get_str(&args, "scope");
    let summary = get_str(&args, "summary");
    let metadata = args.get("metadata").cloned();

    engine
        .checkpoint_session(&session_id, scope.as_deref(), summary.as_deref(), metadata)
        .map_err(to_mcp_error)?;

    ok_json(json!({
        "session_id": session_id,
        "checkpointed": true,
    }))
}

fn handle_load_context(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let scope =
        get_str(&args, "scope").ok_or_else(|| ErrorData::invalid_params("missing scope", None))?;
    let activity_limit = get_usize(&args, "activity_limit").unwrap_or(20);
    let fact_limit = get_usize(&args, "fact_limit").unwrap_or(10);
    let depth_level = get_depth(&args)?;

    let ctx = engine
        .load_context(&scope, activity_limit, fact_limit)
        .map_err(to_mcp_error)?;

    ok_json(depth::shape_project_context(&ctx, depth_level))
}
