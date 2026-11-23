//! Fuzz target for `POST /backfill/uploads` (#1120).
//!
//! The route reads an envelope from its headers, the body as raw bytes, and hands both to
//! `MemoryMcpServer::import_upload`: the seam that reads the upload with the harness's reader,
//! redacts it against the denylist, and imports it under the capture contract — as a dry run
//! or as the write of a reviewed dry run. Every byte of it is untrusted. The contract is
//! total: any envelope, project facts and body answer one of 200, 409, 412, 422 or 503 with a
//! JSON report, an `error` member exactly when the status is neither 200 nor 409, and nothing
//! panics.
//!
//! The input is `0x00`-separated parts: the five envelope headers (`ME-Backfill-Kind`,
//! `ME-Backfill-Mode`, `ME-Backfill-Source`, `ME-Backfill-Mtime`, `ME-Backfill-Preview`),
//! `ME-Client-Version`, the five project headers, then the body, which is everything after
//! the eleventh `0x00` and may hold more. `ME-Harness` is always `claude-code`. An empty part
//! takes its default; a part that is not a valid header value leaves its header out, which
//! the route answers with a 400 before the seam, so the run ends there (the handler's own
//! order). Each run uses a fresh in-memory engine and no embedder, so no network is reached.
//! A dry run that answers 200 is replayed as a write of its own preview, on the same server:
//! the review-then-write path the route exists for.
#![no_main]

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use memory_engine::MemoryEngine;
use memory_engine::types::lifecycle::BatchMode;
use memory_engine_mcp::auth::{Machine, TokenTable};
use memory_engine_mcp::backfill::UploadEnvelope;
use memory_engine_mcp::home::User;
use memory_engine_mcp::server::MemoryMcpServer;
use serde_json::Value;

const DIM: usize = 8;

/// The envelope headers, then the project headers, in the order the input's parts fill them,
/// each with the value an empty part takes.
const HEADERS: [(&str, &str); 11] = [
    ("me-backfill-kind", "transcript"),
    ("me-backfill-mode", "dry-run"),
    ("me-backfill-source", "projects/-work-fixture-p/s.jsonl"),
    ("me-backfill-mtime", "2026-11-21T09:00:00Z"),
    ("me-backfill-preview", ""),
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
    headers.append("me-harness", http::HeaderValue::from_static("claude-code"));
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
    let Ok(envelope) = UploadEnvelope::from_headers(&headers) else {
        return;
    };
    drive(&envelope, &headers, body);
});

fn drive(envelope: &UploadEnvelope, headers: &http::HeaderMap, body: &[u8]) {
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
        let server = MemoryMcpServer::new(Arc::new(engine), None, user)
            .with_denylist(vec!["fixture".to_owned(), "fuzz".to_owned()]);
        let write = matches!(envelope.mode, BatchMode::Write { .. });
        let raw = body; // the upload bytes, kept for the replay
        let (status, body) = rt.block_on(server.import_upload(machine(), envelope, headers, raw));
        check(write, status.as_u16(), &body);
        // A dry run that answers 200 is replayed as a write of its own preview, on the same
        // server: the review-then-write path the route exists for.
        if !write && status.as_u16() == 200 {
            let mut replay = headers.clone();
            replay.insert("me-backfill-mode", "write".parse().unwrap());
            replay.insert(
                "me-backfill-preview",
                body["preview"]
                    .as_str()
                    .expect("a 200 report holds its preview")
                    .parse()
                    .expect("a preview is a header value"),
            );
            let envelope =
                UploadEnvelope::from_headers(&replay).expect("the replay envelope parses");
            let (status, body) =
                rt.block_on(server.import_upload(machine(), &envelope, &replay, raw));
            check(true, status.as_u16(), &body);
            // Nothing changed since the dry run but the receipt time, which the preview never
            // holds: a reviewed write never answers 412 (#1153).
            assert_ne!(
                status.as_u16(),
                412,
                "a replayed dry run lost its preview: {body}"
            );
        }
    });
}

fn check(write: bool, status: u16, body: &Value) {
    assert!(
        match (write, status) {
            (false, 200 | 422 | 503) => true,
            (true, 200 | 409 | 412 | 422 | 503) => true,
            _ => false,
        },
        "a status outside the seam's table for this run: {status}"
    );
    assert!(body.is_object(), "the answer is a JSON object: {body}");
    assert!(
        body.get("error").is_some() == !(status == 200 || status == 409),
        "an `error` member exists exactly when the status is neither 200 nor 409: {status}"
    );
    if status == 200 {
        assert!(
            body.get("committed").and_then(Value::as_bool) == Some(write),
            "a 200's `committed` is true exactly when the run wrote: {status}"
        );
    }
}
