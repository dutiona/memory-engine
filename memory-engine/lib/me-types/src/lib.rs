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
//! - [`limits`] — size caps enforced during (de)serialization.

// Panic-safety gate (#725): `unwrap_used = "deny"` (workspace lints) forbids
// `.unwrap()` in production paths, where a panic aborts the *consumer's* process.
// This crate's own `#[cfg(test)]` unit tests are exempt — a panic there is the
// intended failure signal, not a consumer-facing hazard.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod error;
pub mod limits;
pub mod types;
