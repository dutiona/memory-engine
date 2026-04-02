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
use crate::types::{
    AddFactRequest, ConsolidationLevel, Fact, NewEvent, NewFact, Summary,
};
#[cfg(test)]
use crate::types::{
    AddFactOptions, FactType,
};

/// Async wrapper around [`MemoryEngine`].
///
/// All operations dispatch to `tokio::task::spawn_blocking`.
/// Async methods take **owned** values (not references) for `'static` lifetime.
///
/// ```rust,ignore
/// let engine = MemoryEngine::open_memory(768)?;
/// let async_engine = AsyncMemoryEngine::new(engine);
/// let req = AddFactRequest { content: "hello".into(), fact_type: FactType::Semantic,
///     source_event_id: None, scope: None, opts: None };
/// let id = async_engine.add_fact(req, embedder, None).await?;
/// ```
#[derive(Clone)]
pub struct AsyncMemoryEngine {
    inner: Arc<MemoryEngine>,
}

/// Convert a `tokio::task::JoinError` into a `MemoryError::Pool`.
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
    pub fn from_arc(engine: Arc<MemoryEngine>) -> Self {
        Self { inner: engine }
    }

    /// Open a file-backed async engine.
    ///
    /// # Errors
    ///
    /// Returns errors from [`MemoryEngine::open`].
    pub async fn open(config: EngineConfig) -> Result<Self> {
        let engine = tokio::task::spawn_blocking(move || MemoryEngine::open(&config))
            .await
            .map_err(join_err)??;
        Ok(Self::new(engine))
    }

    /// Returns the name of the active reranker, if any.
    #[must_use]
    pub fn reranker_name(&self) -> Option<&str> {
        self.inner.reranker_name()
    }

    /// Open an in-memory async engine.
    ///
    /// # Errors
    ///
    /// Returns errors from [`MemoryEngine::open_memory`].
    pub async fn open_memory(embed_dim: usize) -> Result<Self> {
        let engine = tokio::task::spawn_blocking(move || MemoryEngine::open_memory(embed_dim))
            .await
            .map_err(join_err)??;
        Ok(Self::new(engine))
    }

    /// Append an event to the event log.
    pub async fn ingest(&self, event: NewEvent) -> Result<i64> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.ingest(&event))
            .await
            .map_err(join_err)?
    }

    /// Add a fact with embedding computation.
    pub async fn add_fact(
        &self,
        req: AddFactRequest,
        embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
        classifier: Option<Arc<dyn PersistenceClassifier + Send + Sync>>,
    ) -> Result<i64> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            engine.add_fact(
                &req,
                embedder.as_ref(),
                classifier
                    .as_ref()
                    .map(|c| c.as_ref() as &dyn PersistenceClassifier),
            )
        })
        .await
        .map_err(join_err)?
    }

    /// Add multiple facts atomically via batch embedding + single transaction.
    ///
    /// Async wrapper around [`MemoryEngine::add_facts_batch`].
    pub async fn add_facts_batch(
        &self,
        entries: Vec<AddFactRequest>,
        embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
        classifier: Option<Arc<dyn PersistenceClassifier + Send + Sync>>,
    ) -> Result<Vec<i64>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            engine.add_facts_batch(
                &entries,
                embedder.as_ref(),
                classifier
                    .as_ref()
                    .map(|c| c.as_ref() as &dyn PersistenceClassifier),
            )
        })
        .await
        .map_err(join_err)?
    }

    /// Query facts using hybrid search.
    pub async fn query(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.query(&query))
            .await
            .map_err(join_err)?
    }

    /// Execute a composed query using the [`MemoryQuery`](crate::search::query::MemoryQuery) builder.
    pub async fn execute_query(
        &self,
        query: crate::search::query::MemoryQuery,
    ) -> Result<QueryResponse> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.execute_query(&query))
            .await
            .map_err(join_err)?
    }

    /// Run three-pass consolidation.
    pub async fn consolidate(
        &self,
        generator: Arc<dyn SummaryGenerator + Send + Sync>,
        config: ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.consolidate(generator.as_ref(), &config))
            .await
            .map_err(join_err)?
    }

    /// Prune stale facts.
    pub async fn forget(&self, policy: ForgetPolicy) -> Result<PruneStats> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.forget(&policy))
            .await
            .map_err(join_err)?
    }

    /// Resolve a conflict between facts.
    pub async fn resolve_conflict(
        &self,
        arbiter: Arc<dyn ConflictArbiter + Send + Sync>,
        old_id: i64,
        new_fact: NewFact,
    ) -> Result<ConflictResolution> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            engine.resolve_conflict(arbiter.as_ref(), old_id, &new_fact)
        })
        .await
        .map_err(join_err)?
    }

    /// Get a fact by id.
    pub async fn get_fact(&self, id: i64) -> Result<Fact> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.get_fact(id))
            .await
            .map_err(join_err)?
    }

    /// List active facts, optionally limited.
    pub async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.list_active_facts(limit))
            .await
            .map_err(join_err)?
    }

    /// List summaries by level.
    pub async fn list_summaries(&self, level: ConsolidationLevel) -> Result<Vec<Summary>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.list_summaries(&level))
            .await
            .map_err(join_err)?
    }

    /// Read a config value.
    pub async fn get_config(&self, key: String) -> Result<Option<String>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.get_config(&key))
            .await
            .map_err(join_err)?
    }

    /// Write a config value.
    pub async fn set_config(&self, key: String, value: String) -> Result<()> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.set_config(&key, &value))
            .await
            .map_err(join_err)?
    }

    /// Retrieve tiered context for resuming a session.
    pub async fn resume_context(&self, config: ResumeConfig) -> Result<ResumeContext> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.resume_context(&config))
            .await
            .map_err(join_err)?
    }

    /// Get facts whose scheduled time has arrived.
    pub async fn list_due(&self, now: DateTime<Utc>, scope: Option<String>) -> Result<Vec<Fact>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.list_due(now, scope.as_deref()))
            .await
            .map_err(join_err)?
    }

    /// Scheduling hint: earliest `t_valid` among active future-dated facts.
    pub async fn next_due_time(&self, scope: Option<String>) -> Result<Option<DateTime<Utc>>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.next_due_time(scope.as_deref()))
            .await
            .map_err(join_err)?
    }

    /// Pin a fact (make it unforgettable).
    pub async fn pin_fact(&self, id: i64) -> Result<()> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.pin_fact(id))
            .await
            .map_err(join_err)?
    }

    /// Unpin a fact (allow forgetting).
    pub async fn unpin_fact(&self, id: i64) -> Result<()> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.unpin_fact(id))
            .await
            .map_err(join_err)?
    }

    /// Create co-session edges between facts sharing a session.
    pub async fn link_session_facts(
        &self,
        session_id: String,
        scope: Option<String>,
    ) -> Result<usize> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            engine.link_session_facts(&session_id, scope.as_deref())
        })
        .await
        .map_err(join_err)?
    }

    /// Async wrapper for [`MemoryEngine::statistics`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    pub async fn statistics(&self) -> Result<crate::inspect::EngineStatistics> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.statistics())
            .await
            .map_err(join_err)?
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
        let engine = self.inner.clone();
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || engine.replay_events(&filter))
            .await
            .map_err(join_err)?
    }

    /// Async wrapper for [`MemoryEngine::explain_fact`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::NotFound`] if the fact does not exist, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn explain_fact(&self, id: i64) -> Result<crate::inspect::FactExplanation> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.explain_fact(id))
            .await
            .map_err(join_err)?
    }

    /// Async wrapper for [`MemoryEngine::fact_history`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::NotFound`] if the fact does not exist, or
    /// [`MemoryError::Database`] on SQL failure.
    pub async fn fact_history(&self, id: i64) -> Result<crate::inspect::FactHistory> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.fact_history(id))
            .await
            .map_err(join_err)?
    }

    /// Async wrapper for [`MemoryEngine::dump_state`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Internal`] on I/O or serialization failure,
    /// or [`MemoryError::Database`] on SQL failure.
    pub async fn dump_state(&self, format: &crate::inspect::DumpFormat) -> Result<()> {
        let engine = self.inner.clone();
        let format = format.clone();
        tokio::task::spawn_blocking(move || engine.dump_state(&format))
            .await
            .map_err(join_err)?
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

    /// Graph statistics: (node_count, edge_count).
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
    /// records a manifest row, and hard-deletes them from SQLite.
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
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.archive(&policy))
            .await
            .map_err(join_err)?
    }

    /// Async wrapper for [`MemoryEngine::list_archives`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Database`] on SQL failure.
    #[cfg(feature = "archive")]
    pub async fn list_archives(&self) -> Result<Vec<crate::archive::ArchiveManifestEntry>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.list_archives())
            .await
            .map_err(join_err)?
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
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.verify_archives())
            .await
            .map_err(join_err)?
    }

    // --- Restore (static constructors) ---

    /// Async wrapper for [`MemoryEngine::restore_json`].
    pub async fn restore_json(
        snapshot_path: std::path::PathBuf,
        config: EngineConfig,
    ) -> Result<Self> {
        let engine = tokio::task::spawn_blocking(move || {
            MemoryEngine::restore_json(&snapshot_path, &config)
        })
        .await
        .map_err(join_err)??;
        Ok(Self::new(engine))
    }

    /// Async wrapper for [`MemoryEngine::restore_json_memory`].
    pub async fn restore_json_memory(snapshot_path: std::path::PathBuf) -> Result<Self> {
        let engine =
            tokio::task::spawn_blocking(move || MemoryEngine::restore_json_memory(&snapshot_path))
                .await
                .map_err(join_err)??;
        Ok(Self::new(engine))
    }

    /// Async wrapper for [`MemoryEngine::restore_sqlite`].
    pub async fn restore_sqlite(
        backup_path: std::path::PathBuf,
        config: EngineConfig,
    ) -> Result<Self> {
        let engine = tokio::task::spawn_blocking(move || {
            MemoryEngine::restore_sqlite(&backup_path, &config)
        })
        .await
        .map_err(join_err)??;
        Ok(Self::new(engine))
    }

    /// Write a snapshot of in-memory state to the sidecar file.
    ///
    /// Delegates to [`MemoryEngine::write_snapshot`] on a blocking task.
    pub async fn write_snapshot(&self) -> Result<bool> {
        let engine = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || engine.write_snapshot())
            .await
            .map_err(join_err)?
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
}
