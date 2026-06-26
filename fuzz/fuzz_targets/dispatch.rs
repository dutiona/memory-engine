//! Fuzz target for the MCP tool dispatcher (#321).
//!
//! `memory_engine_mcp::tools::dispatch` is the MCP server's primary
//! untrusted-input entry point: every JSON-RPC `tools/call` lands here as a
//! `(name, args)` pair that the server hands straight to the matching handler.
//! The handlers do all the argument parsing/validation (`get_str`, `get_i64`,
//! `parse_embedding`, `parse_declared_fingerprint`, datetime/importance/range
//! checks, …) against a `serde_json::Map` the caller fully controls. The
//! contract is total: any name + any argument object must resolve to
//! `Ok(CallToolResult)` or `Err(ErrorData)` — never a panic (a panic in an
//! embedded handler aborts the *consumer's* process, per the crate's
//! no-`unwrap`/no-`unsafe` posture).
//!
//! `dispatch` is already `pub`, so no `#[cfg(fuzzing)]` seam is needed: the
//! harness drives the real public entry point. It is `async`, so each iteration
//! is driven on a reused current-thread Tokio runtime. Read-side handlers run
//! against a fresh in-memory engine; write-side handlers that require an
//! embedding provider / summary generator short-circuit on the `None` we pass
//! (their validation still runs first, which is the surface we want).
#![no_main]

use libfuzzer_sys::fuzz_target;
use memory_engine::{ActivityFilterConfig, MemoryEngine};
use memory_engine_mcp::tools;
use serde_json::{Map, Value};
use std::cell::RefCell;

/// Small embedding dimension — keeps engine setup cheap and lets a fuzzed
/// `embedding` array of length 8 slip past the pre-allocation length gate
/// (`parse_embedding`) into the deeper deserialize/identity-check paths.
const DIM: usize = 8;

/// Every tool name `dispatch` routes (plus one off-list name to drive the
/// `unknown tool` arm). The first input byte selects which name to invoke, so
/// the fuzzer can reach every handler's validation path rather than just one.
const TOOL_NAMES: &[&str] = &[
    "memory_ingest",
    "memory_add_fact",
    "memory_query",
    "memory_resume_context",
    "memory_list_due",
    "memory_next_due_time",
    "memory_explain_fact",
    "memory_get_fact",
    "memory_statistics",
    "memory_flush_insights",
    "memory_consolidate",
    "memory_forget",
    "memory_dump_state",
    "memory_pin_fact",
    "memory_unpin_fact",
    "memory_replay_events",
    "memory_fact_history",
    "memory_bootstrap_session",
    "memory_record_outcome",
    "memory_outcome_counts",
    "memory_record_activity",
    "memory_checkpoint_session",
    "memory_load_context",
    "memory_dream_cycle",
    "memory_apply_cycle_report",
    "memory_get_recent_insights",
    "memory_unknown_tool_name",
];

thread_local! {
    // Reuse a single current-thread runtime across iterations: building a Tokio
    // runtime per run is pure overhead that would dominate fuzz throughput.
    static RT: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
}

fuzz_target!(|data: &[u8]| {
    // First byte picks the tool name; the rest is the argument object. An empty
    // input still drives `memory_ingest` (index 0) with empty args.
    let (selector, json_bytes) = data
        .split_first()
        .map_or((0u8, data), |(b, rest)| (*b, rest));
    let name = TOOL_NAMES[selector as usize % TOOL_NAMES.len()];

    // Interpret the remainder as a JSON value; only an Object is a valid args map
    // (the MCP framework only ever hands `dispatch` an object). Anything else —
    // non-UTF-8, non-JSON, or a non-object JSON value — is uninteresting input.
    let Ok(value) = serde_json::from_slice::<Value>(json_bytes) else {
        return;
    };
    let Value::Object(args) = value else {
        return;
    };

    drive(name, args);
});

/// Build a fresh in-memory engine and dispatch one tool call on the reused
/// runtime. A fresh engine per run keeps iterations independent (a write from a
/// prior input can't shift a later input's path) and an in-memory SQLite store
/// makes construction cheap enough not to bottleneck the fuzzer.
fn drive(name: &str, args: Map<String, Value>) {
    RT.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            *slot = Some(rt);
        }
        let rt = slot.as_ref().expect("runtime initialized above");

        let Ok(engine) = MemoryEngine::builder(DIM).build() else {
            return;
        };

        // No embedder / no summary generator: write handlers that need them
        // reject *after* running their argument validation, which is exactly the
        // untrusted-parse surface this target exercises. The result (Ok or Err)
        // is discarded; only a panic constitutes a finding.
        rt.block_on(async {
            let _ = tools::dispatch(
                name,
                args,
                &engine,
                None,
                None,
                DIM,
                &ActivityFilterConfig::default(),
            )
            .await;
        });
    });
}
