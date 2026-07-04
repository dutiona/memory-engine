//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.
//!
//! `ann`/`filter_sql`/`fts`/`strategy`/`vector` (the FTS5/vector search cores
//! `SqliteBackend` drives) carved into [`me_backend_sqlite`] (Wave 2 #816 / S2,
//! sub-PR 2b) along with `storage::sqlite` (their only consumer beyond `hybrid`/
//! `query`, moved in the same commit). `fts`/`strategy`/`vector` are re-exported below
//! (mirroring the 2a `store`/`pool` seam) so `hybrid`/`query` (staying) and the crate's
//! public API keep resolving unchanged; `ann`/`filter_sql` and the `FilterSql`/
//! `fts_count_expired`/`fts_search_filtered`/`vector_search_filtered` flat re-exports
//! are NOT re-exported here — their only callers (`storage::sqlite::{search_index,
//! convert}`) moved to `me-backend-sqlite` too, so nothing in the facade reaches them
//! anymore (production or test).
pub(crate) use me_backend_sqlite::search::{fts, strategy, vector};
pub(crate) mod hybrid;
pub(crate) mod query;

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
