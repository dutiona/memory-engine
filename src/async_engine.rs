//! Async wrapper around [`MemoryEngine`] using `tokio::task::spawn_blocking`.
//!
//! Gated behind the `async` feature flag. All operations dispatch to
//! the blocking thread pool, so slow embedding calls don't block the
//! async runtime.

use std::sync::Arc;

use crate::engine::{EngineConfig, MemoryEngine};
use crate::error::{MemoryError, Result};
use crate::resume::context::{ResumeConfig, ResumeContext};
use crate::search::hybrid::{QueryResponse, SearchQuery, SearchResult};
use chrono::{DateTime, Utc};

use crate::traits::{
    ConflictArbiter, ConflictResolution, ConsolidationConfig, ConsolidationStats,
    EmbeddingProvider, ForgetPolicy, PersistenceClassifier, PruneStats, SummaryGenerator,
};
#[cfg(test)]
use crate::types::{AddFactOptions, FactType};
use crate::types::{AddFactRequest, ConsolidationLevel, Fact, NewEvent, NewFact, Summary};

/// Dispatch `$expr` on a `tokio` blocking thread, propagating any
/// `JoinError` (including panics) as [`MemoryError::Pool`].
///
/// **Instance variant** (`delegate_blocking!(self, engine, $expr)`):
/// clones `self.inner` into a local named `$engine` that `$expr` can
/// capture, then spawns the closure.
///
/// **Static variant** (`delegate_blocking!($expr)`):
/// no engine clone — used for associated-function constructors where the
/// closure captures its own arguments.
macro_rules! delegate_blocking {
    // Instance method: clone self.inner into $engine_ident, capture in $expr.
    ($self:ident, $engine_ident:ident, $expr:expr) => {{
        let $engine_ident = $self.inner.clone();
        tokio::task::spawn_blocking(move || $expr)
            .await
            .map_err(join_err)?
    }};
    // Static/associated constructor: no self, no engine clone.
    ($expr:expr) => {
        tokio::task::spawn_blocking(move || $expr)
            .await
            .map_err(join_err)?
    };
}

/// Async wrapper around [`MemoryEngine`].
///
/// All operations dispatch to `tokio::task::spawn_blocking`.
/// Async methods take **owned** values (not references) for `'static` lifetime.
///
/// ```
/// use std::sync::Arc;
///
/// use memory_engine::async_engine::AsyncMemoryEngine;
/// use memory_engine::{
///     AddFactRequest, EmbeddingFingerprint, EmbeddingProvider, FactType, MemoryEngine, MemoryError,
/// };
///
/// // Deterministic, dependency-free embedder (see the crate-level example).
/// struct HashEmbedder {
///     dim: usize,
/// }
/// impl EmbeddingProvider for HashEmbedder {
///     fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
///         let mut v = vec![0.0_f32; self.dim];
///         for &b in text.as_bytes() {
///             v[b as usize % self.dim] += 1.0;
///         }
///         Ok(v)
///     }
///     fn fingerprint(&self) -> EmbeddingFingerprint {
///         EmbeddingFingerprint::new("mock", "test", self.dim)
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dim = 64;
/// let async_engine = AsyncMemoryEngine::new(MemoryEngine::builder(dim).build()?);
/// // Owned values cross the spawn_blocking boundary; the embedder is shared as an Arc.
/// let embedder: Arc<dyn EmbeddingProvider + Send + Sync> = Arc::new(HashEmbedder { dim });
/// let req = AddFactRequest {
///     content: "hello".into(),
///     fact_type: FactType::Semantic,
///     source_event_id: None,
///     scope: None,
///     opts: None,
/// };
/// let id = async_engine.add_fact(req, embedder, None).await?;
/// assert!(id > 0);
///
/// // Read the fact back through a second spawn_blocking hop to prove the
/// // async round-trip actually persisted it (not just returned an id).
/// let fact = async_engine.get_fact(id).await?;
/// assert_eq!(fact.content, "hello");
/// # Ok::<(), MemoryError>(())
/// # }).unwrap();
/// ```
#[derive(Clone)]
pub struct AsyncMemoryEngine {
    inner: Arc<MemoryEngine>,
}

/// Convert a `tokio::task::JoinError` into a `MemoryError::Pool`.
///
/// Takes `JoinError` by value so it can be passed directly as `map_err(join_err)`.
/// The value is only read via `Display`, so the by-value signature is a deliberate
/// ergonomic choice for the `map_err` combinator.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as map_err(join_err) fn pointer"
)]
fn join_err(e: tokio::task::JoinError) -> MemoryError {
    MemoryError::Pool(format!("task join error: {e}"))
}

impl AsyncMemoryEngine {
    /// Wrap a [`MemoryEngine`] for async use.
    #[must_use]
    pub fn new(engine: MemoryEngine) -> Self {
        Self {
            inner: Arc::new(engine),
        }
    }

    /// Wrap an existing `Arc<MemoryEngine>`.
    #[must_use]
    pub const fn from_arc(engine: Arc<MemoryEngine>) -> Self {
        Self { inner: engine }
    }

    /// Open a file-backed async engine from an [`EngineConfig`].
    ///
    /// For the synchronous ergonomic front door, see
    /// [`MemoryEngine::builder`].
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration` if the stored `embed_dim` doesn't match.
    pub async fn open(config: EngineConfig) -> Result<Self> {
        let engine = delegate_blocking!(MemoryEngine::open_from_config(&config, None))?;
        Ok(Self::new(engine))
    }

    /// Returns the name of the active reranker, if any.
    #[must_use]
    pub fn reranker_name(&self) -> Option<&str> {
        self.inner.reranker_name()
    }

    /// Open an in-memory async engine.
    ///
    /// For the synchronous ergonomic front door, see
    /// [`MemoryEngine::builder`].
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if the connection or schema setup fails.
    pub async fn open_memory(embed_dim: usize) -> Result<Self> {
        let engine = delegate_blocking!(MemoryEngine::builder(embed_dim).build())?;
        Ok(Self::new(engine))
    }

    /// Append an event to the event log.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn ingest(&self, event: NewEvent) -> Result<i64> {
        delegate_blocking!(self, engine, engine.ingest(&event))
    }

    /// Add a fact with embedding computation.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, [`MemoryError::Embedding`]
    /// if the embedder fails, or [`MemoryError::Pool`] if the blocking task panics.
    pub async fn add_fact(
        &self,
        req: AddFactRequest,
        embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
        classifier: Option<Arc<dyn PersistenceClassifier + Send + Sync>>,
    ) -> Result<i64> {
        delegate_blocking!(
            self,
            engine,
            engine.add_fact(
                &req,
                embedder.as_ref(),
                classifier
                    .as_ref()
                    .map(|c| c.as_ref() as &dyn PersistenceClassifier),
            )
        )
    }

    /// Add multiple facts atomically via batch embedding + single transaction.
    ///
    /// Async wrapper around [`MemoryEngine::add_facts_batch`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, [`MemoryError::Embedding`]
    /// if the embedder fails, or [`MemoryError::Pool`] if the blocking task panics.
    pub async fn add_facts_batch(
        &self,
        entries: Vec<AddFactRequest>,
        embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
        classifier: Option<Arc<dyn PersistenceClassifier + Send + Sync>>,
    ) -> Result<Vec<i64>> {
        delegate_blocking!(
            self,
            engine,
            engine.add_facts_batch(
                &entries,
                embedder.as_ref(),
                classifier
                    .as_ref()
                    .map(|c| c.as_ref() as &dyn PersistenceClassifier),
            )
        )
    }

    /// Query facts using hybrid search.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn query(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        delegate_blocking!(self, engine, engine.query(&query))
    }

    /// Execute a composed query using the [`MemoryQuery`](crate::search::query::MemoryQuery) builder.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn execute_query(
        &self,
        query: crate::search::query::MemoryQuery,
    ) -> Result<QueryResponse> {
        delegate_blocking!(self, engine, engine.execute_query(&query))
    }

    /// Run three-pass consolidation.
    ///
    /// `generator` produces the summary text; `embedder` projects it into the
    /// fact vector space (issue #116 — embedding is no longer on the generator).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, [`MemoryError::Embedding`]
    /// if the embedder fails, or [`MemoryError::Pool`] if the blocking task panics.
    pub async fn consolidate(
        &self,
        generator: Arc<dyn SummaryGenerator + Send + Sync>,
        embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
        config: ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        delegate_blocking!(
            self,
            engine,
            engine.consolidate(generator.as_ref(), embedder.as_ref(), &config)
        )
    }

    /// Prune stale facts.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn forget(&self, policy: ForgetPolicy) -> Result<PruneStats> {
        delegate_blocking!(self, engine, engine.forget(&policy))
    }

    /// Resolve a conflict between facts.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Conflict`] if the arbiter rejects both facts,
    /// [`MemoryError::Database`] on SQL failure, or [`MemoryError::Pool`] if
    /// the blocking task panics.
    pub async fn resolve_conflict(
        &self,
        arbiter: Arc<dyn ConflictArbiter + Send + Sync>,
        old_id: i64,
        new_fact: NewFact,
    ) -> Result<ConflictResolution> {
        delegate_blocking!(
            self,
            engine,
            engine.resolve_conflict(arbiter.as_ref(), old_id, &new_fact)
        )
    }

    /// Get a fact by id.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::NotFound`] if the fact does not exist, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn get_fact(&self, id: i64) -> Result<Fact> {
        delegate_blocking!(self, engine, engine.get_fact(id))
    }

    /// List active facts, optionally limited.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        delegate_blocking!(self, engine, engine.list_active_facts(limit))
    }

    /// List summaries by level.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn list_summaries(&self, level: ConsolidationLevel) -> Result<Vec<Summary>> {
        delegate_blocking!(self, engine, engine.list_summaries(&level))
    }

    /// Read a config value.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn get_config(&self, key: String) -> Result<Option<String>> {
        delegate_blocking!(self, engine, engine.get_config(&key))
    }

    /// Write a config value.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn set_config(&self, key: String, value: String) -> Result<()> {
        delegate_blocking!(self, engine, engine.set_config(&key, &value))
    }

    /// Retrieve tiered context for resuming a session.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn resume_context(&self, config: ResumeConfig) -> Result<ResumeContext> {
        delegate_blocking!(self, engine, engine.resume_context(&config))
    }

    /// Get facts whose scheduled time has arrived.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn list_due(&self, now: DateTime<Utc>, scope: Option<String>) -> Result<Vec<Fact>> {
        delegate_blocking!(self, engine, engine.list_due(now, scope.as_deref()))
    }

    /// Scheduling hint: earliest `t_valid` among active future-dated facts.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn next_due_time(&self, scope: Option<String>) -> Result<Option<DateTime<Utc>>> {
        delegate_blocking!(self, engine, engine.next_due_time(scope.as_deref()))
    }

    /// Pin a fact (make it unforgettable).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::NotFound`] if the fact does not exist, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn pin_fact(&self, id: i64) -> Result<()> {
        delegate_blocking!(self, engine, engine.pin_fact(id))
    }

    /// Unpin a fact (allow forgetting).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::NotFound`] if the fact does not exist, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn unpin_fact(&self, id: i64) -> Result<()> {
        delegate_blocking!(self, engine, engine.unpin_fact(id))
    }

    /// Create co-session edges between facts sharing a session.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn link_session_facts(
        &self,
        session_id: String,
        scope: Option<String>,
    ) -> Result<usize> {
        delegate_blocking!(
            self,
            engine,
            engine.link_session_facts(&session_id, scope.as_deref())
        )
    }

    /// Async wrapper for [`MemoryEngine::statistics`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn statistics(&self) -> Result<crate::inspect::EngineStatistics> {
        delegate_blocking!(self, engine, engine.statistics())
    }

    /// Async wrapper for [`MemoryEngine::replay_events`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn replay_events(
        &self,
        filter: &crate::inspect::ReplayFilter,
    ) -> Result<Vec<crate::types::Event>> {
        let filter = filter.clone();
        delegate_blocking!(self, engine, engine.replay_events(&filter))
    }

    /// Async wrapper for [`MemoryEngine::explain_fact`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::NotFound`] if the fact does not exist, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn explain_fact(&self, id: i64) -> Result<crate::inspect::FactExplanation> {
        delegate_blocking!(self, engine, engine.explain_fact(id))
    }

    /// Async wrapper for [`MemoryEngine::fact_history`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::NotFound`] if the fact does not exist, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn fact_history(&self, id: i64) -> Result<crate::inspect::FactHistory> {
        delegate_blocking!(self, engine, engine.fact_history(id))
    }

    /// Async wrapper for [`MemoryEngine::dump_state`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Internal`] on I/O or serialization failure,
    /// or [`MemoryError::Database`] on SQL failure.
    pub async fn dump_state(&self, format: &crate::inspect::DumpFormat) -> Result<()> {
        let format = format.clone();
        delegate_blocking!(self, engine, engine.dump_state(&format))
    }

    /// Graph degree for a fact.
    #[must_use]
    pub fn graph_degree(&self, fact_id: i64) -> usize {
        self.inner.graph_degree(fact_id)
    }

    /// Get the connected component containing a fact.
    #[must_use]
    pub fn graph_component(&self, fact_id: i64) -> Vec<i64> {
        self.inner.graph_component(fact_id)
    }

    /// Get outgoing neighbors of a fact in the graph.
    #[must_use]
    pub fn graph_neighbors(&self, fact_id: i64) -> Vec<i64> {
        self.inner.graph_neighbors(fact_id)
    }

    /// Graph statistics: (`node_count`, `edge_count`).
    #[must_use]
    pub fn graph_stats(&self) -> (usize, usize) {
        self.inner.graph_stats()
    }

    /// Check if a node exists in the graph.
    #[must_use]
    pub fn graph_has_node(&self, fact_id: i64) -> bool {
        self.inner.graph_has_node(fact_id)
    }

    /// Whether the engine is backed by a file (vs. in-memory).
    #[must_use]
    pub fn is_file_backed(&self) -> bool {
        self.inner.is_file_backed()
    }

    /// Embedding dimension.
    #[must_use]
    pub fn embed_dim(&self) -> usize {
        self.inner.embed_dim()
    }

    // --- Archive ---

    /// Async wrapper for [`MemoryEngine::archive`].
    ///
    /// Moves expired, non-pinned facts into a `.pak` file (zstd + blake3),
    /// records a manifest row, and hard-deletes them from `SQLite`.
    ///
    /// Returns `None` if fewer than `policy.min_facts` candidates exist.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Archive`] on I/O failure, or
    /// [`MemoryError::Database`] on SQL failure.
    #[cfg(feature = "archive")]
    pub async fn archive(
        &self,
        policy: crate::archive::ArchivePolicy,
    ) -> Result<Option<crate::archive::ArchiveStats>> {
        delegate_blocking!(self, engine, engine.archive(&policy))
    }

    /// Async wrapper for [`MemoryEngine::list_archives`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    #[cfg(feature = "archive")]
    pub async fn list_archives(&self) -> Result<Vec<crate::archive::ArchiveManifestEntry>> {
        delegate_blocking!(self, engine, engine.list_archives())
    }

    /// Async wrapper for [`MemoryEngine::verify_archives`].
    ///
    /// Checks each manifest entry's blake3 hash against the actual file.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    #[cfg(feature = "archive")]
    pub async fn verify_archives(&self) -> Result<Vec<crate::archive::ArchiveVerifyResult>> {
        delegate_blocking!(self, engine, engine.verify_archives())
    }

    // --- Restore (static constructors) ---

    /// Async wrapper for [`MemoryEngine::restore_json`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Internal`] on I/O or deserialization failure, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn restore_json(
        snapshot_path: std::path::PathBuf,
        config: EngineConfig,
    ) -> Result<Self> {
        let engine = delegate_blocking!(MemoryEngine::restore_json(&snapshot_path, &config))?;
        Ok(Self::new(engine))
    }

    /// Async wrapper for [`MemoryEngine::restore_json_memory`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Internal`] on I/O or deserialization failure, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn restore_json_memory(snapshot_path: std::path::PathBuf) -> Result<Self> {
        let engine = delegate_blocking!(MemoryEngine::restore_json_memory(&snapshot_path))?;
        Ok(Self::new(engine))
    }

    /// Async wrapper for [`MemoryEngine::restore_sqlite`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Internal`] on I/O failure, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn restore_sqlite(
        backup_path: std::path::PathBuf,
        config: EngineConfig,
    ) -> Result<Self> {
        let engine = delegate_blocking!(MemoryEngine::restore_sqlite(&backup_path, &config))?;
        Ok(Self::new(engine))
    }

    /// Write a snapshot of in-memory state to the sidecar file.
    ///
    /// Delegates to [`MemoryEngine::write_snapshot`] on a blocking task.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Internal`] on I/O or serialization failure.
    pub async fn write_snapshot(&self) -> Result<bool> {
        delegate_blocking!(self, engine, engine.write_snapshot())
    }

    // --- Phase 5a: Cognitive pipeline ---

    /// Sample dormant facts semantically related to a context.
    ///
    /// See [`MemoryEngine::sample_dormant`] for details.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn sample_dormant(
        &self,
        n: usize,
        context: Vec<f32>,
        scope_ids: Option<Vec<i64>>,
    ) -> Result<Vec<Fact>> {
        delegate_blocking!(
            self,
            engine,
            engine.sample_dormant(n, &context, scope_ids.as_deref())
        )
    }

    /// Record a high-value insight via the provided `InsightStream`.
    ///
    /// See [`MemoryEngine::record_insight`] for details.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn record_insight(
        &self,
        insight: crate::types::Insight,
        stream: Arc<dyn crate::traits::InsightStream + Send + Sync>,
    ) -> Result<()> {
        delegate_blocking!(
            self,
            engine,
            engine.record_insight(insight, stream.as_ref())
        )
    }

    /// Run a `DreamCycle` using a capability-restricted `DreamContext`.
    ///
    /// See [`MemoryEngine::run_dream_cycle`] for details.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure, or
    /// [`MemoryError::Pool`] if the blocking task panics.
    pub async fn run_dream_cycle(
        &self,
        cycle: Arc<dyn crate::traits::DreamCycle + Send + Sync>,
    ) -> Result<crate::engine::cycle::CycleReport> {
        delegate_blocking!(self, engine, engine.run_dream_cycle(cycle.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::hybrid::SearchMode;

    const DIM: usize = 4;

    struct MockEmbedder {
        dim: usize,
    }

    impl EmbeddingProvider for MockEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; self.dim])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", self.dim)
        }
    }

    /// Convenience: a freshly built `Arc<dyn EmbeddingProvider>` for the tests
    /// that pass an embedder to a delegated async method.
    fn embedder() -> Arc<dyn EmbeddingProvider + Send + Sync> {
        Arc::new(MockEmbedder { dim: DIM })
    }

    /// Minimal summary generator: concatenates fact contents. Used to exercise
    /// the `consolidate` delegation arm.
    struct MockGen;
    impl SummaryGenerator for MockGen {
        fn summarize(&self, items: &[crate::traits::SummarizableContent<'_>]) -> Result<String> {
            Ok(items.iter().map(|c| c.text).collect::<Vec<_>>().join("; "))
        }
    }

    /// Arbiter that always returns a fixed decision. Used to drive
    /// `resolve_conflict` deterministically across the `spawn_blocking` boundary.
    struct FixedArbiter {
        decision: crate::traits::CrudDecision,
    }
    impl ConflictArbiter for FixedArbiter {
        fn arbitrate(&self, _: &Fact, _: &Fact) -> Result<crate::traits::CrudDecision> {
            Ok(self.decision)
        }
    }

    /// Build a simple `NewEvent` for the `ingest` delegation tests.
    fn make_event() -> NewEvent {
        NewEvent {
            timestamp: Utc::now(),
            event_type: crate::types::EventType::Interaction,
            payload: serde_json::json!({"msg": "hello"}),
            source: "test".into(),
            session_id: None,
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        }
    }

    #[tokio::test]
    async fn async_add_fact_and_query() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let embedder: Arc<dyn EmbeddingProvider + Send + Sync> =
            Arc::new(MockEmbedder { dim: DIM });
        let id = engine
            .add_fact(
                AddFactRequest {
                    content: "async test fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder,
                None,
            )
            .await
            .unwrap();
        assert!(id > 0);

        let results = engine
            .query(SearchQuery {
                text: Some("async".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn async_concurrent_queries() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let embedder: Arc<dyn EmbeddingProvider + Send + Sync> =
            Arc::new(MockEmbedder { dim: DIM });
        engine
            .add_fact(
                AddFactRequest {
                    content: "concurrent test".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder,
                None,
            )
            .await
            .unwrap();

        let mut handles = vec![];
        for _ in 0..10 {
            let e = engine.clone();
            handles.push(tokio::spawn(async move {
                let results = e
                    .query(SearchQuery {
                        text: Some("concurrent".into()),
                        embedding: None,
                        mode: SearchMode::Fts,
                        limit: 10,
                        rerank_depth: None,
                        valid_at: None,
                        fact_type: None,
                        scope: None,
                    })
                    .await
                    .unwrap();
                assert_eq!(results.len(), 1);
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    }

    #[tokio::test]
    async fn async_graph_methods() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();

        // Empty graph baseline
        assert_eq!(engine.graph_stats(), (0, 0));
        assert!(!engine.graph_has_node(1));
        assert!(engine.graph_component(1).is_empty());
        assert!(engine.graph_neighbors(1).is_empty());
        assert_eq!(engine.graph_degree(1), 0);
        assert!(!engine.is_file_backed());
    }

    #[tokio::test]
    async fn async_list_due() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let embedder: Arc<dyn EmbeddingProvider + Send + Sync> =
            Arc::new(MockEmbedder { dim: DIM });
        let past = Utc::now() - chrono::Duration::hours(1);
        let opts = AddFactOptions {
            t_valid: Some(past),
            ..Default::default()
        };
        engine
            .add_fact(
                AddFactRequest {
                    content: "reminder".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(opts),
                },
                embedder,
                None,
            )
            .await
            .unwrap();

        let due = engine.list_due(Utc::now(), None).await.unwrap();
        assert_eq!(due.len(), 1);
        assert!(due[0].content.contains("reminder"));
    }

    #[tokio::test]
    async fn async_pin_unpin() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let embedder: Arc<dyn EmbeddingProvider + Send + Sync> =
            Arc::new(MockEmbedder { dim: DIM });
        let id = engine
            .add_fact(
                AddFactRequest {
                    content: "pinnable".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder,
                None,
            )
            .await
            .unwrap();

        assert!(!engine.get_fact(id).await.unwrap().is_pinned);
        engine.pin_fact(id).await.unwrap();
        assert!(engine.get_fact(id).await.unwrap().is_pinned);
        engine.unpin_fact(id).await.unwrap();
        assert!(!engine.get_fact(id).await.unwrap().is_pinned);
    }

    #[tokio::test]
    async fn async_config_roundtrip() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        engine
            .set_config("async_key".into(), "async_val".into())
            .await
            .unwrap();
        let val = engine.get_config("async_key".into()).await.unwrap();
        assert_eq!(val, Some("async_val".into()));
    }

    /// Pins the `join_err` helper contract: a panic caught by tokio's
    /// blocking-task runtime surfaces as `MemoryError::Pool` with the
    /// "task join error" prefix.
    ///
    /// This test calls `join_err` **directly** — it does NOT exercise the
    /// `delegate_blocking!` macro expansion path. See
    /// `delegate_blocking_macro_panic_maps_to_pool_error` for the end-to-end
    /// macro oracle.
    #[tokio::test]
    async fn blocking_panic_maps_to_pool_error() {
        let result: Result<()> = tokio::task::spawn_blocking(|| {
            panic!("deliberate panic in blocking task");
        })
        .await
        .map_err(join_err);

        match result {
            Err(MemoryError::Pool(msg)) => {
                assert!(
                    msg.contains("task join error"),
                    "expected 'task join error' prefix, got: {msg}"
                );
            }
            other => panic!("expected MemoryError::Pool, got: {other:?}"),
        }
    }

    /// Embedder whose `embed` implementation panics unconditionally.
    ///
    /// Used by `delegate_blocking_macro_panic_maps_to_pool_error` to inject a
    /// panic into the `spawn_blocking` closure expanded by the instance arm of
    /// `delegate_blocking!`.
    struct PanickingEmbedder;

    impl EmbeddingProvider for PanickingEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            panic!("deliberate panic inside delegate_blocking! instance arm");
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", DIM)
        }
    }

    /// End-to-end oracle for the `delegate_blocking!` instance arm.
    ///
    /// Routes a panic through a real `AsyncMemoryEngine` delegated method
    /// (`add_fact`) whose inner synchronous call invokes `embedder.embed()`.
    /// The panic is caught by tokio inside the `spawn_blocking` closure
    /// expanded by the macro, and MUST surface as `MemoryError::Pool` — not a
    /// hang, not a different error variant, not an unwinding abort.
    ///
    /// **Discrimination**: removing `.map_err(join_err)` from the instance arm
    /// of `delegate_blocking!` causes a compile error because `JoinError` has
    /// no `From` impl for `MemoryError`, so the `?` operator cannot coerce the
    /// `Result<Result<i64, MemoryError>, JoinError>` return type. This is a
    /// compile-time discrimination signal — the test becomes unreachable before
    /// the runtime assertion can be evaluated.
    #[tokio::test]
    async fn delegate_blocking_macro_panic_maps_to_pool_error() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let embedder: Arc<dyn EmbeddingProvider + Send + Sync> = Arc::new(PanickingEmbedder);

        let result = engine
            .add_fact(
                AddFactRequest {
                    content: "panic test".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder,
                None,
            )
            .await;

        match result {
            Err(MemoryError::Pool(msg)) => {
                assert!(
                    msg.contains("task join error"),
                    "expected 'task join error' prefix from delegate_blocking! instance arm, got: {msg}"
                );
            }
            other => panic!(
                "expected MemoryError::Pool from delegate_blocking! instance arm, got: {other:?}"
            ),
        }
    }

    // --- Issue #308: delegation coverage for the spawn_blocking shim ---
    //
    // The async wrapper's only value-add is dispatching each sync method onto
    // `tokio::task::spawn_blocking`. The original suite covered add_fact / query
    // / graph / list_due / pin / config plus the two join-error oracles, leaving
    // the bulk of the delegated surface (ingest, execute_query, consolidate,
    // forget, resolve_conflict, statistics, the inspection readers, scheduling,
    // session linking, snapshotting, and the trivial getters) with no async
    // test. Each test below asserts the async method round-trips to its sync
    // counterpart across the spawn_blocking boundary — the result is observed
    // through a *second* async hop where possible, so a broken delegation (wrong
    // method, dropped argument, or a closure that never runs) fails the oracle.

    #[tokio::test]
    async fn async_ingest_appends_event() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let id = engine.ingest(make_event()).await.unwrap();
        assert_eq!(id, 1, "first ingested event gets id 1");
        // A second ingest advances the id — proves the write actually committed.
        let id2 = engine.ingest(make_event()).await.unwrap();
        assert_eq!(id2, 2);
    }

    #[tokio::test]
    async fn async_add_facts_batch_round_trip() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let reqs = vec![
            AddFactRequest {
                content: "batch fact one".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            AddFactRequest {
                content: "batch fact two".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
        ];
        let ids = engine
            .add_facts_batch(reqs, embedder(), None)
            .await
            .unwrap();
        assert_eq!(ids.len(), 2);
        let active = engine.list_active_facts(None).await.unwrap();
        assert_eq!(active.len(), 2, "both batch facts persisted");
    }

    #[tokio::test]
    async fn async_execute_query_returns_active_facts() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        engine
            .add_fact(
                AddFactRequest {
                    content: "composed query fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder(),
                None,
            )
            .await
            .unwrap();

        let resp = engine
            .execute_query(crate::search::query::MemoryQuery::new())
            .await
            .unwrap();
        assert_eq!(resp.results.len(), 1);
    }

    #[tokio::test]
    async fn async_consolidate_dedups_duplicates() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        // Two facts with identical (constant) embeddings → cosine 1.0 → dedup.
        for content in ["duplicate alpha", "duplicate beta"] {
            engine
                .add_fact(
                    AddFactRequest {
                        content: content.into(),
                        fact_type: FactType::Semantic,
                        source_event_id: None,
                        scope: None,
                        opts: None,
                    },
                    embedder(),
                    None,
                )
                .await
                .unwrap();
        }

        let config = ConsolidationConfig::builder()
            .dedup_threshold(0.90)
            .min_cluster_size(10) // high so no clusters form; isolate dedup
            .build();
        let generator: Arc<dyn SummaryGenerator + Send + Sync> = Arc::new(MockGen);
        let stats = engine
            .consolidate(generator, embedder(), config)
            .await
            .unwrap();
        assert_eq!(stats.duplicates_removed, 1);
        assert_eq!(engine.list_active_facts(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn async_forget_prunes_and_rejects_invalid_policy() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        // Backdate the fact 200 days so Ebbinghaus decay drives its computed
        // importance below the threshold (Episodic decays by design). A fresh
        // fact would survive — this mirrors the sync `forget_prunes_stale_facts`
        // setup, exercising real pruning through the shim rather than a no-op.
        let old_time = Utc::now() - chrono::Duration::days(200);
        engine
            .add_fact(
                AddFactRequest {
                    content: "forgettable".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(AddFactOptions {
                        importance: Some(0.01),
                        t_created: Some(old_time),
                        last_accessed: Some(old_time),
                        ..Default::default()
                    }),
                },
                embedder(),
                None,
            )
            .await
            .unwrap();

        // Valid policy prunes the low-importance fact.
        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };
        let stats = engine.forget(policy).await.unwrap();
        assert_eq!(stats.facts_expired, 1);

        // Invalid policy surfaces an error through the shim (not a panic/hang).
        let bad = ForgetPolicy {
            half_life_days: 0.0,
            ..ForgetPolicy::default()
        };
        assert!(engine.forget(bad).await.is_err());
    }

    #[tokio::test]
    async fn async_resolve_conflict_expires_old_fact() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let old_id = engine
            .add_fact(
                AddFactRequest {
                    content: "outdated belief".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder(),
                None,
            )
            .await
            .unwrap();

        let arbiter: Arc<dyn ConflictArbiter + Send + Sync> = Arc::new(FixedArbiter {
            decision: crate::traits::CrudDecision::Update,
        });
        let new_fact = NewFact::builder("updated belief", vec![0.5; DIM], FactType::Semantic)
            .content_hash("h_updated")
            .build();
        let resolution = engine
            .resolve_conflict(arbiter, old_id, new_fact)
            .await
            .unwrap();
        // An Update resolution supersedes the old fact with a new one.
        assert_eq!(resolution.decision, crate::traits::CrudDecision::Update);
        assert_eq!(resolution.old_fact_id, old_id);
        assert!(
            resolution.new_fact_id.is_some(),
            "Update must create a superseding fact"
        );
        // Old fact is soft-deleted (expired); a new active fact replaces it.
        assert!(engine.get_fact(old_id).await.unwrap().t_expired.is_some());
    }

    #[tokio::test]
    async fn async_statistics_counts_facts() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        engine
            .add_fact(
                AddFactRequest {
                    content: "counted".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder(),
                None,
            )
            .await
            .unwrap();
        let stats = engine.statistics().await.unwrap();
        assert_eq!(stats.facts.active, 1);
    }

    #[tokio::test]
    async fn async_list_summaries_empty() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let summaries = engine
            .list_summaries(ConsolidationLevel::Global)
            .await
            .unwrap();
        assert!(summaries.is_empty());
    }

    #[tokio::test]
    async fn async_resume_context_surfaces_pinned() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        engine
            .add_fact(
                AddFactRequest {
                    content: "pinned identity".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(AddFactOptions {
                        pinned: Some(true),
                        importance: Some(0.95),
                        ..Default::default()
                    }),
                },
                embedder(),
                None,
            )
            .await
            .unwrap();
        let ctx = engine
            .resume_context(ResumeConfig::default())
            .await
            .unwrap();
        assert_eq!(ctx.pinned.len(), 1);
        assert!(ctx.pinned[0].is_pinned);
    }

    #[tokio::test]
    async fn async_link_session_facts_creates_edges() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        // Two facts sharing a session via their source events.
        for content in ["session fact a", "session fact b"] {
            let mut event = make_event();
            event.session_id = Some("s1".into());
            let event_id = engine.ingest(event).await.unwrap();
            engine
                .add_fact(
                    AddFactRequest {
                        content: content.into(),
                        fact_type: FactType::Semantic,
                        source_event_id: Some(event_id),
                        scope: None,
                        opts: None,
                    },
                    embedder(),
                    None,
                )
                .await
                .unwrap();
        }
        let created = engine.link_session_facts("s1".into(), None).await.unwrap();
        assert_eq!(created, 2, "A→B and B→A co-session edges");
    }

    #[tokio::test]
    async fn async_next_due_time_none_when_no_future_facts() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        assert!(engine.next_due_time(None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn async_replay_events_returns_ingested() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        engine.ingest(make_event()).await.unwrap();
        let filter = crate::inspect::ReplayFilter::default();
        let events = engine.replay_events(&filter).await.unwrap();
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn async_explain_and_history_for_fact() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        let id = engine
            .add_fact(
                AddFactRequest {
                    content: "explainable fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder(),
                None,
            )
            .await
            .unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        assert_eq!(explanation.fact_id, id);
        let history = engine.fact_history(id).await.unwrap();
        assert_eq!(history.fact_id, id);
        // Missing fact surfaces NotFound through the shim.
        assert!(matches!(
            engine.explain_fact(999_999).await,
            Err(MemoryError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn async_write_snapshot_in_memory_is_noop() {
        // In-memory engines have no sidecar path, so write_snapshot returns
        // Ok(false) — the point is the delegation runs without panicking.
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        assert!(!engine.write_snapshot().await.unwrap());
    }

    #[tokio::test]
    async fn async_getters_reflect_inner_engine() {
        let engine = AsyncMemoryEngine::open_memory(DIM).await.unwrap();
        assert_eq!(engine.embed_dim(), DIM);
        assert!(!engine.is_file_backed());
        assert!(engine.reranker_name().is_none());
    }

    #[tokio::test]
    async fn async_open_file_backed_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("async_open.db");
        let config = EngineConfig::new(db_path, DIM);
        let engine = AsyncMemoryEngine::open(config).await.unwrap();
        assert!(engine.is_file_backed());
        let id = engine.ingest(make_event()).await.unwrap();
        assert_eq!(id, 1);
    }
}
