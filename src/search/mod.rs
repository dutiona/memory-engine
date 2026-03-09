//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.

pub mod fts;
pub mod hybrid;
pub mod vector;

pub use fts::{FtsResult, fts_search};
pub use hybrid::{MatchType, SearchMode, SearchQuery, SearchResult, hybrid_search, rrf_merge};
pub use vector::{VectorResult, cosine_similarity, vector_search};
