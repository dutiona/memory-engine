//! Hybrid search's backend half: FTS5 (BM25) + vector (cosine), relocated below the seam
//! (Wave 2 #816 / S2, sub-PR 2b).
//!
//! `hybrid`/`query` (the RRF merge + the `MemoryQuery` API) stay in the facade's `search`
//! module, which re-exports the whole tree here (`pub(crate) use
//! me_backend_sqlite::search::{ann, filter_sql, fts, strategy, vector};`, mirroring the
//! 2a `store`/`pool` seam) so every existing `crate::search::<submodule>::*` path in the
//! facade keeps resolving unchanged.

#[cfg(feature = "ann")]
pub mod ann;
pub mod filter_sql;
pub mod fts;
pub mod strategy;
pub mod vector;

// --- Flat re-exports: every name the moved backend files (and the facade's remaining
// `hybrid`/`query`, via the module alias above) reach as `crate::search::X` / `[facade
// crate]::search::X` rather than the fully-qualified submodule path. ---
pub use filter_sql::FilterSql;
pub use fts::{fts_count_expired, fts_search_filtered};
pub use strategy::{SearchConfig, VectorSearchStrategy};
pub use vector::{cosine_similarity, vector_search_filtered};

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
pub fn serialize_scope_ids(scope_ids: Option<&[i64]>) -> me_types::error::Result<Option<String>> {
    scope_ids
        .map(serde_json::to_string)
        .transpose()
        .map_err(me_types::error::MemoryError::Serialization)
}
