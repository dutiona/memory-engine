use std::sync::Arc;

use chrono::{DateTime, Utc};
use memory_engine::engine::MemoryEngine;
use memory_engine::inspect_types::FactExplanation;
use memory_engine::resume::ResumeConfig;
use memory_engine::search::hybrid::SearchMode;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, EventType, FactType, NewEvent};
use rmcp::model::{CallToolResult, Content, ErrorData, Tool};
use serde_json::{Map, Value, json};

use crate::depth::{self, Depth};
use crate::embedding::{HttpEmbeddingProvider, PassthroughEmbedder};
use crate::error::{ValidationError, to_mcp_error};

// ---------------------------------------------------------------------------
// Tool definitions (JSON schemas)
// ---------------------------------------------------------------------------

/// Returns all P0 tool definitions with JSON schemas.
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
    embed_dim: usize,
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

fn get_depth(args: &Map<String, Value>) -> Depth {
    get_str(args, "depth")
        .and_then(|s| match s.as_str() {
            "sparse" => Some(Depth::Sparse),
            "standard" => Some(Depth::Standard),
            "full" => Some(Depth::Full),
            _ => None,
        })
        .unwrap_or_default()
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

    let opts = AddFactOptions {
        importance,
        metadata,
        t_valid,
        t_invalid,
        pinned,
        ..Default::default()
    };

    let fact_id = if let Some(emb) = pre_computed {
        let passthrough = PassthroughEmbedder::new(emb);
        engine
            .add_fact(
                &content,
                fact_type,
                source_event_id,
                &passthrough,
                scope.as_deref(),
                Some(&opts),
                None,
            )
            .map_err(to_mcp_error)?
    } else {
        let emb = embedder.ok_or(ValidationError::NoEmbeddingProvider)?;
        engine
            .add_fact(
                &content,
                fact_type,
                source_event_id,
                emb,
                scope.as_deref(),
                Some(&opts),
                None,
            )
            .map_err(to_mcp_error)?
    };

    ok_json(json!({ "fact_id": fact_id }))
}

fn handle_query(
    args: Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&HttpEmbeddingProvider>,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(&args);

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
            } else if explicit_mode.is_some() {
                // User explicitly asked for vector/hybrid but no embedder available
                return Err(ErrorData::invalid_params(
                    format!(
                        "mode '{:?}' requires an embedding provider or pre-computed embedding",
                        explicit_mode.unwrap()
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

    let results = engine.execute_query(&query).map_err(to_mcp_error)?;

    let shaped: Vec<Value> = results
        .iter()
        .map(|r| depth::shape_search_result(r, depth_level, None))
        .collect();

    ok_json(json!({ "results": shaped, "count": shaped.len() }))
}

fn handle_resume_context(
    args: Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(&args);

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
    let depth_level = get_depth(&args);
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
    let depth_level = get_depth(&args);

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
    let depth_level = get_depth(&args);

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

    let mut fact_ids = Vec::new();
    let mut failed = Vec::new();

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

        match engine.add_fact(
            &content,
            fact_type,
            None,
            emb,
            scope.as_deref(),
            Some(&opts),
            None,
        ) {
            Ok(id) => fact_ids.push(id),
            Err(e) => {
                failed.push(json!({ "index": i, "error": e.to_string() }));
            }
        }
    }

    ok_json(json!({
        "fact_ids": fact_ids,
        "added": fact_ids.len(),
        "failed": failed,
        "failed_count": failed.len(),
    }))
}
