//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.
//!
//! `ann`/`filter_sql`/`fts`/`strategy`/`vector` (the FTS5/vector search cores
//! `SqliteBackend` drives) carved into [`me_backend_sqlite`] (Wave 2 #816 / S2,
//! sub-PR 2b) along with `storage::sqlite` (their only consumer beyond `hybrid`/
//! `query`, moved in the same commit). `hybrid`/`query` (the RRF merge + port-driven
//! hybrid search + the `MemoryQuery` builder) carved into [`me_query`] in turn
//! (Wave 2 #816 / S4, sub-PR 2); the facade's `engine/query.rs` now calls
//! `me_query::execute::{query, execute_query}` directly rather than routing through a
//! `crate::search::hybrid` re-export, so `hybrid` has no facade call site left and is
//! dropped. `query` (the module holding `MemoryQuery`) is dropped too, one layer
//! further down (Wave 2 #816 / S4, sub-PR 3a): `MemoryQuery` relocated from `me-query`
//! to `me-types` (a pure data + builder DTO with zero `me-query`-internal
//! dependencies, sitting beside its sibling search vocabulary), so it is now
//! re-exported directly from `crate::types::search` below rather than through a
//! `query` submodule; internal callers (`engine/archive.rs`/`engine/query.rs`/
//! `engine/tests.rs`/`archive/search.rs`) reach it as `crate::search::MemoryQuery`.
//! `ann`/`filter_sql` and the `FilterSql`/`fts_count_expired`/
//! `fts_search_filtered`/`vector_search_filtered` flat re-exports are NOT re-exported
//! here — their only callers (`storage::sqlite::{search_index, convert}`) moved to
//! `me-backend-sqlite` too, so nothing in the facade reaches them anymore (production
//! or test). `fts` itself is re-exported only under `cfg(fuzzing)` (Wave 2 #816 / S4,
//! sub-PR 2): its last non-fuzz consumer was the sync `hybrid_search` twin deleted in
//! the `me-query` carve, so a normal build no longer reaches it — only `lib.rs`'s
//! `fuzz_seam::fuzz_fts_query` still does. `VectorSearchStrategy` is NOT re-exported
//! (Wave 2 #816 / S4, sub-PR 2, API-break #2 superseded — see ADR-0018): it is now a
//! `me-backend-sqlite`-internal dispatch trait (`HnswStrategy` vs `BruteForce`) that
//! never crosses the port boundary, and the facade has zero remaining consumers of it
//! (verified: `grep -rn VectorSearchStrategy src` matches only this note).
#[cfg(fuzzing)]
pub(crate) use me_backend_sqlite::search::fts;
pub(crate) use me_backend_sqlite::search::{strategy, vector};

// --- Genuinely-public surface (re-exported at the crate root by `lib.rs`) ---
//
// These are the only `pub` items the `search` module exposes. Everything else
// below is an impl-internal helper kept `pub(crate)` so it stays reachable from
// other engine modules without leaking onto the public API (#365).
pub use crate::types::search::{
    MatchType, MemoryQuery, QueryDiagnostics, QueryResponse, SearchMode, SearchQuery, SearchResult,
};

// `SearchConfig` and `cosine_similarity` are not part of the engine's facade
// API, but the top-level benches/tests (which compile as separate crates and
// therefore cannot see `pub(crate)`) consume them via `memory_engine::search::*`.
// They are kept `pub` for those out-of-crate consumers only.
pub use strategy::SearchConfig;
pub use vector::cosine_similarity;
