//! Fuzz target for the cold-storage `.pak` reader (#421).
//!
//! `archive::pak::read_pak` is the untrusted-FILE ingest for archived facts: it
//! opens the file, wraps it in a `zstd::Decoder`, caps the decompressed stream at
//! `MAX_PAK_DECOMPRESSED_SIZE` (4 GiB, the CWE-409 decompression-bomb guard),
//! then `serde_json::from_reader::<ArchivePak>` over the inflated bytes and
//! validates the pak/schema versions. This is a genuinely two-layer parser
//! (zstd frame + JSON), distinct from the `snapshot_restore` target's single
//! `serde_json` magic-sniff path — a fuzzer-discovered valid zstd frame routes
//! through the decoder AND the `ArchivePak` deserializer, and a non-zstd prefix
//! exercises the lazy-decoder error path.
//!
//! `read_pak` is `pub fn` inside the `#[cfg(feature = "archive")] pub(crate) mod
//! archive`, so it is unreachable from this detached crate without the
//! `#[cfg(fuzzing)]` `memory_engine::fuzz_seam` re-export (the fuzz crate enables
//! the `archive` + `compress-zstd` features the seam entry needs). The contract:
//! every input yields `Ok` or `Err`, never a panic.
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
        let _ = memory_engine::fuzz_seam::read_pak(path);
    });
});
