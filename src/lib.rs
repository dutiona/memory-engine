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
//! `MemoryEngine` is `Send + Sync`. Thread safety is provided by an internal
//! connection pool (N readers + 1 writer) and `RwLock`-protected caches.
//! Consumers can share via `Arc<MemoryEngine>`.

#[cfg(feature = "async")]
pub mod async_engine;
pub mod bootstrap;
pub mod conflict;
pub mod consolidation;
pub mod engine;
pub mod error;
pub mod forgetting;
pub mod graph;
pub mod inspect;
pub mod pool;
pub mod resume;
pub mod scope;
pub mod search;
pub mod store;
pub mod traits;
pub mod types;

pub use engine::{EngineConfig, MemoryEngine};
pub use inspect::types as inspect_types;
pub use error::*;
pub use search::MemoryQuery;
pub use store::{deserialize_embedding, serialize_embedding, UpcasterRegistry};
pub use bootstrap::{BootstrapConfig, BootstrapReport, KeywordExtractor, SessionExtractor};
pub use traits::EmbeddingProvider;
pub use types::*;
