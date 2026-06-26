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
// `restore` is the lone exception: it stays `pub` because the detached `fuzz` crate
// (its own workspace, excluded from `cargo build --workspace`, so CI cannot catch a
// regression) drives `inspect::restore::read_snapshot` directly as its untrusted-FILE
// ingest fuzz target. Narrowing it to `pub(crate)` would silently break that build.
// See `fuzz/fuzz_targets/snapshot_restore.rs` and the `fuzz_seam` note in `lib.rs`.
pub(crate) mod dump;
pub(crate) mod explain;
pub(crate) mod replay;
pub mod restore;
pub(crate) mod statistics;
pub mod types;

pub use types::*;
