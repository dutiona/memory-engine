//! Query primitive: hybrid FTS5 + vector + Reciprocal Rank Fusion retrieval over
//! `MemoryCtx`.
//!
//! Extracted from the facade in Wave 2 #816 / S4, sub-PR 2. The engine's
//! `MemoryEngine::{query, execute_query}` are thin delegates over the free
//! functions here.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod execute;
pub mod hybrid;
pub mod query;

// `execute::query` (the free fn) is deliberately NOT flat-re-exported here: it would
// collide with the `query` module (holding `MemoryQuery`) at the crate root, exactly
// the naming tension the facade's original `search::query`/`engine::query` module
// split existed to avoid. Callers reach it as `me_query::execute::query(...)`.
pub use execute::{QueryExecution, execute_query};
pub use hybrid::{port_hybrid_search, rrf_merge};
pub use query::MemoryQuery;
