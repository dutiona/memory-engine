//! Async wrapper around [`MemoryEngine`] using `tokio::task::spawn_blocking`.
//!
//! Gated behind the `async` feature flag. All operations dispatch to
//! the blocking thread pool, so slow embedding calls don't block the
//! async runtime.

use std::sync::Arc;

use crate::engine::{EngineConfig, MemoryEngine};
use crate::error::{MemoryError, Result};
use crate::resume::context::{ResumeConfig, ResumeContext};
use crate::search::hybrid::{SearchQuery, SearchResult};
use crate::traits::{
    ConflictArbiter, ConflictResolution, ConsolidationConfig, ConsolidationStats,
    EmbeddingProvider, ForgetPolicy, PruneStats, SummaryGenerator,
};
use crate::types::{
    AddFactOptions, ConsolidationLevel, Fact, FactType, NewEvent, NewFact, Summary,
};

/// Async wrapper around [`MemoryEngine`].
///
/// All operations dispatch to `tokio::task::spawn_blocking`.
/// Async methods take **owned** values (not references) for `'static` lifetime.
///
/// ```rust,ignore
/// let engine = MemoryEngine::open_memory(768)?;
/// let async_engine = AsyncMemoryEngine::new(engine);
/// let id = async_engine.add_fact("hello".into(), FactType::Semantic, None,
///     embedder, None, None).await?;
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
        content: String,
        fact_type: FactType,
        source_event_id: Option<i64>,
        embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
        scope: Option<String>,
        opts: Option<AddFactOptions>,
    ) -> Result<i64> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            engine.add_fact(
                &content,
                fact_type,
                source_event_id,
                embedder.as_ref(),
                scope.as_deref(),
                opts.as_ref(),
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

    /// List all active facts.
    pub async fn list_active_facts(&self) -> Result<Vec<Fact>> {
        let engine = self.inner.clone();
        tokio::task::spawn_blocking(move || engine.list_active_facts())
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
                "async test fact".into(),
                FactType::Semantic,
                None,
                embedder,
                None,
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
                "concurrent test".into(),
                FactType::Semantic,
                None,
                embedder,
                None,
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
