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

    /// Rebuild the in-process vector index from the current active facts, after a
    /// **same-dim** reconstruction promote (#624).
    ///
    /// A same-dim reconstruction rewrites every `facts.embedding` under a new
    /// embedding model without changing the dimension, so the engine keeps serving on
    /// the same handle (no reopen). A backend that maintains a *live in-process* ANN
    /// index (`SQLite` + `ann`) must therefore rebuild it here, because the index was
    /// built on the now-replaced vectors. (A *different-dim* reconstruction fences the
    /// handle and the consumer reopens, rebuilding on open — #742 — so it never calls
    /// this.)
    ///
    /// ## Contract — backends with a live index MUST override
    /// The default is a **no-op**, correct ONLY for backends that need no in-process
    /// rebuild: the brute-force `SQLite` path reads `facts.embedding` directly per query
    /// (already correct the instant the promote commits), and a server-side index (a
    /// future Postgres/`pgvector` backend, #633) is maintained by the database. **Any
    /// backend that holds a live in-memory similarity index MUST override this** —
    /// inheriting the no-op would silently serve stale vectors after a model swap.
    /// (The #632 conformance suite is the place to assert post-swap query correctness
    /// so a non-overriding backend fails loudly.)
    ///
    /// ## Concurrency
    /// An implementation MUST rebuild atomically: a concurrent `vector_search` must
    /// observe either the entire old or the entire new index, never a partial graph,
    /// and a concurrent index mutation (insert/expire) must not be lost to the swap.
    ///
    /// ## Similarity-edge invalidation (N/A in this engine — canonical note)
    /// Issue #624 also asked to "invalidate cached similarity graph edges (the
    /// analog of the Knowledge layer deleting `relation_type = "similar"`)." **The
    /// Memory layer persists no such edges.** Every graph edge is *semantic
    /// provenance* — `co_session` / `supersedes` / `supplements` / `contradicts`,
    /// i.e. session links and arbiter decisions — which encode real history and MUST
    /// survive a model swap. Vector similarity is computed transiently (query-time RRF
    /// fusion, consolidation/DBSCAN clustering, on-the-fly resonance), never
    /// materialized as edges. The **only** materialized embedding-similarity cache is
    /// this vector index itself, so "invalidate cached similarity edges" collapses to
    /// "rebuild the index" — the same single action this method performs, not a
    /// second edge-deletion step. A persisted associative similarity graph (for
    /// spreading-activation recall) is a possible *future* cognitive-layer feature; if
    /// it lands, its invalidation slots in alongside this call at the reconstruction
    /// seam.
    ///
    /// # Errors
    /// [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a backend
    /// failure, or
    /// [`MemoryError::EmbeddingDimension`](crate::error::MemoryError::EmbeddingDimension)
    /// if a stored embedding has the wrong width.
    async fn rebuild_vector_index(&self) -> Result<()> {
        Ok(())
    }
}
