//! Inspection APIs for debugging and observability.
//!
//! Provides introspection into engine state: fact explanations, temporal history,
//! event replay, state dumps, and aggregate statistics.

// Implementation submodules are crate-internal: their free functions accept a raw
// `&Connection` and carry no lock discipline, so reaching them from outside would
// bypass the engine's `with_read`/`write_conn` pool protocol (a lock-safety hazard).
// The public seam is the `StorageBackend` port (`storage::schema::{statistics,
// dump_state}`) and the engine's own inspection methods, which own the locking.
//
// `restore` is also crate-internal — `restore_snapshot_into(&Connection)` takes a raw
// connection and bypasses the engine's lock discipline exactly like the other
// submodules, so it must not be externally reachable. The detached `fuzz` crate (its
// own workspace, excluded from `cargo build --workspace`, so CI cannot catch a
// regression) needs only `read_snapshot` as its untrusted-FILE ingest target; that one
// function is re-exported through the `#[cfg(fuzzing)] fuzz_seam` in `lib.rs` rather
// than by widening the whole module. See `fuzz/fuzz_targets/snapshot_restore.rs`.
//
// `dump`/`statistics` relocated to `me-backend-sqlite` (Wave 2 #816 / S2, sub-PR 2b):
// `compute_statistics`/`dump_json`/… are backend-internal — their one caller,
// `sqlite::schema`'s `SchemaManager` impl, moved there too in this same sub-PR (the big
// `storage::sqlite` move), so no re-export is needed here at all (no facade ref remains,
// test or production — `engine/inspect.rs`'s tests reach `MemoryEngine::statistics()`/
// `dump_state()`, the port methods, never these free functions directly).
pub(crate) mod explain;
pub(crate) mod replay;
pub(crate) mod restore;
pub mod types;

pub use types::*;
