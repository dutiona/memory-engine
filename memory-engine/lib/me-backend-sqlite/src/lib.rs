//! # me-backend-sqlite — sub-PRs 2a + 2b (Wave 2 #816 / S2)
//!
//! The `SQLite` persistence leaves carved out of the `memory-engine` facade.
//!
//! [`store`] (the per-table `SQLite` accessors + schema migrations) and [`pool`]
//! (the reader/writer connection pool) landed in sub-PR 2a. Sub-PR 2b adds
//! [`consolidation`] (the SQL-touching halves of the 3-pass pipeline —
//! `load_snapshot`/`apply_plan`) and [`inspect`] (`compute_statistics` +
//! `dump_json`/`dump_sqlite`/…). Both are `pub` purely so the facade can re-export the
//! specific paths it still calls (its own `compute_plan`/orchestration stays put — see
//! `consolidation`'s module docs) — neither is meant as a stable path for anything
//! outside this workspace.
//!
//! `storage/sqlite/` (the `StorageBackend` impl), `search/`, the `archive/` `.pak`
//! format, and `engine/{snapshot,graph_load}` still stay in the facade — `sqlite`/
//! `search`/`snapshot` carve out later in this same sub-PR 2b; `archive`/`graph_load`
//! are out of scope for the `SQLite` backend carve entirely.

// Panic-safety gate (#725, workspace lints). This crate's own `#[cfg(test)]` unit
// tests are exempt — a panic there is the intended failure signal.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod consolidation;
pub mod inspect;
pub mod pool;
pub mod store;

/// Shared store-level test setup (`setup_memory_db`).
///
/// `#[cfg(test)]`-only: its only two call sites (`store::activities`,
/// `store::checkpoints`) live in this same crate, and nothing outside it needs
/// `NewFact`/`NewEvent` test doubles from here (those stay in
/// `me-types`/`me-traits`, Commit 2). Kept `pub(crate)` for the same reason.
#[cfg(test)]
pub(crate) mod test_util;
