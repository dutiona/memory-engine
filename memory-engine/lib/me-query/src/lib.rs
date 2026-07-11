//! Query primitive: hybrid FTS5 + vector + Reciprocal Rank Fusion retrieval over
//! `MemoryCtx`.
//!
//! Extracted from the facade in Wave 2 #816 / S4, sub-PR 2. The engine's
//! `MemoryEngine::{query, execute_query}` are thin delegates over the free
//! functions here.
#![cfg_attr(test, allow(clippy::unwrap_used))]
