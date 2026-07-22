//! P0 tool handlers: `ingest`, `add_fact`, `query`, `resume_context`,
//! `list_due`, `next_due_time`, `explain_fact`, `get_fact`, `statistics`,
//! `flush_insights`.

use std::sync::Arc;

use chrono::Utc;
use memory_engine::INSIGHT_MARKER_KEY;
use memory_engine::ResumeConfig;
use memory_engine::SearchMode;
use memory_engine::engine::MemoryEngine;
use memory_engine::inspect_types::FactExplanation;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType, NewEvent};
use rmcp::model::{CallToolResult, ErrorData};
use serde_json::{Value, json};

use crate::depth;
use crate::embedding::HttpEmbeddingProvider;
use crate::error::{ValidationError, to_mcp_error};
use crate::tools::MAX_FLUSH_INSIGHTS;
use crate::tools::parse::{
    get_bool, get_depth, get_f64, get_i64, get_str, get_usize, ok_json, parse_declared_fingerprint,
    parse_embedding, parse_event_type, parse_fact_type, parse_search_mode,
};

pub async fn handle_ingest(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let event_type_str = get_str(args, "event_type")?
        .ok_or_else(|| ErrorData::invalid_params("missing event_type", None))?;
    let event_type = parse_event_type(&event_type_str)?;
    let payload = args.get("payload").cloned().unwrap_or_else(|| json!({}));
    let source = get_str(args, "source")?
        .ok_or_else(|| ErrorData::invalid_params("missing source", None))?;
    let session_id = get_str(args, "session_id")?;
    let timestamp = crate::tools::parse::get_datetime(args, "timestamp")?.unwrap_or_else(Utc::now);

    let scope_id = match get_str(args, "scope")? {
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

pub async fn handle_add_fact(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
    embedder: Option<&Arc<HttpEmbeddingProvider>>,
    embed_dim: usize,
) -> Result<CallToolResult, ErrorData> {
    let content = get_str(args, "content")?
        .ok_or_else(|| ErrorData::invalid_params("missing content", None))?;
    let fact_type = match get_str(args, "fact_type")? {
        Some(s) => parse_fact_type(&s)?,
        None => FactType::Semantic,
    };
    let source_event_id = get_i64(args, "source_event_id")?;
    let scope = get_str(args, "scope")?;

    // Validate importance range
    let importance = get_f64(args, "base_importance")?;
    if let Some(imp) = importance
        && !(0.0..=1.0).contains(&imp)
    {
        return Err(ValidationError::ImportanceOutOfRange(imp).into());
    }

    // Validate temporal consistency
    let t_valid = crate::tools::parse::get_datetime(args, "t_valid")?;
    let t_invalid = crate::tools::parse::get_datetime(args, "t_invalid")?;
    if let (Some(tv), Some(ti)) = (t_valid, t_invalid)
        && tv >= ti
    {
        return Err(ValidationError::TemporalInconsistency.into());
    }

    let pinned = get_bool(args, "pinned")?;
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
pub async fn handle_query(
    args: &serde_json::Map<String, Value>,
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
    let explicit_mode = match get_str(args, "mode")? {
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

    if let Some(text) = get_str(args, "text")? {
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
    if let Some(scope) = get_str(args, "scope")? {
        let scope_mode = get_str(args, "scope_mode")?.unwrap_or_else(|| "subtree".to_owned());
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
    let period_start = crate::tools::parse::get_datetime(args, "period_start")?;
    let period_end = crate::tools::parse::get_datetime(args, "period_end")?;
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

    if let Some(ft) = get_str(args, "fact_type")? {
        query = query.fact_type(parse_fact_type(&ft)?);
    }
    if let Some(min) = get_f64(args, "min_importance")? {
        query = query.min_importance_score(min);
    }
    if get_bool(args, "pinned_only")?.unwrap_or(false) {
        query = query.pinned_only();
    }
    if let Some(limit) = limit {
        query = query.limit(limit);
    }
    if get_bool(args, "include_expired_probe")?.unwrap_or(false) {
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

pub async fn handle_resume_context(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(args)?;

    let config = ResumeConfig {
        scope_path: get_str(args, "scope")?,
        now: Some(Utc::now()),
        pinned_cap: get_usize(args, "pinned_cap")?.unwrap_or(50),
        high_importance_cap: get_usize(args, "high_importance_cap")?.unwrap_or(20),
        high_importance_min: get_f64(args, "high_importance_min")?.unwrap_or(0.7),
        due_cap: get_usize(args, "due_cap")?.unwrap_or(10),
        recent_cap: get_usize(args, "recent_cap")?.unwrap_or(10),
    };

    let ctx = engine.resume_context(&config).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_resume_context(&ctx, depth_level);

    ok_json(shaped)
}

pub async fn handle_list_due(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let depth_level = get_depth(args)?;
    let scope = get_str(args, "scope")?;

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

pub async fn handle_next_due_time(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let scope = get_str(args, "scope")?;
    let next = engine
        .next_due_time(scope.as_deref())
        .await
        .map_err(to_mcp_error)?;

    ok_json(json!({ "next_due": next }))
}

pub async fn handle_explain_fact(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")?
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(args)?;

    let explanation: FactExplanation = engine.explain_fact(fact_id).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_explanation(&explanation, depth_level)?;

    ok_json(shaped)
}

pub async fn handle_get_fact(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")?
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(args)?;

    let fact = engine.get_fact(fact_id).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_fact(&fact, depth_level, None);

    ok_json(shaped)
}

pub async fn handle_statistics(engine: &MemoryEngine) -> Result<CallToolResult, ErrorData> {
    let stats = engine.statistics().await.map_err(to_mcp_error)?;
    let value = serde_json::to_value(&stats)
        .map_err(|e| ErrorData::internal_error(format!("serialize stats: {e}"), None))?;
    ok_json(value)
}

pub async fn handle_flush_insights(
    args: &serde_json::Map<String, Value>,
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

        let Some(content) = get_str(obj, "content")? else {
            failed.push(json!({ "index": i, "error": "missing content" }));
            continue;
        };

        let fact_type = match get_str(obj, "fact_type")? {
            Some(s) => match parse_fact_type(&s) {
                Ok(ft) => ft,
                Err(e) => {
                    failed.push(json!({ "index": i, "error": e.to_string() }));
                    continue;
                }
            },
            None => FactType::Semantic,
        };

        let scope = get_str(obj, "scope")?;
        let importance = get_f64(obj, "base_importance")?;

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
