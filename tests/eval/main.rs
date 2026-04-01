//! Evaluation harness for memory-engine.
//!
//! Two-tier architecture (LAM Protocol conformance model):
//! - **Tier 1 (Conformance)**: Binary pass/fail invariant tests for system contracts
//! - **Tier 2 (Quality)**: Regression corpus with retrieval quality metrics and gates
//!
//! See issue #16 and `docs/plans/2026-03-09-future-phases-design.md`.

mod conformance;
mod corpus;
mod helpers;
mod metrics;
mod quality;
mod skeletons;
