//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.

#[cfg(feature = "ann")]
pub(crate) mod ann;
pub(crate) mod filter_sql;
pub(crate) mod fts;
pub(crate) mod hybrid;
pub(crate) mod query;
pub(crate) mod strategy;
pub(crate) mod vector;

#[cfg(feature = "ann")]
pub use ann::HnswStrategy;
pub use filter_sql::FilterSql;
pub use fts::{FtsResult, fts_count_expired, fts_search, fts_search_filtered};
pub use hybrid::{
    MatchType, QueryDiagnostics, QueryResponse, RRF_K, SearchMode, SearchQuery, SearchResult,
    hybrid_search, rrf_merge,
};
pub use query::MemoryQuery;
pub use strategy::{BruteForce, SearchConfig, VectorSearchStrategy};
pub use vector::{VectorResult, cosine_similarity, vector_search, vector_search_filtered};

/// Serialize an optional scope-ID slice to a JSON string for use as a SQL
/// parameter in `json_each(?N)` expressions.
///
/// Returns `None` when `scope_ids` is `None` (no scope filter), letting SQL
/// treat the parameter as NULL and skip the filter clause.
///
/// # Errors
///
/// Returns `MemoryError::Serialization` if `serde_json::to_string` fails,
/// which cannot happen for `&[i64]` in practice.
pub(crate) fn serialize_scope_ids(
    scope_ids: Option<&[i64]>,
) -> crate::error::Result<Option<String>> {
    scope_ids
        .map(serde_json::to_string)
        .transpose()
        .map_err(crate::error::MemoryError::Serialization)
}
