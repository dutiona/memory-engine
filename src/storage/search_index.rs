//! Lexical + vector retrieval — the seam's crown jewel (all variance hides inside
//! the impls).
//!
//! `lexical_search`/`vector_search` return **ranked scored pairs `(id, score)`**,
//! best-first. RRF (`src/search/hybrid.rs`, engine-side, untouched) fuses by **rank
//! position only** — it ignores the score — so the cross-backend bm25-vs-`ts_rank`
//! incommensurability stays invisible to fusion (the property that lets all tiers
//! satisfy one trait). Single-channel FTS-only / vector-only query modes keep the
//! user-visible score (surfaced in the CLI and MCP). Scores are **backend-native,
//! not cross-comparable** — use rank for cross-backend reasoning.
//!
//! **Query parsing is backend-owned**: the engine passes the raw user string; each
//! impl parses to its dialect (FTS5 `MATCH`, Postgres `websearch_to_tsquery` /
//! `@@@`). No query syntax crosses the seam. A malformed query yields an empty
//! result, not an error (mirrors today's FTS5-syntax swallow). The
//! brute-force-vs-HNSW choice and `ann.rs`'s `#[cfg(feature = "ann")]` are an impl
//! detail the trait never sees.

use async_trait::async_trait;

use crate::error::Result;
use crate::storage::FactFilter;
use crate::types::FactType;

/// Lexical + vector retrieval, returning ranked `(fact_id, score)` pairs.
///
/// # Errors
/// [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a backend
/// failure (a malformed query is *not* an error — it returns an empty result).
#[async_trait]
pub trait SearchIndex: Send + Sync {
    /// Lexical retrieval — up to `k` `(fact_id, score)` pairs, best-first.
    async fn lexical_search(
        &self,
        query: &str,
        filter: &FactFilter,
        k: usize,
    ) -> Result<Vec<(i64, f64)>>;
    /// Vector retrieval — up to `k` `(fact_id, score)` pairs, nearest-first.
    ///
    /// `embedding` must have the backend's configured dimension; a slice of the
    /// wrong length (including empty) is a
    /// [`MemoryError::EmbeddingDimension`](crate::error::MemoryError::EmbeddingDimension),
    /// not an empty result.
    async fn vector_search(
        &self,
        embedding: &[f32],
        filter: &FactFilter,
        k: usize,
    ) -> Result<Vec<(i64, f64)>>;
    /// Count facts matching the lexical query that are **expired**
    /// (`t_expired IS NOT NULL`) — the `diagnostics.expired_matches` probe
    /// (transcribes `fts_count_expired`).
    ///
    /// Takes only the parameters it honors (`fact_type` + `scope_ids`); the
    /// expired temporal predicate is intrinsic to this probe, so — unlike the
    /// search methods — it deliberately does **not** accept a [`FactFilter`] (whose
    /// `temporal`/`ids`/`pinned`/`metadata` would be silently meaningless here).
    async fn lexical_count_expired(
        &self,
        query: &str,
        fact_type: Option<&FactType>,
        scope_ids: Option<&[i64]>,
    ) -> Result<usize>;
}
