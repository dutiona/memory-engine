//! Fuzz target for the MCP tool dispatcher (#321).
//!
//! `MemoryMcpServer::dispatch` is the server's primary untrusted-input entry point: every
//! JSON-RPC `tools/call` lands there as a `(name, args)` pair for the authenticated machine,
//! with the project facts its `ME-Project-*` headers carried, decoded by
//! `ProjectFacts::from_headers`, which this target drives too.
//! The model tools parse their arguments into closed structs and answer every failure as a
//! tool result; any other name is a JSON-RPC error. The contract is total: any name and any
//! argument object resolve to `Ok` or `Err`, never a panic.
//!
//! Each run uses a fresh in-memory engine and no embedder, so `remember` refuses after
//! validating its arguments and `recall` runs full-text only: exactly the untrusted-parse
//! surface. Each run also seeds one fact at the user level, so `set_level` reaches its move
//! (a fuzzed key's project creation included) for the fact's id 1. No tool writes outside
//! the store (no tool takes a path).
#![no_main]

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use memory_engine::MemoryEngine;
use memory_engine_mcp::auth::{Machine, TokenTable};
use memory_engine_mcp::home::User;
use memory_engine_mcp::project::ProjectFacts;
use memory_engine_mcp::server::MemoryMcpServer;
use serde_json::{Map, Value};

const DIM: usize = 8;

/// The model tools, one removed tool and one unknown name, picked by the first input byte.
const TOOL_NAMES: &[&str] = &[
    "recall",
    "remember",
    "set_level",
    "memory_forget",
    "memory_unknown_tool_name",
];

thread_local! {
    // Reuse a single current-thread runtime across iterations: building a Tokio
    // runtime per run is pure overhead that would dominate fuzz throughput.
    static RT: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
}

/// The calling machine, authenticated once from ADR-0020 §4's test vector: the token
/// `me_` followed by 32 `0` characters. `Machine` has no public constructor on purpose.
fn machine() -> &'static Machine {
    static MACHINE: OnceLock<Machine> = OnceLock::new();
    MACHINE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("tokens");
        std::fs::write(
            &file,
            "fuzz f7ca4ebc8eb419b37dea09411716d9aa5506b9e5572f1a5e8b3170e815ee13aa\n",
        )
        .expect("token file");
        let table = TokenTable::load(&file).expect("the test vector parses");
        let token = format!("Bearer me_{}", "0".repeat(32));
        table
            .authenticate(token.as_bytes())
            .expect("the test vector authenticates")
            .clone()
    })
}

/// The project headers, in the order the input's `0x00`-separated parts fill them.
const FACT_HEADERS: [&str; 5] = [
    "me-project-root",
    "me-project-dev-root",
    "me-project-upstream-url",
    "me-project-origin-url",
    "me-project-branch",
];

fuzz_target!(|data: &[u8]| {
    // First byte picks the tool name. The rest splits on `0x00`, which no header value and no
    // JSON text holds: the last part is the argument object, the others fill the five project
    // headers in order, cycling, so a sixth part repeats one; a part that is not a valid
    // header value is left out. An input without `0x00` is only an argument object, as before.
    // An empty argument part still drives the call with empty arguments, so
    // missing-argument validation runs; one that is not a JSON object is skipped (the
    // framework only ever hands `dispatch` an object).
    let (selector, rest) = data
        .split_first()
        .map_or((0u8, data), |(b, rest)| (*b, rest));
    let name = TOOL_NAMES[selector as usize % TOOL_NAMES.len()];
    let mut parts: Vec<&[u8]> = rest.split(|&b| b == 0).collect();
    let json_bytes = parts.pop().unwrap_or_default();
    let mut headers = http::HeaderMap::new();
    for (i, part) in parts.into_iter().enumerate() {
        if let Ok(value) = http::HeaderValue::from_bytes(part) {
            headers.append(FACT_HEADERS[i % FACT_HEADERS.len()], value);
        }
    }
    let facts = ProjectFacts::from_headers(&headers);
    let args = if json_bytes.is_empty() {
        Map::new()
    } else {
        match serde_json::from_slice::<Value>(json_bytes) {
            Ok(Value::Object(args)) => args,
            _ => return,
        }
    };
    drive(name, &facts, args);
});

fn drive(name: &str, facts: &ProjectFacts, args: Map<String, Value>) {
    let machine = machine();
    RT.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            // rt-only is sufficient: every SQLite operation goes through spawn_blocking.
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            *slot = Some(rt);
        }
        let rt = slot.as_ref().expect("runtime initialized above");
        let Ok(engine) = MemoryEngine::builder(DIM).build() else {
            return;
        };
        let user = User::parse("fuzz").expect("a valid user");
        let engine = Arc::new(engine);
        let server = MemoryMcpServer::new(Arc::clone(&engine), None, user);
        rt.block_on(async {
            if seed(&engine).await.is_none() {
                return;
            }
            let _ = server.dispatch(machine, facts, name, args).await;
        });
    });
}

/// One fact at `user:fuzz`, so `set_level` has a memory it may move.
async fn seed(engine: &MemoryEngine) -> Option<i64> {
    let fingerprint = memory_engine::EmbeddingFingerprint::new("fuzz", "m", DIM);
    let request = memory_engine::types::AddFactRequest {
        content: "seeded".to_owned(),
        fact_type: memory_engine::types::FactType::Semantic,
        source_event_id: None,
        scope: Some("user:fuzz".to_owned()),
        opts: None,
    };
    engine
        .add_fact_precomputed(&request, vec![0.5; DIM], &fingerprint, None)
        .await
        .ok()
}
