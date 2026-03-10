//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.

pub mod fts;
pub mod hybrid;
pub mod strategy;
pub mod vector;

pub use fts::{fts_search, FtsResult};
pub use hybrid::{hybrid_search, rrf_merge, MatchType, SearchMode, SearchQuery, SearchResult};
pub use strategy::{BruteForce, SearchConfig, VectorSearchStrategy};
pub use vector::{cosine_similarity, vector_search, VectorResult};
