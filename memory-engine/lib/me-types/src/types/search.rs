//! Search/query result vocabulary — the DTOs the retrieval surface speaks.
//!
//! Pure data types (no `rusqlite`, no search logic): the query input
//! (`SearchQuery`), the result (`SearchResult`/`MatchType`), the response
//! (`QueryResponse`/`QueryDiagnostics`), the `SearchMode` selector, and the
//! `RRF_K` fusion constant. Homed in `me-types` (Wave 2 #816) so the consumer
//! traits (`Reranker` returns `Vec<SearchResult>`), the backend, and the query
//! crate all share one definition. The fusion *logic* (`rrf_merge`, hybrid search)
//! stays in the retrieval layer.

use chrono::{DateTime, Utc};

use crate::types::{Fact, FactType};

/// How to combine search sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Lexical-only: FTS5 BM25 ranking over the query text. Requires `text`;
    /// ignores any `embedding`. Best for exact-term and keyword recall.
    Fts,
    /// Semantic-only: cosine similarity over the query `embedding`. Requires
    /// `embedding`; ignores any `text`. Best for paraphrase and concept recall.
    Vector,
    /// Both lexical and semantic, fused with Reciprocal Rank Fusion (see
    /// `rrf_merge`). Uses whichever of `text`/`embedding` are present. The
    /// default, balancing keyword precision with semantic recall.
    Hybrid,
}

/// Which source(s) contributed to a result.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MatchType {
    Fts,
    Vector,
    Both,
    /// Result came from importance-ranked store query (no text/vector search).
    ImportanceRank,
    /// Result came from a decompressed archive `.pak` file (slow fallback).
    /// Always present in the enum for serde ABI stability across feature combinations.
    Archive,
}

/// A unified search query across FTS5 and vector sources.
///
/// Construct with the fluent builder rather than a struct literal: the type is
/// `#[non_exhaustive]`, so struct-literal construction is forbidden outside this
/// crate. Start from [`SearchQuery::new`] (the two required fields — `mode` and
/// `limit`) and chain the optional setters:
///
/// ```ignore
/// let q = SearchQuery::new(SearchMode::Hybrid, 10)
///     .text("rust ownership")
///     .fact_type(FactType::Semantic);
/// ```
///
/// This mirrors `MemoryQuery`'s builder style and is
/// misuse- and forward-compatibility-resistant: adding a field is a non-breaking
/// change for callers, who never had to spell out the `None` defaults.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct SearchQuery {
    // Fields are `pub` (widened from `pub(crate)` in Wave 2 #816): the retrieval
    // layer — `me-query`'s hybrid search and the SQLite backend's strategies — reads
    // and writes them directly, now across the crate boundary the `me-types` carve
    // introduced. `#[non_exhaustive]` still forbids external struct-literal
    // construction, so callers must start from the builder (`SearchQuery::new(..)` +
    // the chainable setters); the widening only exposes the fields of an
    // already-constructed query.
    pub text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub mode: SearchMode,
    pub limit: usize,
    /// How many candidates to pass to the reranker before truncating to `limit`.
    /// Clamped to at least `limit` — can only widen the candidate pool, never shrink it.
    /// When `None`, falls back to `limit` (no over-fetch).
    pub rerank_depth: Option<usize>,
    pub valid_at: Option<DateTime<Utc>>,
    pub fact_type: Option<FactType>,
    pub scope: Option<crate::types::ScopeQuery>,
}

impl SearchQuery {
    /// Create a query for the given [`SearchMode`] and result `limit`.
    ///
    /// `mode` and `limit` are the only required parameters; all other filters
    /// default to `None`. Refine the query with the chainable setters
    /// ([`text`](Self::text), [`embedding`](Self::embedding), etc.).
    #[must_use]
    pub const fn new(mode: SearchMode, limit: usize) -> Self {
        Self {
            text: None,
            embedding: None,
            mode,
            limit,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        }
    }

    /// Set the FTS5 query text.
    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Set the embedding vector for vector similarity search.
    #[must_use]
    pub fn embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    /// Override the search mode.
    #[must_use]
    pub const fn mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    /// Override the result limit.
    #[must_use]
    pub const fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set how many candidates to over-fetch for the reranker before truncating
    /// to `limit`. Clamped to at least `limit` at query time.
    #[must_use]
    pub const fn rerank_depth(mut self, depth: usize) -> Self {
        self.rerank_depth = Some(depth);
        self
    }

    /// Set the point-in-time temporal cutoff.
    #[must_use]
    pub const fn valid_at(mut self, at: DateTime<Utc>) -> Self {
        self.valid_at = Some(at);
        self
    }

    /// Filter by fact type.
    #[must_use]
    pub const fn fact_type(mut self, fact_type: FactType) -> Self {
        self.fact_type = Some(fact_type);
        self
    }

    /// Filter by scope.
    #[must_use]
    pub fn scope(mut self, scope: crate::types::ScopeQuery) -> Self {
        self.scope = Some(scope);
        self
    }
}

/// A search result with the full fact, combined score, and match source.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub fact: Fact,
    pub score: f64,
    pub match_type: MatchType,
}

/// Diagnostic signals from a query execution, enabling consumer-side
/// abstention classification.
///
/// The engine computes mechanical retrieval signals. The consumer interprets
/// them alongside content understanding to classify the four abstention types
/// (Retrieval / Evidence / Reasoning / Decay) from research note 18.
///
/// ## Interpreting `expired_matches`
///
/// `expired_matches` counts ALL expired facts matching the query, regardless
/// of expiry reason (Ebbinghaus decay, conflict resolution, deduplication).
/// The engine does not currently track expiry provenance — `t_expired` is a
/// generic tombstone. Consumers wanting true decay-only counts should
/// cross-reference with `ExpiredReason` via `explain_fact()`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct QueryDiagnostics {
    /// Total candidates found before post-filters (temporal, importance, pinned).
    pub candidates_before_filter: usize,
    /// Total results returned after all filters and truncation.
    pub results_returned: usize,
    /// Number of expired facts matching the FTS5 query text with the same
    /// `fact_type` and `scope` filters. Does NOT apply `min_importance_score`
    /// or `pinned_only` — the probe answers "how many expired facts match
    /// the search terms?" not "how many would survive the full filter chain?"
    ///
    /// `None` = probe not run (opt-in via `include_expired_probe`).
    /// `None` also when query is vector-only (no FTS5 terms to probe).
    pub expired_matches: Option<usize>,
    /// Number of FTS candidates before merge.
    pub fts_candidates: usize,
    /// Number of vector candidates before merge.
    pub vector_candidates: usize,
    /// Number of archive `.pak` files scanned. `0` when archives not searched.
    #[cfg(feature = "archive")]
    pub archive_paks_scanned: usize,
    /// Total milliseconds spent decompressing and searching archives.
    #[cfg(feature = "archive")]
    pub archive_search_ms: u64,
}

/// Complete query response including results and diagnostic metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryResponse {
    pub results: Vec<SearchResult>,
    pub diagnostics: QueryDiagnostics,
}

/// Default RRF smoothing constant (Cormack & Clarke, 2009).
///
/// k=60 is the value recommended in the original RRF paper and widely adopted
/// in practice. It controls rank-score attenuation: larger k = slower decay.
pub const RRF_K: u32 = 60;
