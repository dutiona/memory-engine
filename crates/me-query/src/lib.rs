//! Query primitive: hybrid FTS5 + vector + Reciprocal Rank Fusion retrieval over
//! `MemoryCtx`.
//!
//! Extracted from the facade in Wave 2 #816 / S4, sub-PR 2. The engine's
//! `MemoryEngine::{query, execute_query}` are thin delegates over the free
//! functions here.
//!
//! `MemoryQuery` moved down to `me-types` in sub-PR 3a (it is a pure data +
//! builder DTO with zero `me-query`-internal dependencies — its sibling search
//! vocabulary already lived in `me-types::types::search`, and the L3-to-L3 edge
//! it forced onto the forthcoming `me-archive` crate was illegal). Re-exported
//! below so `me_query::MemoryQuery` keeps resolving unchanged for every existing
//! caller (the facade, this crate's own `execute.rs`, and downstream tests).
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod execute;
pub mod hybrid;

pub use execute::{QueryExecution, execute_query};
pub use hybrid::{port_hybrid_search, rrf_merge};
/// Compatibility alias. `MemoryQuery` now lives in `me-types` (L0) — it is shared query
/// vocabulary, consumed by more than one primitive (`me-archive` searches `.pak`s against
/// it), so keeping it here would have forced an illegal L3→L3 edge. Re-exported so
/// `me_query::MemoryQuery` keeps resolving for existing callers; new code should prefer
/// the canonical `me_types::types::search::MemoryQuery`.
pub use me_types::types::search::MemoryQuery;
