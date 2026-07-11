use chrono::{DateTime, Utc};

use me_types::types::search::SearchMode;
use me_types::types::{FactType, ScopeQuery};

/// Default limit for queries when not explicitly set.
const DEFAULT_LIMIT: usize = 50;

/// Fluent query builder for composing memory extraction queries.
///
/// All filters are optional and combine with AND semantics.
/// An empty query returns all temporally-valid active facts sorted by `importance_score`.
/// Future-dated facts (`t_valid > now`) are excluded by default (scheduling model invariant).
///
/// # Examples
///
/// ```
/// use memory_engine::{
///     AddFactRequest, EmbeddingFingerprint, EmbeddingProvider, FactType, MemoryEngine, MemoryError,
///     MemoryQuery,
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
/// // The engine API is async (#631); a consumer binary uses `#[tokio::main]`.
/// tokio::runtime::Runtime::new().unwrap().block_on(async {
///     let dim = 64;
///     let engine = MemoryEngine::builder(dim).build()?;
///     let embedder = HashEmbedder { dim };
///     // A deep, hierarchical scope: the fact lives under `user:michael/project:demo`.
///     // `add_fact` auto-creates every missing segment (`user:michael`, then
///     // `project:demo`) in both the database and the in-memory scope tree.
///     engine.add_fact(
///         &AddFactRequest {
///             content: "deployment issue in the demo project".into(),
///             fact_type: FactType::Episodic,
///             source_event_id: None,
///             scope: Some("user:michael/project:demo".into()),
///             opts: None,
///         },
///         std::sync::Arc::new(embedder),
///         None,
///     ).await?;
///
///     // Scoped retrieval over a *subtree*: every fact rooted at the `user:michael`
///     // ancestor (which includes the deeper `project:demo` child), capped at 20
///     // results. An empty query (no `text`/`embedding`) returns every
///     // temporally-valid fact in scope, sorted by importance.
///     let response = engine.execute_query(
///         &MemoryQuery::new()
///             .scope_subtree("user:michael")
///             .limit(20),
///     ).await?;
///     assert_eq!(response.results.len(), 1);
///     assert_eq!(response.results[0].fact.content, "deployment issue in the demo project");
///
///     // All pinned facts (none were pinned, so this is empty).
///     let pinned = engine.execute_query(&MemoryQuery::new().pinned_only()).await?;
///     assert!(pinned.results.is_empty());
///     Ok::<(), MemoryError>(())
/// })
/// .unwrap();
/// ```
#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    /// Scope filter (exact, subtree, ancestors, or inherited).
    pub scope: Option<ScopeQuery>,
    /// Period overlap start (inclusive). Must be set together with `period_end`.
    pub period_start: Option<DateTime<Utc>>,
    /// Period overlap end (exclusive). Must be set together with `period_start`.
    pub period_end: Option<DateTime<Utc>>,
    /// Text query for FTS search.
    pub text: Option<String>,
    /// Embedding vector for vector similarity search.
    pub embedding: Option<Vec<f32>>,
    /// Search mode override. When `None`, inferred from `text`/`embedding` presence (D7).
    pub search_mode: Option<SearchMode>,
    /// Filter by fact type (Episodic, Semantic, Procedural).
    pub fact_type: Option<FactType>,
    /// Minimum materialized importance score threshold (filters on `Fact.importance_score`).
    pub min_importance_score: Option<f64>,
    /// Return only pinned (unforgettable) facts.
    pub pinned_only: bool,
    /// Maximum number of results. Default: 50.
    pub limit: Option<usize>,
    /// Point-in-time temporal filter (mutually exclusive with `period`).
    pub valid_at: Option<DateTime<Utc>>,
    /// Run a secondary probe for expired facts matching the FTS5 query.
    /// Adds one SQL COUNT query per execute. Only effective when `text` is set
    /// (vector-only queries have no FTS5 terms to probe — `expired_matches`
    /// will be `None`).
    pub include_expired_probe: bool,
    /// When `true`, brute-force search archived `.pak` files after the normal
    /// search. Results are merged and re-ranked by score. This is a slow
    /// fallback — use only when recalling archived facts is required.
    ///
    /// Requires the `archive` feature.
    #[cfg(feature = "archive")]
    pub include_archives: bool,
}

impl MemoryQuery {
    /// Create a new empty query. All filters default to `None`/`false`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Effective limit, applying the default when not explicitly set.
    #[must_use]
    pub fn effective_limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }

    /// Whether this query involves text/vector search (vs. store-only path).
    #[must_use]
    pub const fn has_search(&self) -> bool {
        self.text.is_some() || self.embedding.is_some()
    }

    /// Whether a temporal period filter is set.
    #[must_use]
    pub const fn has_period(&self) -> bool {
        self.period_start.is_some() || self.period_end.is_some()
    }

    // --- Scope ---

    /// Filter to facts at exactly this scope path.
    #[must_use]
    pub fn scope_exact(mut self, path: impl Into<String>) -> Self {
        self.scope = Some(ScopeQuery::Exact(path.into()));
        self
    }

    /// Filter to facts at this scope path and all descendants.
    #[must_use]
    pub fn scope_subtree(mut self, path: impl Into<String>) -> Self {
        self.scope = Some(ScopeQuery::Subtree(path.into()));
        self
    }

    /// Filter to facts at this scope path and all ancestors up to root.
    #[must_use]
    pub fn scope_ancestors(mut self, path: impl Into<String>) -> Self {
        self.scope = Some(ScopeQuery::Ancestors(path.into()));
        self
    }

    /// Filter to facts at ancestors + subtree (full inherited context).
    #[must_use]
    pub fn scope_inherited(mut self, path: impl Into<String>) -> Self {
        self.scope = Some(ScopeQuery::Inherited(path.into()));
        self
    }

    // --- Temporal ---

    /// Point-in-time filter (existing `SearchQuery` semantics).
    /// Mutually exclusive with [`period`](Self::period).
    #[must_use]
    pub const fn valid_at(mut self, at: DateTime<Utc>) -> Self {
        self.valid_at = Some(at);
        self
    }

    /// Period overlap filter: facts whose `[t_valid, t_invalid)` overlaps `[start, end)`.
    /// Mutually exclusive with [`valid_at`](Self::valid_at).
    #[must_use]
    pub const fn period(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        self.period_start = Some(start);
        self.period_end = Some(end);
        self
    }

    // --- Semantic search ---

    /// Set text query for FTS search.
    #[must_use]
    pub fn text(mut self, query: impl Into<String>) -> Self {
        self.text = Some(query.into());
        self
    }

    /// Set embedding vector for vector similarity search.
    #[must_use]
    pub fn embedding(mut self, emb: Vec<f32>) -> Self {
        self.embedding = Some(emb);
        self
    }

    /// Override the inferred search mode.
    /// Returns `MemoryError::Conflict` at execution time if incompatible with `text`/`embedding`.
    #[must_use]
    pub const fn search_mode(mut self, mode: SearchMode) -> Self {
        self.search_mode = Some(mode);
        self
    }

    // --- Fact filters ---

    /// Filter by fact type.
    #[must_use]
    pub const fn fact_type(mut self, ft: FactType) -> Self {
        self.fact_type = Some(ft);
        self
    }

    /// Filter by minimum materialized importance score (not the base importance hint).
    #[must_use]
    pub const fn min_importance_score(mut self, threshold: f64) -> Self {
        self.min_importance_score = Some(threshold);
        self
    }

    /// Return only pinned (unforgettable) facts.
    #[must_use]
    pub const fn pinned_only(mut self) -> Self {
        self.pinned_only = true;
        self
    }

    // --- Diagnostics ---

    /// Enable the expired-facts probe for abstention diagnostics.
    ///
    /// When enabled, `execute_query` runs an additional FTS5 COUNT query
    /// against expired facts matching the search terms. Only effective when
    /// `text` is set — vector-only queries produce `expired_matches: None`.
    #[must_use]
    pub const fn include_expired_probe(mut self) -> Self {
        self.include_expired_probe = true;
        self
    }

    /// Enable brute-force archive search as a fallback after the normal search.
    ///
    /// Decompresses all `.pak` files in the archive directory and searches them
    /// with text substring matching and cosine similarity. Slow — use only when
    /// recall of archived facts is required.
    ///
    /// Requires the `archive` feature.
    #[cfg(feature = "archive")]
    #[must_use]
    pub const fn include_archives(mut self) -> Self {
        self.include_archives = true;
        self
    }

    // --- Pagination ---

    /// Set the maximum number of results. Default: 50.
    #[must_use]
    pub const fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- new() / Default ---

    #[test]
    fn new_produces_all_none_defaults() {
        let q = MemoryQuery::new();
        assert!(q.scope.is_none());
        assert!(q.period_start.is_none());
        assert!(q.period_end.is_none());
        assert!(q.text.is_none());
        assert!(q.embedding.is_none());
        assert!(q.search_mode.is_none());
        assert!(q.fact_type.is_none());
        assert!(q.min_importance_score.is_none());
        assert!(!q.pinned_only);
        assert!(q.limit.is_none());
        assert!(q.valid_at.is_none());
        assert!(!q.include_expired_probe);
    }

    #[test]
    fn new_equals_default() {
        let new = MemoryQuery::new();
        let def = MemoryQuery::default();
        // Both should have identical field values
        assert_eq!(format!("{new:?}"), format!("{def:?}"));
    }

    // --- effective_limit ---

    #[test]
    fn effective_limit_returns_default_when_unset() {
        assert_eq!(MemoryQuery::new().effective_limit(), DEFAULT_LIMIT);
    }

    #[test]
    fn effective_limit_returns_set_value() {
        let q = MemoryQuery::new().limit(10);
        assert_eq!(q.effective_limit(), 10);
    }

    #[test]
    fn effective_limit_respects_zero() {
        let q = MemoryQuery::new().limit(0);
        assert_eq!(q.effective_limit(), 0);
    }

    // --- has_search ---

    #[test]
    fn has_search_false_by_default() {
        assert!(!MemoryQuery::new().has_search());
    }

    #[test]
    fn has_search_true_with_text() {
        assert!(MemoryQuery::new().text("hello").has_search());
    }

    #[test]
    fn has_search_true_with_embedding() {
        assert!(MemoryQuery::new().embedding(vec![1.0]).has_search());
    }

    #[test]
    fn has_search_true_with_both() {
        let q = MemoryQuery::new().text("hello").embedding(vec![1.0]);
        assert!(q.has_search());
    }

    // --- has_period ---

    #[test]
    fn has_period_false_by_default() {
        assert!(!MemoryQuery::new().has_period());
    }

    #[test]
    fn has_period_true_with_period() {
        let now = chrono::Utc::now();
        let start = now - chrono::Duration::hours(1);
        let q = MemoryQuery::new().period(start, now);
        assert!(q.has_period());
        assert_eq!(q.period_start, Some(start));
        assert_eq!(q.period_end, Some(now));
    }

    // --- Scope builders ---

    #[test]
    fn scope_exact_sets_exact_variant() {
        let q = MemoryQuery::new().scope_exact("user:alice");
        assert_eq!(q.scope, Some(ScopeQuery::Exact("user:alice".into())));
    }

    #[test]
    fn scope_subtree_sets_subtree_variant() {
        let q = MemoryQuery::new().scope_subtree("project:demo");
        assert_eq!(q.scope, Some(ScopeQuery::Subtree("project:demo".into())));
    }

    #[test]
    fn scope_ancestors_sets_ancestors_variant() {
        let q = MemoryQuery::new().scope_ancestors("deep/path");
        assert_eq!(q.scope, Some(ScopeQuery::Ancestors("deep/path".into())));
    }

    #[test]
    fn scope_inherited_sets_inherited_variant() {
        let q = MemoryQuery::new().scope_inherited("ctx");
        assert_eq!(q.scope, Some(ScopeQuery::Inherited("ctx".into())));
    }

    #[test]
    fn scope_last_wins() {
        let q = MemoryQuery::new().scope_exact("a").scope_subtree("b");
        assert_eq!(q.scope, Some(ScopeQuery::Subtree("b".into())));
    }

    // --- Temporal builders ---

    #[test]
    fn valid_at_sets_point_in_time() {
        let now = chrono::Utc::now();
        let q = MemoryQuery::new().valid_at(now);
        assert_eq!(q.valid_at, Some(now));
    }

    // --- Fact filter builders ---

    #[test]
    fn fact_type_sets_filter() {
        let q = MemoryQuery::new().fact_type(FactType::Episodic);
        assert_eq!(q.fact_type, Some(FactType::Episodic));
    }

    #[test]
    fn min_importance_score_sets_threshold() {
        let q = MemoryQuery::new().min_importance_score(0.7);
        assert!((q.min_importance_score.unwrap() - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn pinned_only_sets_flag() {
        let q = MemoryQuery::new().pinned_only();
        assert!(q.pinned_only);
    }

    #[test]
    fn include_expired_probe_sets_flag() {
        let q = MemoryQuery::new().include_expired_probe();
        assert!(q.include_expired_probe);
    }

    #[test]
    fn search_mode_sets_override() {
        let q = MemoryQuery::new().search_mode(SearchMode::Hybrid);
        assert_eq!(q.search_mode, Some(SearchMode::Hybrid));
    }

    // --- Builder chaining preserves fields ---

    #[test]
    fn chaining_preserves_all_fields() {
        let now = chrono::Utc::now();
        let q = MemoryQuery::new()
            .scope_exact("s")
            .text("hello")
            .embedding(vec![1.0, 2.0])
            .fact_type(FactType::Procedural)
            .min_importance_score(0.5)
            .pinned_only()
            .limit(10)
            .valid_at(now)
            .include_expired_probe()
            .search_mode(SearchMode::Fts);

        assert_eq!(q.scope, Some(ScopeQuery::Exact("s".into())));
        assert_eq!(q.text.as_deref(), Some("hello"));
        assert_eq!(q.embedding, Some(vec![1.0, 2.0]));
        assert_eq!(q.fact_type, Some(FactType::Procedural));
        assert!((q.min_importance_score.unwrap() - 0.5).abs() < f64::EPSILON);
        assert!(q.pinned_only);
        assert_eq!(q.limit, Some(10));
        assert_eq!(q.valid_at, Some(now));
        assert!(q.include_expired_probe);
        assert_eq!(q.search_mode, Some(SearchMode::Fts));
        assert_eq!(q.effective_limit(), 10);
        assert!(q.has_search());
        assert!(!q.has_period());
    }
}
