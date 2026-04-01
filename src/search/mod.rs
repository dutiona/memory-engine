//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.

#[cfg(feature = "ann")]
pub mod ann;
pub mod fts;
pub mod hybrid;
pub mod query;
pub mod strategy;
pub mod vector;

#[cfg(feature = "ann")]
pub use ann::HnswStrategy;
pub use fts::{FtsResult, fts_count_expired, fts_search};
pub use hybrid::{
    MatchType, QueryDiagnostics, QueryResponse, SearchMode, SearchQuery, SearchResult,
    hybrid_search, rrf_merge,
};
pub use query::MemoryQuery;
pub use strategy::{BruteForce, SearchConfig, VectorSearchStrategy};
pub use vector::{VectorResult, cosine_similarity, vector_search};
