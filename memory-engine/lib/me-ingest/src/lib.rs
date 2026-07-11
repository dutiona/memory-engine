//! Ingest primitive: event-log append + fact derivation over `MemoryCtx`.
//!
//! Extracted from the facade in Wave 2 #816 / S4, sub-PR 1. The engine's
//! `MemoryEngine::{ingest, add_fact, add_fact_precomputed, add_facts_batch,
//! add_facts_batch_partial}` are one-line delegates over the free functions here.
