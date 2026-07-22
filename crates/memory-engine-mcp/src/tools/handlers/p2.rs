//! P2 tool handlers (debugging / operator): `replay_events`, `fact_history`,
//! `bootstrap_session`.

use std::io::Cursor;
use std::sync::Arc;

use memory_engine::bootstrap::{BootstrapConfig, KeywordExtractor};
use memory_engine::engine::MemoryEngine;
use memory_engine::inspect_types::{ReplayFilter, ReplayOrder};
use memory_engine::traits::EmbeddingProvider;
use rmcp::model::{CallToolResult, ErrorData};
use serde_json::{Value, json};

use crate::depth;
use crate::embedding::HttpEmbeddingProvider;
use crate::error::to_mcp_error;
use crate::tools::MAX_BOOTSTRAP_BYTES;
use crate::tools::parse::{
    get_bool, get_datetime, get_depth, get_i64, get_str, get_usize, ok_json, parse_event_type,
    parse_replay_order,
};

pub async fn handle_replay_events(
    args: &serde_json::Map<String, Value>,
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

pub async fn handle_fact_history(
    args: &serde_json::Map<String, Value>,
    engine: &MemoryEngine,
) -> Result<CallToolResult, ErrorData> {
    let fact_id = get_i64(args, "fact_id")
        .ok_or_else(|| ErrorData::invalid_params("missing fact_id", None))?;
    let depth_level = get_depth(args)?;

    let history = engine.fact_history(fact_id).await.map_err(to_mcp_error)?;
    let shaped = depth::shape_fact_history(&history, depth_level)?;

    ok_json(shaped)
}

pub async fn handle_bootstrap_session(
    args: &serde_json::Map<String, Value>,
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
