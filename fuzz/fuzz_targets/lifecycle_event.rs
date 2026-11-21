//! Fuzz target for `POST /lifecycle/events` (#1025).
//!
//! The route reads an envelope from its headers, the body as raw bytes, and hands both to
//! `MemoryMcpServer::record_lifecycle`: the seam that picks the adapter by `ME-Harness`, reads
//! the key, parses the harness's bytes and maps and applies them. Every byte of it is
//! untrusted. The contract is total: any envelope, project facts and body answer one of
//! 204, 409, 422 or 503, a 204 names its outcome, and nothing panics.
//!
//! The input is `0x00`-separated parts: the five envelope headers (`ME-Event-Id`,
//! `ME-Event-Time`, `ME-Event-Seq`, `ME-Harness`, `ME-Client-Version`), the five project
//! headers, then the body, which is everything after the tenth `0x00` and may hold more. An
//! empty envelope part takes a valid default (and an empty harness is `claude-code`), so a
//! bare body already reaches the adapter and the core; a part that is not a valid header
//! value leaves its header out, which the route answers with a 400 before the seam, so the
//! run ends there (the handler's own order). Each run uses a fresh in-memory engine and no
//! embedder, so no network is reached and a captured memory waits in the queue.
#![no_main]

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use memory_engine::MemoryEngine;
use memory_engine_mcp::auth::{Machine, TokenTable};
use memory_engine_mcp::home::User;
use memory_engine_mcp::lifecycle::Envelope;
use memory_engine_mcp::server::MemoryMcpServer;

const DIM: usize = 8;

/// The envelope headers, then the project headers, in the order the input's parts fill them,
/// each with the value an empty part takes.
const HEADERS: [(&str, &str); 10] = [
    ("me-event-id", "fuzz-1"),
    ("me-event-time", "2026-11-21T09:00:00Z"),
    ("me-event-seq", "1"),
    ("me-harness", "claude-code"),
    ("me-client-version", "0.0.0"),
    ("me-project-root", ""),
    ("me-project-dev-root", ""),
    ("me-project-upstream-url", ""),
    ("me-project-origin-url", ""),
    ("me-project-branch", ""),
];

thread_local! {
    // One current-thread runtime for every iteration: building one per run would dominate the
    // fuzzer's throughput.
    static RT: RefCell<Option<tokio::runtime::Runtime>> = const { RefCell::new(None) };
}

/// The calling machine, authenticated once from ADR-0020 §4's test vector: the token `me_`
/// followed by 32 `0` characters. `Machine` has no public constructor on purpose.
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

fuzz_target!(|data: &[u8]| {
    let mut parts = data.splitn(HEADERS.len() + 1, |&b| b == 0);
    let mut headers = http::HeaderMap::new();
    for (name, default) in HEADERS {
        let part = parts.next().unwrap_or_default();
        let value = if part.is_empty() {
            http::HeaderValue::from_static(default)
        } else if let Ok(value) = http::HeaderValue::from_bytes(part) {
            value
        } else {
            continue;
        };
        if !value.is_empty() {
            headers.append(name, value);
        }
    }
    let body = parts.next().unwrap_or_default();
    // The route's first check: a bad envelope is a 400 and ends the request.
    let Ok(envelope) = Envelope::from_headers(&headers) else {
        return;
    };
    drive(&envelope, &headers, body);
});

fn drive(envelope: &Envelope, headers: &http::HeaderMap, body: &[u8]) {
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
        let server = MemoryMcpServer::new(Arc::new(engine), None, user);
        let answer = rt.block_on(server.record_lifecycle(machine(), envelope, headers, body));
        let status = answer.status.as_u16();
        assert!(
            matches!(status, 204 | 409 | 422 | 503),
            "a status outside ADR-0020 §3's table for this route: {status}"
        );
        assert!(
            (status == 204) == answer.outcome.is_some()
                && answer
                    .outcome
                    .is_none_or(|o| matches!(o, "stored" | "duplicate" | "quarantined")),
            "a 204 names its outcome, and only a 204 does: {answer:?}"
        );
    });
}
