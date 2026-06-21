//! Fuzz target for the JSONL session-log parser (#426).
//!
//! `bootstrap::parse::parse_session_file` is the bootstrap pipeline's first
//! touch on untrusted JSONL (real session logs, or files handed to the MCP
//! bootstrap endpoint). It reads bounded lines, UTF-8-checks each, then
//! `serde_json::from_str::<SessionEntry>`. The per-line / per-stream / per-entry
//! caps are enforced; this drives the parser with adversarial bytes to prove it
//! never panics and always returns a best-effort `(entries, malformed)` pair.
//!
//! The parser is `pub(crate)`; reached via the `#[cfg(fuzzing)]`
//! `memory_engine::fuzz_seam` re-export.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::io::BufReader;

fuzz_target!(|data: &[u8]| {
    // Caps fuzzed at both ends: 0/0 = fully unbounded (worst case for the
    // allocator), plus a small explicit cap to exercise the truncation branch.
    let reader = BufReader::new(data);
    let _ = memory_engine::fuzz_seam::parse_session_file(reader, 0, 0);

    let reader = BufReader::new(data);
    let (_entries, _malformed) = memory_engine::fuzz_seam::parse_session_file(reader, 4096, 16);
});
