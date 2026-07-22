//! # me-types
//!
//! Layer-0 (L0) of the memory-engine crate workspace (Wave 2, #816).
//!
//! The pure data + error vocabulary every other crate depends on — the only crate
//! with no internal (`me-*`) dependencies, so it is the acyclic leaf the layering
//! rests on. It owns three module trees, relocated verbatim from the monolithic core
//! so the cycle-break and the crate-carve stayed independently verifiable:
//! - [`types`] — the domain DTOs (`Fact`, `Event`, `NewFact`, plus the
//!   snapshot / cycle-report / search-result sidecar vocabularies).
//! - [`error`] — the [`MemoryError`](error::MemoryError) umbrella, the typed
//!   sub-enums, and the crate [`Result`](error::Result) alias.
//! - [`limits`] — the workspace-internal resource bounds: ingest payload size caps
//!   enforced during (de)serialization, plus the consolidation dedup/cluster
//!   complexity caps (#983).

// Panic-safety gate (#725): `unwrap_used = "deny"` (workspace lints) forbids
// `.unwrap()` in production paths, where a panic aborts the *consumer's* process.
// This crate's own `#[cfg(test)]` unit tests are exempt — a panic there is the
// intended failure signal, not a consumer-facing hazard.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod error;
pub mod limits;
/// Pure math primitives (`cosine_similarity`), shared across the workspace.
///
/// Wave 2 #816 / S4, sub-PR 3a — relocated from `me-backend-sqlite` so a
/// primitive doesn't force a dependency on a concrete storage backend.
pub mod math;
/// Shared test-only factory helpers (`new_fact`/`new_event` family).
///
/// Gated behind the `test-util` feature (Wave 2 #816, Commit 2 of the
/// `me-backend-sqlite` carve). Every consumer that needs a `NewFact`/`NewEvent`
/// test double depends on this instead of duplicating the factories.
#[cfg(feature = "test-util")]
pub mod test_util;
pub mod types;
