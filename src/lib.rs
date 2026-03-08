//! # memory-engine
//!
//! Embedded memory engine for autonomous AI agents.
//!
//! Provides 5 core primitives:
//! - **Ingest**: Append events to an immutable log (source of truth)
//! - **Query**: Hybrid retrieval (FTS5 + vector + graph) with temporal filtering
//! - **Consolidate**: Merge, cluster, and integrate memories (dream cycle)
//! - **Forget**: Decay, prune, and archive stale facts
//! - **Resolve**: Bi-temporal conflict arbitration for contradicting facts
//!
//! ## Storage
//!
//! - `SQLite` WAL for event log, facts, and FTS5
//! - `LanceDB` for vector similarity search
//! - In-memory petgraph for relationship traversal
//!
//! ## Consumers
//!
//! Designed for two consumers from a single backend:
//! 1. Autonomous agents (Qwen 3.5 on Mac Mini M4)
//! 2. Developer workflows (Claude Code / IDE hooks)

pub mod types;

pub use types::*;
