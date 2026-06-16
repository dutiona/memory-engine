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
//! - Optional HNSW approximate nearest-neighbour index (`ann` feature)
//!
//! ## Threading
//!
//! `MemoryEngine` is `Send + Sync`. Thread safety is provided by an internal
//! connection pool (N readers + 1 writer) and `RwLock`-protected caches.
//! Consumers can share via `Arc<MemoryEngine>`.
//!
//! ## Quick start
//!
//! Build an in-memory engine with the typestate [`MemoryEngine::builder`], add a
//! fact, and retrieve it. The engine delegates embedding to a consumer-supplied
//! [`EmbeddingProvider`]; the example wires a tiny deterministic one so it is
//! fully self-contained and runs under `cargo test --doc`.
//!
//! ```
//! use memory_engine::{
//!     AddFactRequest, EmbeddingFingerprint, EmbeddingProvider, FactType, MemoryEngine, MemoryError,
//!     MemoryQuery,
//! };
//!
//! // A deterministic, dependency-free embedder: hash each byte into a fixed-dim
//! // bag-of-bytes vector. Real consumers plug in a model here instead.
//! struct HashEmbedder {
//!     dim: usize,
//! }
//! impl EmbeddingProvider for HashEmbedder {
//!     fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
//!         let mut v = vec![0.0_f32; self.dim];
//!         for &b in text.as_bytes() {
//!             v[b as usize % self.dim] += 1.0;
//!         }
//!         Ok(v)
//!     }
//!     fn fingerprint(&self) -> EmbeddingFingerprint {
//!         EmbeddingFingerprint::new("mock", "test", self.dim)
//!     }
//! }
//!
//! let dim = 64;
//! let engine = MemoryEngine::builder(dim).build()?;
//! let embedder = HashEmbedder { dim };
//!
//! // Ingest a fact (embedding computed via the provider, then persisted).
//! let id = engine.add_fact(
//!     &AddFactRequest {
//!         content: "Rust has no garbage collector".into(),
//!         fact_type: FactType::Semantic,
//!         source_event_id: None,
//!         scope: None,
//!         opts: None,
//!     },
//!     &embedder,
//!     None,
//! )?;
//! assert!(id > 0);
//!
//! // Retrieve it back via full-text search and assert on the result.
//! let response = engine.execute_query(&MemoryQuery::new().text("garbage collector"))?;
//! assert_eq!(response.results.len(), 1);
//! assert_eq!(response.results[0].fact.content, "Rust has no garbage collector");
//! # Ok::<(), MemoryError>(())
//! ```

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
#[cfg(feature = "archive")]
pub(crate) mod archive;
pub(crate) mod conflict;
pub(crate) mod consolidation;
pub(crate) mod forgetting;
pub(crate) mod graph;
pub(crate) mod limits;
pub(crate) mod pool;
pub(crate) mod resume;
pub(crate) mod scope;
pub(crate) mod store;

// === Re-exports: flat access to the most-used consumer types ===
#[cfg(feature = "archive")]
pub use archive::{ArchiveManifestEntry, ArchivePolicy, ArchiveStats, ArchiveVerifyResult};
pub use bootstrap::{BootstrapConfig, BootstrapReport, KeywordExtractor, SessionExtractor};
pub use engine::activity_filter::{ActivityFilterConfig, ActivityFilterDecision, PromoteAction};
pub use engine::builder::{File, InMemory, MemoryEngineBuilder};
pub use engine::{EngineConfig, MemoryEngine};
pub use error::*;
pub use inspect::types as inspect_types;
pub use resume::{ResumeConfig, ResumeContext};
pub use search::{MemoryQuery, QueryDiagnostics, QueryResponse};
pub use store::UpcasterRegistry;
pub use store::schema::CURRENT_SCHEMA_VERSION;
pub use traits::{
    ConflictArbiter, ConflictResolution, ConsolidationConfig, ConsolidationStats, CrudDecision,
    DreamCycle, EmbeddingProvider, ForgetPolicy, InsightStream, PersistenceClassifier, PruneStats,
    Reranker, SummaryGenerator,
};
pub use types::*;

// Phase 5a cognitive pipeline re-exports
pub use engine::cognitive::{DreamContext, INSIGHT_MARKER_KEY};
pub use engine::cycle::{
    ApplyResult, CycleAnomaly, CycleContext, CycleDelta, CycleMetadata, CycleOutcome, CycleReport,
    DefaultDreamCycle, IMPORTANCE_STEP, IdentityOutput, MAX_ADJUSTMENT, SkipReason, TimeWindow,
};

#[cfg(test)]
mod tests {
    /// Smoke-test: verify that re-exported types are reachable from the crate root.
    /// If any re-export is removed or renamed, this test will fail to compile.
    #[test]
    fn reexports_are_accessible() {
        // traits
        fn _accepts_embedding_provider(_: &dyn crate::EmbeddingProvider) {}
        fn _accepts_summary_generator(_: &dyn crate::SummaryGenerator) {}
        fn _accepts_conflict_arbiter(_: &dyn crate::ConflictArbiter) {}
        fn _accepts_persistence_classifier(_: &dyn crate::PersistenceClassifier) {}
        fn _accepts_reranker(_: &dyn crate::Reranker) {}
        fn _accepts_insight_stream(_: &dyn crate::InsightStream) {}
        fn _accepts_dream_cycle(_: &dyn crate::DreamCycle) {}

        // trait types
        let _ = std::mem::size_of::<crate::CrudDecision>();
        let _ = std::mem::size_of::<crate::ConsolidationConfig>();
        let _ = std::mem::size_of::<crate::ConsolidationStats>();
        let _ = std::mem::size_of::<crate::PruneStats>();
        let _ = std::mem::size_of::<crate::ConflictResolution>();
        let _ = std::mem::size_of::<crate::ForgetPolicy>();

        // core types
        let _ = std::mem::size_of::<crate::FactType>();
        let _ = std::mem::size_of::<crate::Fact>();
        let _ = std::mem::size_of::<crate::EngineConfig>();
        let _ = std::mem::size_of::<crate::MemoryEngine>();
    }
}
