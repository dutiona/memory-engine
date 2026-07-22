//! # me-index — in-memory projections (Wave 2 #816 / S2)
//!
//! Backend-free L2 leaf: the petgraph-backed `MemoryGraph` and the hierarchical
//! `ScopeTree`, rebuilt from `me-types` DTOs by the facade's backend glue. Depends
//! only on `me-types`.

// Panic-safety gate (#725): `unwrap_used = "deny"` (workspace lints) forbids
// `.unwrap()` in production paths, where a panic aborts the *consumer's* process.
// This crate's own `#[cfg(test)]` unit tests are exempt — a panic there is the
// intended failure signal, not a consumer-facing hazard.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod graph;
pub mod scope;

pub use graph::{EdgeData, MemoryGraph};
pub use scope::{ScopeTree, cache_scope_chain};
