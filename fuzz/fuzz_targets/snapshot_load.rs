//! Fuzz target for the binary snapshot reader (#311).
//!
//! `engine::snapshot::load_from_file` ingests an untrusted sidecar file: it
//! parses a `u32` LE `header_len`, slices `data[4..header_end]`, then runs two
//! `rmp_serde::from_slice` passes (header, then payload) plus a blake3 verify.
//! The on-disk size and in-band bounds are guarded, but the MessagePack
//! deep-nesting / large-allocation paths were never fuzzed. The contract is
//! total: every malformed input must return `None`, never panic.
//!
//! The reader is `pub(crate)`; it is reached through the `#[cfg(fuzzing)]`
//! `memory_engine::fuzz_seam` re-export so this harness does not widen the
//! shipped public API.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(tmp) = tempfile::NamedTempFile::new() else {
        return;
    };
    if std::fs::write(tmp.path(), data).is_err() {
        return;
    }
    // `embed_dim` is fuzzed implicitly: try the common 768 and a mismatching 0
    // so both the dimension-accept and dimension-reject branches are exercised.
    // Both `None` and `Some(..)` are acceptable; the only failure is a panic.
    let _ = memory_engine::fuzz_seam::load_from_file(tmp.path(), 768);
    let _ = memory_engine::fuzz_seam::load_from_file(tmp.path(), 0);
});
