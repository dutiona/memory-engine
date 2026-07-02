//! # me-backend-sqlite — sub-PR 2a (Wave 2 #816 / S2)
//!
//! The two clean-leaf `SQLite` persistence modules carved out of the `memory-engine` facade.
//!
//! [`store`] (the per-table `SQLite` accessors + schema migrations) and [`pool`]
//! (the reader/writer connection pool). Depends only on `me-types` (L0) and
//! `me-storage` (L1, for [`me_storage::UpcasterRegistry`]).
//!
//! `storage/sqlite/` (the `StorageBackend` impl that drives these modules),
//! `search/`, the `archive/` `.pak` format, and `engine/{snapshot,graph_load}`
//! stay in the facade — they carve out in a later sub-PR (2b).

// Panic-safety gate (#725, workspace lints). This crate's own `#[cfg(test)]` unit
// tests are exempt — a panic there is the intended failure signal.
#![cfg_attr(test, allow(clippy::unwrap_used))]

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
