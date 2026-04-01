use chrono::{DateTime, Utc};

use crate::error::{MemoryError, Result};
use crate::search::hybrid::{
    hybrid_search, QueryDiagnostics, QueryResponse, SearchMode, SearchQuery, SearchResult,
};
use crate::search::query::MemoryQuery;
use crate::search::strategy::VectorSearchStrategy;
use crate::store::facts::FactStore;
use crate::types::Fact;

use super::{fact_overlaps_period, fact_to_search_result, passes_temporal_cutoff, MemoryEngine};

impl MemoryEngine {
    /// Query facts using hybrid search (FTS5 + vector + RRF).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        // Resolve scope IDs from cache (short-lived read lock).
        // When a scope query is provided but the path doesn't exist,
        // return empty results instead of silently falling through to unscoped search.
        let scope_ids: Option<Vec<i64>> = match &query.scope {
            Some(sq) => {
                let resolved = self.scope_tree.read().resolve_query(sq);
                match resolved {
                    Some(ids) => Some(ids),
                    None => return Ok(vec![]), // scope doesn't exist → no results
                }
            }
            None => None,
        };

        #[cfg(feature = "ann")]
        let strategy: &dyn VectorSearchStrategy = if self.should_use_hnsw() {
            self.hnsw_strategy.as_ref().unwrap()
        } else {
            &*self.vector_strategy
        };
        #[cfg(not(feature = "ann"))]
        let strategy: &dyn VectorSearchStrategy = &*self.vector_strategy;

        let (mut results, _diagnostics) = self.with_read(|conn| {
            hybrid_search(conn, query, self.embed_dim, scope_ids.as_deref(), strategy)
        })?;

        // Apply reranker if present and query has text (cross-encoder needs query text).
        // Runs OUTSIDE the read lock — reranking may involve slow inference/API calls.
        if let (Some(reranker), Some(text)) = (&self.reranker, &query.text) {
            let ranked = reranker.rerank(text, &results)?;
            Self::validate_reranker_output(results.len(), &ranked)?;
            // Reconstruct results from indices — move, don't clone (#144).
            // Safe: validate_reranker_output guarantees unique indices.
            let mut candidates: Vec<_> = results.into_iter().map(Some).collect();
            results = ranked
                .into_iter()
                .map(|(idx, score)| {
                    let mut r = candidates[idx].take().expect("index validated as unique");
                    r.score = score;
                    r
                })
                .collect();
        }

        // Always truncate to limit — rerank_depth may have over-fetched from hybrid_search.
        results.truncate(query.limit);

        Ok(results)
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
    /// Returns `MemoryError::Database` on query failure.
    pub fn execute_query(&self, query: &MemoryQuery) -> Result<QueryResponse> {
        // --- Validation ---
        self.validate_memory_query(query)?;

        // --- Resolve scope ---
        let scope_ids: Option<Vec<i64>> = match &query.scope {
            Some(sq) => {
                let resolved = self.scope_tree.read().resolve_query(sq);
                match resolved {
                    Some(ids) => Some(ids),
                    None => {
                        return Ok(QueryResponse {
                            results: vec![],
                            diagnostics: QueryDiagnostics::default(),
                        });
                    }
                }
            }
            None => None,
        };

        // --- Compute effective temporal cutoff ---
        // Default: Utc::now() to hide future-dated facts (scheduling model invariant).
        // Overridden by explicit valid_at. Disabled when period is set (period handles its own semantics).
        let effective_cutoff = if query.valid_at.is_some() {
            query.valid_at
        } else if query.has_period() {
            None // period handles temporal semantics itself
        } else {
            Some(Utc::now()) // default: hide future-dated facts
        };

        let limit = query.effective_limit();

        if query.has_search() {
            self.execute_search_path(query, scope_ids.as_deref(), effective_cutoff, limit)
        } else {
            self.execute_store_path(query, scope_ids.as_deref(), effective_cutoff, limit)
        }
    }

    /// Validate a `MemoryQuery` for conflicting or incomplete options.
    fn validate_memory_query(&self, query: &MemoryQuery) -> Result<()> {
        // Period: both start and end must be set, or neither
        if query.period_start.is_some() != query.period_end.is_some() {
            return Err(MemoryError::Conflict(
                "period_start and period_end must both be set or both unset".into(),
            ));
        }

        // valid_at and period are mutually exclusive
        if query.valid_at.is_some() && query.has_period() {
            return Err(MemoryError::Conflict(
                "valid_at and period are mutually exclusive".into(),
            ));
        }

        // Search mode compatibility (D7)
        if let Some(ref mode) = query.search_mode {
            match mode {
                SearchMode::Fts if query.text.is_none() => {
                    return Err(MemoryError::Conflict(
                        "SearchMode::Fts requires text to be set".into(),
                    ));
                }
                SearchMode::Vector if query.embedding.is_none() => {
                    return Err(MemoryError::Conflict(
                        "SearchMode::Vector requires embedding to be set".into(),
                    ));
                }
                SearchMode::Hybrid if query.text.is_none() || query.embedding.is_none() => {
                    return Err(MemoryError::Conflict(
                        "SearchMode::Hybrid requires both text and embedding".into(),
                    ));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Infer search mode from available inputs (D7).
    fn infer_search_mode(query: &MemoryQuery) -> SearchMode {
        if let Some(ref mode) = query.search_mode {
            return mode.clone();
        }
        match (query.text.is_some(), query.embedding.is_some()) {
            (true, true) => SearchMode::Hybrid,
            (true, false) => SearchMode::Fts,
            (false, true) => SearchMode::Vector,
            (false, false) => unreachable!("has_search() should be false"),
        }
    }

    /// Search path: delegate to hybrid_search, then post-filter.
    ///
    /// When post-filters are active (period, importance, pinned), we pass the
    /// raw `limit` (not inflated) because `hybrid_search` already does its own
    /// internal 3x overfetch before its temporal filter.
    fn execute_search_path(
        &self,
        query: &MemoryQuery,
        scope_ids: Option<&[i64]>,
        effective_cutoff: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<QueryResponse> {
        let mode = Self::infer_search_mode(query);

        // When period is set, effective_cutoff is None — but hybrid_search defaults
        // valid_at=None to Utc::now(), which would hide facts outside the current
        // instant. We need hybrid_search to return ALL temporally-viable candidates
        // so our period post-filter can apply exact interval overlap semantics.
        // Fix: pass MAX_UTC as the cutoff to effectively disable hybrid_search's
        // built-in temporal filter. The period post-filter handles the real semantics.
        let search_cutoff = if effective_cutoff.is_none() && query.has_period() {
            Some(DateTime::<Utc>::MAX_UTC)
        } else {
            effective_cutoff
        };

        // hybrid_search already does its own 3x overfetch internally (line 87),
        // so we pass the raw limit here — no double inflation.
        let search_query = SearchQuery {
            text: query.text.clone(),
            embedding: query.embedding.clone(),
            mode,
            limit,
            rerank_depth: None,
            valid_at: search_cutoff,
            fact_type: query.fact_type.clone(),
            scope: query.scope.clone(),
        };

        #[cfg(feature = "ann")]
        let strategy: &dyn VectorSearchStrategy = if self.should_use_hnsw() {
            self.hnsw_strategy.as_ref().unwrap()
        } else {
            &*self.vector_strategy
        };
        #[cfg(not(feature = "ann"))]
        let strategy: &dyn VectorSearchStrategy = &*self.vector_strategy;

        let (mut results, mut diagnostics) = self.with_read(|conn| {
            hybrid_search(conn, &search_query, self.embed_dim, scope_ids, strategy)
        })?;

        // Post-filter by period overlap
        if let (Some(start), Some(end)) = (query.period_start, query.period_end) {
            results.retain(|r| fact_overlaps_period(&r.fact, start, end));
        }

        // Post-filter by importance score
        if let Some(min_score) = query.min_importance_score {
            results.retain(|r| r.fact.importance_score >= min_score);
        }

        // Post-filter by pinned
        if query.pinned_only {
            results.retain(|r| r.fact.is_pinned);
        }

        results.truncate(limit);
        diagnostics.results_returned = results.len();

        // Expired-facts probe (opt-in, FTS-only).
        if query.include_expired_probe {
            if let Some(text) = &query.text {
                let fact_type_ref = query.fact_type.as_ref();
                let expired_count = self.with_read(|conn| {
                    crate::search::fts::fts_count_expired(conn, text, fact_type_ref, scope_ids)
                })?;
                diagnostics.expired_matches = Some(expired_count);
            }
            // Vector-only queries: expired_matches stays None (documented limitation).
        }

        // Archive fallback (opt-in, best-effort).
        #[cfg(feature = "archive")]
        if query.include_archives {
            match self.search_archives_fallback(query, limit) {
                Ok(Some(archive_results)) => {
                    diagnostics.archive_paks_scanned = archive_results.paks_scanned;
                    diagnostics.archive_search_ms = archive_results.search_ms;
                    results.extend(archive_results.results);
                    results.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    results.truncate(limit);
                }
                Ok(None) => {}
                Err(_e) => {
                    // Archive search is best-effort; silently skip on error.
                    tracing::warn!("archive search fallback failed: {_e}");
                }
            }
        }

        Ok(QueryResponse {
            results,
            diagnostics,
        })
    }

    /// Store path: no text/vector search, use FactStore directly.
    ///
    /// Strategy: fetch a broad candidate set from the most selective SQL query,
    /// then apply ALL remaining filters as post-filters to guarantee AND semantics.
    fn execute_store_path(
        &self,
        query: &MemoryQuery,
        scope_ids: Option<&[i64]>,
        effective_cutoff: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<QueryResponse> {
        let scope_slice = scope_ids.unwrap_or(&[]);

        // Fetch a broad candidate set. Use the most selective SQL query available,
        // but do NOT rely on it for AND semantics — post-filters handle that.
        // Overfetch to compensate for post-filter attrition.
        let fetch_limit = limit.saturating_mul(3).max(limit);

        let mut facts: Vec<Fact> =
            if let (Some(start), Some(end)) = (query.period_start, query.period_end) {
                // Period is the most selective: overlap + scope + optional fact_type in SQL.
                self.with_read(|conn| {
                    FactStore::new(conn, self.embed_dim).list_active_in_period(
                        start,
                        end,
                        scope_slice,
                        query.fact_type.as_ref(),
                    )
                })?
            } else {
                // Default: fetch by importance_score DESC (most useful ordering).
                // Use min_importance_score if set, otherwise 0.0 (all active facts).
                let min_score = query.min_importance_score.unwrap_or(0.0);
                self.with_read(|conn| {
                    FactStore::new(conn, self.embed_dim).list_by_importance_score(
                        scope_slice,
                        min_score,
                        fetch_limit,
                        &std::collections::HashSet::new(),
                    )
                })?
            };

        // --- Post-filters: apply ALL filters for AND semantics ---

        // Capture candidate count before post-filters for diagnostics.
        let candidates_before_filter = facts.len();

        // Temporal safety: exclude future-dated facts
        if let Some(cutoff) = effective_cutoff {
            facts.retain(|f| passes_temporal_cutoff(f, cutoff));
        }

        // Fact type (period branch already handles this in SQL; skip to avoid double-filter)
        if !query.has_period() {
            if let Some(ref ft) = query.fact_type {
                facts.retain(|f| &f.fact_type == ft);
            }
        }

        // Importance score (the default branch already handles this in SQL via min_score;
        // but the period branch does NOT, so we must apply it here for AND semantics)
        if query.has_period() {
            if let Some(min_score) = query.min_importance_score {
                facts.retain(|f| f.importance_score >= min_score);
            }
        }

        // Pinned filter — always applied as post-filter regardless of primary query
        if query.pinned_only {
            facts.retain(|f| f.is_pinned);
        }

        // Sort by importance_score DESC and truncate
        facts.sort_by(|a, b| {
            b.importance_score
                .partial_cmp(&a.importance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        facts.truncate(limit);

        let results: Vec<SearchResult> = facts.into_iter().map(fact_to_search_result).collect();
        let diagnostics = QueryDiagnostics {
            candidates_before_filter,
            results_returned: results.len(),
            // Store path has no FTS/vector search — no expired probe possible.
            ..QueryDiagnostics::default()
        };
        Ok(QueryResponse {
            results,
            diagnostics,
        })
    }
}
