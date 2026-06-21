//! Fuzz target for the JSON snapshot import path (#445).
//!
//! `inspect::restore::read_snapshot` is the real untrusted-FILE ingest for the
//! `Deserialize` core types: it size-guards the file, sniffs compression from
//! magic bytes (plain / gzip / zstd), then `serde_json::from_reader::<
//! EngineSnapshot>`. `EngineSnapshot` transitively contains `Vec<Fact>` /
//! `Vec<Event>` / `Vec<Edge>` / etc, so this exercises the whole graph of core
//! deserializers through the genuine entry point (the #445 re-triage's
//! higher-value anchor than a bare `from_str::<Fact>`). `read_snapshot` is
//! already `pub`, so no seam is needed.
//!
//! Built with `compress-gzip` + `compress-zstd`, so a fuzzer-discovered gzip or
//! zstd magic prefix routes through the decoder branches as well as the plain
//! path. The contract: every input yields `Ok` or `Err`, never a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;

thread_local! {
    // Reuse one temp file across iterations (overwrite contents) — per-run
    // file create/delete is heavy disk I/O that throttles fuzz throughput.
    static TMP: RefCell<Option<tempfile::NamedTempFile>> = const { RefCell::new(None) };
}

fuzz_target!(|data: &[u8]| {
    TMP.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let Ok(t) = tempfile::NamedTempFile::new() else {
                return;
            };
            *slot = Some(t);
        }
        let path = slot.as_ref().unwrap().path();
        // fs::write truncates, so each iteration fully replaces the contents.
        if std::fs::write(path, data).is_err() {
            return;
        }
        let _ = memory_engine::inspect::restore::read_snapshot(path);
    });
});
