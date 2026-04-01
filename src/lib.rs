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

// === Public modules (consumer-facing API) ===
#[cfg(feature = "async")]
pub mod async_engine;
pub mod bootstrap;
pub mod engine;
pub mod error;
pub mod inspect;
pub mod search;
pub mod traits;
pub mod types;

// === Internal modules (implementation details) ===
pub(crate) mod conflict;
pub(crate) mod consolidation;
pub(crate) mod forgetting;
pub(crate) mod graph;
pub(crate) mod pool;
pub(crate) mod resume;
pub(crate) mod scope;
pub(crate) mod store;

// === Re-exports: flat access to the most-used consumer types ===
pub use bootstrap::{BootstrapConfig, BootstrapReport, KeywordExtractor, SessionExtractor};
pub use engine::{EngineConfig, MemoryEngine};
pub use error::*;
pub use inspect::types as inspect_types;
pub use resume::{ResumeConfig, ResumeContext};
pub use search::{MemoryQuery, QueryDiagnostics, QueryResponse};
pub use store::UpcasterRegistry;
pub use traits::{EmbeddingProvider, Reranker};
pub use types::*;
