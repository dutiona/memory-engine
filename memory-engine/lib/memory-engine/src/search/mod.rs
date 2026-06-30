//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.

#[cfg(feature = "ann")]
pub(crate) mod ann;
pub(crate) mod filter_sql;
pub(crate) mod fts;
pub(crate) mod hybrid;
pub(crate) mod query;
pub(crate) mod strategy;
pub(crate) mod vector;

// --- Genuinely-public surface (re-exported at the crate root by `lib.rs`) ---
//
// These are the only `pub` items the `search` module exposes. Everything else
// below is an impl-internal helper kept `pub(crate)` so it stays reachable from
// other engine modules without leaking onto the public API (#365).
pub use crate::types::search::{
    MatchType, QueryDiagnostics, QueryResponse, SearchMode, SearchQuery, SearchResult,
};
pub use query::MemoryQuery;
pub use strategy::VectorSearchStrategy;

// `SearchConfig` and `cosine_similarity` are not part of the engine's facade
// API, but the top-level benches/tests (which compile as separate crates and
// therefore cannot see `pub(crate)`) consume them via `memory_engine::search::*`.
// They are kept `pub` for those out-of-crate consumers only.
pub use strategy::SearchConfig;
pub use vector::cosine_similarity;

// --- Impl-internal helpers: crate-private, never reachable as `memory_engine::search::*` ---
//
// Only the helpers that engine modules reach through the `crate::search::*`
// facade path are re-exported here; the rest are addressed via their
// `crate::search::<submodule>::*` paths (e.g. `fts::fts_search`,
// `strategy::BruteForce`, `ann::HnswStrategy`), so a flat re-export would be a
// dead import.
pub(crate) use filter_sql::FilterSql;
pub(crate) use fts::{fts_count_expired, fts_search_filtered};
pub(crate) use vector::vector_search_filtered;

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
