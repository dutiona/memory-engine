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
//! - Pure Rust brute-force vector similarity (cosine)
//!
//! ## Threading
//!
//! `MemoryEngine` is `!Send` and `!Sync` (rusqlite `Connection` is not
//! thread-safe). Consumers must wrap in a `Mutex` or use an actor pattern.

pub mod consolidation;
pub mod engine;
pub mod error;
pub mod forgetting;
pub mod graph;
pub mod search;
pub mod store;
pub mod traits;
pub mod types;

pub use engine::{EngineConfig, MemoryEngine};
pub use error::*;
pub use store::{deserialize_embedding, serialize_embedding};
pub use traits::EmbeddingProvider;
pub use types::*;
