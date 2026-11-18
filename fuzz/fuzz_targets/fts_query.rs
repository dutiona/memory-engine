//! Fuzz target for the FTS5 query-text path (#499 `search/testing-no-fuzz-targets`).
//!
//! `search::fts::fts_search` (reached from the engine's hybrid search) binds an
//! untrusted query string into a SQLite FTS5 `MATCH ?` expression. FTS5 has its
//! own mini query grammar (phrases, `NEAR`, column filters, `AND`/`OR`/`NOT`,
//! prefix `*`, quoting), and malformed input surfaces as an FTS5 error when the
//! statement first steps, not at prepare time. The contract is total: every
//! query string must either return rows or be read as no match — never a
//! panic, never a propagated error on a healthy store.
//!
//! This harness feeds arbitrary bytes (lossy-UTF-8-decoded) through the
//! `#[cfg(fuzzing)]` `memory_engine::fuzz_seam::fuzz_fts_query` seam, which owns
//! the in-memory DB setup (the store/schema helpers are `pub(crate)`) so this
//! harness stays a thin `&[u8] -> ()` wrapper and does not widen the public API.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Lossy decode: FTS5 takes UTF-8 text, and the lossy replacement of invalid
    // sequences is itself part of the surface we want to exercise (replacement
    // chars, embedded NULs, lone surrogates collapsed to U+FFFD).
    let query = String::from_utf8_lossy(data);
    memory_engine::fuzz_seam::fuzz_fts_query(&query);
});
