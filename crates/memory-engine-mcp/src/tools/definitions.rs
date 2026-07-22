//! JSON tool schema definitions advertised via the MCP `list_tools` request.

use std::sync::Arc;

use rmcp::model::Tool;
use serde_json::{Map, Value, json};

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
