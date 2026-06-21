//! Fuzz target for the binary snapshot reader (#311).
//!
//! `engine::snapshot::load_from_file` ingests an untrusted sidecar file: it
//! parses a `u32` LE `header_len`, slices `data[4..header_end]`, then runs two
//! `rmp_serde::from_slice` passes (header, then payload) plus a blake3 verify.
//! The on-disk size and in-band bounds are guarded, but the MessagePack
//! deep-nesting / large-allocation paths were never fuzzed. The contract is
//! total: every malformed input must return `None`, never panic.
//!
//! The reader is crate-internal; it is reached through the `#[cfg(fuzzing)]`
//! `memory_engine::fuzz_seam` re-export so this harness does not widen the
//! shipped public API.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;

thread_local! {
    // Reuse one temp file across iterations and overwrite its contents:
    // creating + deleting a NamedTempFile every run is heavy disk I/O that
    // throttles fuzz throughput.
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

        // (1) Raw bytes: exercises the OUTER envelope parse — size gate,
        // header_len bounds, header MessagePack deserialize, and the
        // checksum-reject path. `embed_dim` fuzzed implicitly (768 vs a
        // mismatching 0). Both None and Some(..) are fine; only a panic fails.
        // fs::write truncates, so each iteration fully replaces the contents.
        if std::fs::write(path, data).is_ok() {
            let _ = memory_engine::fuzz_seam::load_from_file(path, 768);
            let _ = memory_engine::fuzz_seam::load_from_file(path, 0);
        }

        // (2) Wrapped bytes: a valid header + a blake3 recomputed over `data`,
        // so the checksum gate PASSES and `data` reaches the payload MessagePack
        // deserializer (the deep-nesting / large-allocation paths #311 targets,
        // which the raw path can never reach — guessing a 256-bit hash is
        // infeasible). embed_dim 768 matches the header the wrapper writes.
        let envelope = memory_engine::fuzz_seam::fuzz_wrap_payload(data, 768);
        if std::fs::write(path, &envelope).is_ok() {
            let _ = memory_engine::fuzz_seam::load_from_file(path, 768);
        }
    });
});
