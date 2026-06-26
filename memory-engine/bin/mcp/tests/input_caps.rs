//! Trust-boundary input-size cap tests (#266/#267/#355/#294, part of #319/#256).
//!
//! The MCP server materializes untrusted JSON-RPC input. These tests prove the
//! generous caps reject *absurd* inputs early — before any unbounded allocation
//! or engine work — while leaving legitimate large-but-reasonable use untouched.
//!
//! Each test creates an in-memory `MemoryEngine`, bypasses MCP transport, and
//! calls `tools::dispatch()` directly. The caps are checked *before* the
//! embedding-provider requirement in their handlers, so `None` embedder is
//! sufficient to exercise the rejection path.

use memory_engine::MemoryEngine;
use memory_engine_mcp::tools;
use serde_json::{Map, Value, json};

const DIM: usize = 8;

fn make_engine() -> MemoryEngine {
    MemoryEngine::builder(DIM)
        .build()
        .expect("in-memory engine")
}

fn args(pairs: Value) -> Map<String, Value> {
    match pairs {
        Value::Object(m) => m,
        _ => panic!("args() requires a JSON object"),
    }
}

// ---------------------------------------------------------------------------
// #266 — memory_flush_insights: MAX_FLUSH_INSIGHTS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flush_insights_rejects_oversized_array() {
    let engine = make_engine();
    // One past the cap. Each entry is a minimal valid insight object so the
    // *only* reason for rejection is the array-length cap, not entry validity.
    let oversized: Vec<Value> = (0..=tools::MAX_FLUSH_INSIGHTS)
        .map(|i| json!({ "content": format!("insight {i}") }))
        .collect();

    let result = tools::dispatch(
        "memory_flush_insights",
        args(json!({ "insights": oversized })),
        &engine,
        None, // embedder: cap fires before the embedder check
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;

    let err = result.expect_err("oversized insights array must be rejected");
    assert!(
        err.message.contains("insights")
            && err.message.contains(&tools::MAX_FLUSH_INSIGHTS.to_string()),
        "error should name the cap: {}",
        err.message
    );
}

#[tokio::test]
async fn flush_insights_at_cap_passes_size_gate() {
    let engine = make_engine();
    // Exactly at the cap → the size gate must NOT reject. (It then fails later
    // for lack of an embedder; we assert the failure is *not* the cap message.)
    let at_cap: Vec<Value> = (0..tools::MAX_FLUSH_INSIGHTS)
        .map(|i| json!({ "content": format!("insight {i}") }))
        .collect();

    let result = tools::dispatch(
        "memory_flush_insights",
        args(json!({ "insights": at_cap })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;

    let err = result.expect_err("no embedder configured → still an error");
    assert!(
        !err.message.contains(&tools::MAX_FLUSH_INSIGHTS.to_string()),
        "at-cap array must pass the size gate, not trip it: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// #267 + #355 — memory_bootstrap_session: MAX_BOOTSTRAP_BYTES
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bootstrap_session_rejects_oversized_jsonl() {
    let engine = make_engine();
    // Construct a String just past the byte cap cheaply (no per-byte JSON parse).
    let oversized = "x".repeat(tools::MAX_BOOTSTRAP_BYTES + 1);

    let result = tools::dispatch(
        "memory_bootstrap_session",
        args(json!({ "jsonl_data": oversized })),
        &engine,
        None, // embedder: cap fires before the embedder check
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await;

    let err = result.expect_err("oversized jsonl_data must be rejected");
    assert!(
        err.message.contains("jsonl_data") || err.message.contains("too large"),
        "error should reference the bootstrap size cap: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// #319 — memory_replay_events limit=0 semantics (documented as "unbounded")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_events_limit_zero_is_unbounded_not_capped() {
    let engine = make_engine();
    // Seed 105 events — more than the absent-limit default cap of 100 — so the
    // assertion distinguishes "limit=0 is unbounded" from "limit=0 silently fell
    // back to the default 100". With only 3 events both behaviours return 3.
    for i in 0..105 {
        let _ = tools::dispatch(
            "memory_ingest",
            args(json!({
                "event_type": "Interaction",
                "payload": { "n": i },
                "source": "test"
            })),
            &engine,
            None,
            None,
            DIM,
            &memory_engine::ActivityFilterConfig::default(),
        )
        .await
        .expect("ingest");
    }

    // limit=0 must mean "no limit" (unbounded), NOT "return zero rows".
    let result = tools::dispatch(
        "memory_replay_events",
        args(json!({ "limit": 0 })),
        &engine,
        None,
        None,
        DIM,
        &memory_engine::ActivityFilterConfig::default(),
    )
    .await
    .expect("replay should succeed");
    let body = call_result_json(&result);
    assert_eq!(
        body["count"].as_u64().expect("count"),
        105,
        "limit=0 means unbounded: all 105 events return (not capped to the default 100)"
    );
}

/// Extract the JSON body from a successful `CallToolResult`.
fn call_result_json(result: &rmcp::model::CallToolResult) -> Value {
    let content = result.content.first().expect("no content in result");
    let text = content
        .as_text()
        .expect("expected Text content")
        .text
        .as_str();
    serde_json::from_str(text).expect("content is not valid JSON")
}
