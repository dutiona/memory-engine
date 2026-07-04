//! # me-backend-sqlite — sub-PRs 2a + 2b (Wave 2 #816 / S2)
//!
//! The `SQLite` persistence backend carved out of the `memory-engine` facade.
//!
//! [`store`] (the per-table `SQLite` accessors + schema migrations) and [`pool`]
//! (the reader/writer connection pool) landed in sub-PR 2a. Sub-PR 2b adds:
//!
//! - [`consolidation`] / [`inspect`] — the SQL-touching halves of the 3-pass pipeline
//!   (`load_snapshot`/`apply_plan`) and the backend-internal inspection functions
//!   (`compute_statistics`/`dump_json`/…). Both are `pub` purely so the facade can
//!   re-export the specific paths it still calls; neither is a stable path for anything
//!   outside this workspace.
//! - [`sqlite`] — [`sqlite::SqliteBackend`], the `StorageBackend` port impl, re-exported
//!   at the crate root (`pub use sqlite::SqliteBackend;`) to match its pre-carve
//!   `memory_engine::storage::SqliteBackend` path via the facade's own re-export.
//! - [`search`] — the FTS5/vector search cores `SqliteBackend` drives (`hybrid`/`query`,
//!   the RRF merge + the `MemoryQuery` API, stay in the facade).
//! - [`snapshot`] — the sidecar `.snapshot` file I/O (`MemoryGraph`/`ScopeTree`/HNSW),
//!   read/written by `sqlite::schema`'s `SchemaManager::write_engine_snapshot` and the
//!   engine's open path.
//!
//! The `archive/` `.pak` format and `engine/graph_load.rs` stay in the facade — out of
//! scope for the `SQLite` backend carve entirely (the former is a filesystem/codec
//! concern, not a port implementation; the latter wires two facade-owned in-memory
//! projections above both `me-index` and this crate).

// Panic-safety gate (#725, workspace lints). This crate's own `#[cfg(test)]` unit
// tests are exempt — a panic there is the intended failure signal.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod consolidation;
pub mod inspect;
pub mod pool;
pub mod search;
pub mod snapshot;
pub mod sqlite;
pub mod store;

pub use sqlite::SqliteBackend;

/// Shared store-level test setup (`setup_memory_db`).
///
/// `#[cfg(test)]`-only: its only two call sites (`store::activities`,
/// `store::checkpoints`) live in this same crate, and nothing outside it needs
/// `NewFact`/`NewEvent` test doubles from here (those stay in
/// `me-types`/`me-traits`, Commit 2). Kept `pub(crate)` for the same reason.
#[cfg(test)]
pub(crate) mod test_util;
