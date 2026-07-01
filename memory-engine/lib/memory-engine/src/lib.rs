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
//! // The engine API is async (#631); drive it from a tokio runtime. A consumer
//! // binary would use `#[tokio::main]` instead of constructing a `Runtime` here.
//! tokio::runtime::Runtime::new().unwrap().block_on(async {
//!     let dim = 64;
//!     let engine = MemoryEngine::builder(dim).build()?;
//!     let embedder = HashEmbedder { dim };
//!
//!     // Ingest a fact (embedding computed via the provider, then persisted).
//!     let id = engine
//!         .add_fact(
//!             &AddFactRequest {
//!                 content: "Rust has no garbage collector".into(),
//!                 fact_type: FactType::Semantic,
//!                 source_event_id: None,
//!                 scope: None,
//!                 opts: None,
//!             },
//!             std::sync::Arc::new(embedder),
//!             None,
//!         )
//!         .await?;
//!     assert!(id > 0);
//!
//!     // Retrieve it back via full-text search and assert on the result.
//!     let response = engine
//!         .execute_query(&MemoryQuery::new().text("garbage collector"))
//!         .await?;
//!     assert_eq!(response.results.len(), 1);
//!     assert_eq!(response.results[0].fact.content, "Rust has no garbage collector");
//!     Ok::<(), MemoryError>(())
//! })
//! .unwrap();
//! ```

// Panic-safety gate (#725): `unwrap_used = "deny"` (Cargo.toml) forbids
// `.unwrap()` in this library's production paths, where a panic would abort the
// consumer's process. The crate's own `#[cfg(test)]` unit tests are exempt — a
// panic there is the intended failure signal, not a consumer-facing hazard.
#![cfg_attr(test, allow(clippy::unwrap_used))]

// Storage-backend gate (#628/#633): at least one backend must be compiled in. SQLite
// is the default (`backend-sqlite`); `backend-postgres` is opt-in. With neither, the
// crate has no persistence layer at all — fail loudly at compile time. NOTE: today
// `backend-sqlite` is a marker (rusqlite is unconditional, the engine hardcodes
// `SqliteBackend`), so this only fires on an explicit `--no-default-features` with no
// backend — a forward guard-rail for the future engine-selects-backend wiring, not a
// live gate yet. The predicate is written against the final feature set, so that
// future PG-only build needs no change here.
#[cfg(not(any(feature = "backend-sqlite", feature = "backend-postgres")))]
compile_error!(
    "memory-engine requires at least one storage backend feature: \
     enable `backend-sqlite` (the default) or `backend-postgres`."
);

// === Public modules (consumer-facing API) ===
pub mod bootstrap;
pub mod engine;
// `error` + `types` are relocated to the L0 `me-types` crate (Wave 2 #816). The
// module re-export preserves `memory_engine::error::*` (and every internal
// `crate::error::` path) with no call-site churn.
pub use me_types::error;
pub mod inspect;
pub mod search;
pub mod storage;
// `traits` is relocated to the L0.5 `me-traits` crate (Wave 2 #816); the crate
// re-export preserves `memory_engine::traits::*` and every internal `crate::traits::`.
pub use me_traits as traits;
pub use me_types::types; // relocated to me-types (Wave 2 #816); see `error` above

// === Internal modules (implementation details) ===
#[cfg(feature = "archive")]
pub(crate) mod archive;
pub(crate) mod consolidation;
pub(crate) mod forgetting;
pub(crate) mod graph;
pub(crate) use me_types::limits; // relocated to me-types (Wave 2 #816)
pub(crate) mod pool;
pub(crate) mod resume;
pub(crate) mod scope;
pub(crate) mod store;

// === Shared test utilities (#485 / #120) ===
#[cfg(test)]
pub(crate) mod test_utils;

// === Re-exports: flat access to the most-used consumer types ===
#[cfg(feature = "archive")]
pub use archive::{ArchiveManifestEntry, ArchivePolicy, ArchiveStats, ArchiveVerifyResult};
pub use bootstrap::{BootstrapConfig, BootstrapReport, KeywordExtractor, SessionExtractor};
pub use engine::activity_filter::{ActivityFilterConfig, ActivityFilterDecision, PromoteAction};
pub use engine::builder::{File, InMemory, MemoryEngineBuilder};
pub use engine::{EngineConfig, MemoryEngine};
// Explicit re-export of the full `error` public surface (the umbrella
// `MemoryError` + the `Result` alias + each typed sub-enum). Enumerated rather
// than glob-imported so the crate-root API is auditable and a new public error
// type must be added here deliberately. The `reexports_are_accessible` smoke
// test guards the list.
pub use forgetting::{ForgetPolicy, PruneStats};
pub use inspect::types as inspect_types;
pub use me_traits::{
    ConflictArbiter, ConflictResolution, ConsolidationConfig, ConsolidationStats, CrudDecision,
    CycleCtx, DeltaProposer, DreamCycle, EmbeddingProvider, InsightStream, PersistenceClassifier,
    Reranker, SummarizableContent, SummaryGenerator,
};
pub use me_types::error::{
    ArchiveError, ConflictError, CycleError, MemoryError, MigrationError, RerankerError, Result,
    StorageError,
};
pub use resume::{ResumeConfig, ResumeContext};
pub use search::{
    MatchType, MemoryQuery, QueryDiagnostics, QueryResponse, SearchMode, SearchQuery, SearchResult,
    VectorSearchStrategy,
};
pub use store::UpcasterRegistry;
pub use store::schema::CURRENT_SCHEMA_VERSION;
// Explicit re-export of the full `types` public surface, enumerated for the
// same reason as `error` above: a transparent, audit-able crate-root API where
// adding a public type is a deliberate edit, not an implicit consequence of a
// glob. Kept in source order; the `reexports_are_accessible` smoke test guards
// a representative subset, and `cargo build --workspace --all-features` proves
// the whole list still satisfies every downstream consumer.
pub use me_types::types::{
    Activity, ActivityStatus, AddFactOptions, AddFactRequest, ClassifierInput, ConsolidationLevel,
    ConsolidationProposal, DreamCycleConfig, Edge, EmbeddingFingerprint, Event, EventFilter,
    EventType, Fact, FactId, FactScoringRow, FactType, Insight, LineageId, LineageRecord,
    LineageSnapshotEntry, MergeGroup, NewActivity, NewEdge, NewEvent, NewFact, NewFactBuilder,
    NewLineageRecord, NewSummary, Outcome, OutcomeClass, OutcomeCounts, ParseActivityStatusError,
    ProjectContext, PromoteOutcome, PromoteRequest, PromotionProvenance, PromotionResult,
    RecordActivityRequest, RecordActivityResult, RelationType, ScopeNode, ScopeQuery,
    SessionCheckpoint, SessionFact, Summary,
};

// Storage port — tight flat re-export of the umbrella + cross-cutting types only.
// The six bounded traits (FactGraph, EventLog, …) are the internal port surface
// backends implement and tests mock; they stay reachable as
// `memory_engine::storage::FactGraph`, not flat-re-exported, to keep the crate-root
// namespace honest. (StorageError flat-exports via `pub use error::*` above.)
pub use storage::{
    BackendCapabilities, FactFilter, LexicalRanker, MetadataPredicate, StorageBackend,
    TemporalFilter,
};

// Phase 5a cognitive pipeline re-exports
pub use engine::cognitive::{DreamContext, INSIGHT_MARKER_KEY};
pub use engine::cycle::{
    ApplyResult, CycleAnomaly, CycleContext, CycleDelta, CycleMetadata, CycleOutcome, CycleReport,
    DefaultDreamCycle, IMPORTANCE_STEP, IdentityOutput, LlmDreamCycle, MAX_ADJUSTMENT, SkipReason,
    TimeWindow,
};

/// Fuzz-only seam (`--cfg fuzzing`, set only by `cargo fuzz`).
///
/// Re-exports the otherwise crate-internal parser entry points so cargo-fuzz
/// targets can drive untrusted-byte ingest directly:
///
/// - [`engine::snapshot::load_from_file`] — the crate-internal binary snapshot
///   reader (u32 LE header + msgpack header/payload + blake3).
/// - [`bootstrap::parse::parse_session_file`] /
///   [`bootstrap::parse::parse_content_blocks`] — the crate-internal JSONL session
///   parsers (`pub fn` inside a `pub(crate) mod`, so not reachable downstream).
/// - [`search::fts::fuzz_fts_query`] — drives an untrusted query string through the
///   FTS5 `MATCH` path on a seeded in-memory DB (the store/schema setup it needs is
///   `pub(crate)`, so the seam owns it).
/// - [`inspect::restore::read_snapshot`] — the JSON snapshot import path (size-guard +
///   compression sniff + `serde_json::from_reader::<EngineSnapshot>`). #276 narrowed
///   the entire `inspect` module tree — including `restore` — to `pub(crate)`, because
///   `restore_snapshot_into(&Connection)` bypasses the engine lock discipline and must
///   not be externally reachable. The detached `fuzz` crate (excluded from
///   `cargo build --workspace`, so CI cannot catch a regression) needs only
///   `read_snapshot`, so it reaches it through this seam instead of through a widened
///   module.
/// - [`store::parse_timestamp`] / [`store::parse_optional_timestamp`] — the two
///   TEXT-column timestamp parsers (`DateTime::parse_from_rfc3339` wrapped into a
///   `rusqlite` error). They are `pub fn` inside the `pub(crate) mod store`, so the
///   external fuzz crate cannot reach them without this seam (#488). The contract is
///   total: every input yields `Ok`/`Err`, never a panic.
/// - [`archive::pak::read_pak`] — the two-layer (`zstd` frame + `serde_json`) `.pak`
///   reader, with the `MAX_PAK_DECOMPRESSED_SIZE` decompression-bomb cap. It is `pub
///   fn` inside the `#[cfg(feature = "archive")] pub(crate) mod archive`, so it is
///   unreachable from the fuzz crate without this seam (#421); the entry is therefore
///   `archive`-gated to match the module. Every input yields `Ok`/`Err`, never a panic.
///
/// This module compiles to nothing on a normal build, so it adds no public API
/// to the shipped crate.
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz_seam {
    #[cfg(feature = "archive")]
    pub use crate::archive::pak::read_pak;
    pub use crate::bootstrap::parse::{parse_content_blocks, parse_session_file};
    pub use crate::engine::snapshot::{fuzz_wrap_payload, load_from_file};
    pub use crate::inspect::restore::read_snapshot;
    pub use crate::search::fts::fuzz_fts_query;
    pub use crate::store::{parse_optional_timestamp, parse_timestamp};
}

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
        fn _accepts_delta_proposer(_: &dyn crate::DeltaProposer) {}
        // storage port umbrella (bounded traits stay at `crate::storage::*`)
        fn _accepts_storage_backend(_: &dyn crate::StorageBackend) {}
        // `Result` alias is part of the hand-enumerated `error` re-export.
        fn _accepts_result(_: crate::Result<()>) {}

        // `SummaryGenerator`'s parameter type must be reachable from the same
        // crate-root namespace as the trait it appears in (#273).
        fn _uses_summarizable_content(_: crate::SummarizableContent<'_>) {}

        // trait types
        let _ = std::mem::size_of::<crate::CrudDecision>();
        let _ = std::mem::size_of::<crate::ConsolidationConfig>();
        let _ = std::mem::size_of::<crate::ConsolidationStats>();
        let _ = std::mem::size_of::<crate::PruneStats>();
        let _ = std::mem::size_of::<crate::ConflictResolution>();
        let _ = std::mem::size_of::<crate::ForgetPolicy>();
        let _ = std::mem::size_of::<crate::ConsolidationProposal>();
        let _ = std::mem::size_of::<crate::MergeGroup>();

        // core types
        let _ = std::mem::size_of::<crate::FactType>();
        let _ = std::mem::size_of::<crate::Fact>();
        let _ = std::mem::size_of::<crate::EngineConfig>();
        let _ = std::mem::size_of::<crate::MemoryEngine>();

        // error surface — the typed sub-enums + umbrella + Result alias are now
        // hand-enumerated re-exports (no glob), so assert each is reachable at the
        // crate root. A removal from that list fails to compile here.
        let _ = std::mem::size_of::<crate::MemoryError>();
        let _ = std::mem::size_of::<crate::ConflictError>();
        let _ = std::mem::size_of::<crate::RerankerError>();
        let _ = std::mem::size_of::<crate::ArchiveError>();
        let _ = std::mem::size_of::<crate::MigrationError>();
        let _ = std::mem::size_of::<crate::CycleError>();
        // StorageError asserted in the storage-port block below; the `Result`
        // alias is asserted by `_accepts_result` above.

        // storage port cross-cutting types (flat-re-exported; the six bounded
        // traits stay reachable as `crate::storage::FactGraph`, asserted above).
        let _ = std::mem::size_of::<crate::FactFilter>();
        let _ = std::mem::size_of::<crate::TemporalFilter>();
        let _ = std::mem::size_of::<crate::MetadataPredicate>();
        let _ = std::mem::size_of::<crate::BackendCapabilities>();
        let _ = std::mem::size_of::<crate::LexicalRanker>();
        let _ = std::mem::size_of::<crate::StorageError>();
    }
}
