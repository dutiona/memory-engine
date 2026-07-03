//! Shared test utilities for `memory-engine` unit tests.
//!
//! This module is only compiled in `#[cfg(test)]` mode. It provides common
//! test doubles and factory helpers that would otherwise be duplicated across
//! every `mod tests { … }` block.
//!
//! Relocated to `me-types`/`me-traits` (Wave 2 #816, `me-backend-sqlite` carve):
//! the `NewFact`/`NewEvent` factories build only `me-types` DTOs, and
//! `MockEmbedder` implements the `me-traits` `EmbeddingProvider` trait — this
//! module re-exports them so every existing `crate::test_utils::X` call site
//! keeps resolving unchanged. `setup_memory_db` moved with `store/` to
//! `me-backend-sqlite` (Commit 3) — its only two call sites
//! (`store::activities`, `store::checkpoints`) moved with it, so it is not
//! re-exported here.

pub use me_traits::test_util::MockEmbedder;
pub use me_types::test_util::{new_event, new_fact, new_fact_hashed, new_fact_with_type};
