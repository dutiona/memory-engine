use crate::error::Result;
use crate::search::query::MemoryQuery;
use crate::types::search::{QueryDiagnostics, QueryResponse, SearchQuery, SearchResult};

use super::MemoryEngine;

impl MemoryEngine {
    /// Query facts using hybrid search (FTS5 + vector + RRF).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` on query failure.
    /// Returns `MemoryError::Reranker` if a configured [`Reranker`](crate::traits::Reranker)
    /// fails or returns an invalid permutation.
    ///
    /// # Panics
    ///
    /// Panics if internal candidate de-duplication yields an inconsistent
    /// index — an invariant that should never be violated in practice.
    pub async fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        me_query::execute::query(
            self.mem_ctx(),
            &self.scope_tree,
            self.reranker.as_ref(),
            query,
        )
        .await
    }

    /// Execute a composed query using the [`MemoryQuery`] builder.
    ///
    /// Routes to hybrid search (FTS + vector) when text/embedding is present,
    /// or to store-level queries (importance, pinned, period) otherwise.
    /// All code paths enforce the temporal safety invariant: future-dated facts
    /// (`t_valid > now`) are excluded unless overridden by `valid_at` or `period`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if:
    /// - `period_start` is set without `period_end` (or vice versa)
    /// - `valid_at` and `period` are both set (mutually exclusive)
    /// - `search_mode` conflicts with available text/embedding inputs
    ///
    /// Returns `MemoryError::Storage` on query failure.
    pub async fn execute_query(&self, query: &MemoryQuery) -> Result<QueryResponse> {
        // #117: a provided-but-missing scope yields no results — and the archive fallback
        // below (itself unscoped) MUST be skipped, or it would leak cross-scope archived
        // facts. In the pre-carve engine this was guaranteed structurally (the scope-miss
        // early-return preceded `execute_search_path`, where the archive block lived); the
        // carve carries it explicitly via `QueryExecution::ScopeMissing`, which the `match`
        // below cannot fall through — matching the base's empty-results behaviour exactly.
        // `mut` is only exercised by the `#[cfg(feature = "archive")]` block —
        // a `backend-sqlite`-only (archive-off) build never mutates `response`.
        #[cfg_attr(not(feature = "archive"), allow(unused_mut))]
        let mut response =
            match me_query::execute_query(self.mem_ctx(), &self.scope_tree, query).await? {
                me_query::QueryExecution::ScopeMissing => {
                    return Ok(QueryResponse {
                        results: vec![],
                        diagnostics: QueryDiagnostics::default(),
                    });
                }
                me_query::QueryExecution::Executed(response) => response,
            };

        // Archive fallback (opt-in, best-effort) — kept in the facade (not moved to
        // `me-query`, #816 / S4 sub-PR 2): `search_archives_fallback` reaches
        // `MemoryEngine`-only state (`self.db_path`/`self.cold`) that `MemoryCtx`
        // does not expose. Gated on the two conditions the original inline
        // `execute_search_path` enforced structurally: (a) the search path was taken
        // (text/embedding present) — re-guarded with `query.has_search()`; and (b) scope
        // resolution did not scope-miss (#117) — guaranteed above by only reaching this
        // point on the `Executed` arm.
        #[cfg(feature = "archive")]
        if query.has_search() && query.include_archives {
            let limit = query.effective_limit();
            match self.search_archives_fallback(query, limit).await {
                Ok(Some(archive_results)) => {
                    response.diagnostics.archive_paks_scanned = archive_results.paks_scanned;
                    response.diagnostics.archive_search_ms = archive_results.search_ms;
                    response.results.extend(archive_results.results);
                    response.results.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    response.results.truncate(limit);
                }
                Ok(None) => {}
                Err(e) => {
                    // Archive search is best-effort; silently skip on error.
                    tracing::warn!("archive search fallback failed: {e}");
                }
            }
        }

        Ok(response)
    }
}
