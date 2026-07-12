//! Query execution: `MemoryEngine::{query, execute_query}`'s bodies, extracted as free
//! functions over [`MemoryCtx`] (Wave 2 #816 / S4, sub-PR 2).
//!
//! Moved from the facade's `engine/query.rs` verbatim except for one split: the
//! `#[cfg(feature = "archive")]` archive-fallback augmentation in the original
//! `execute_search_path` reached `MemoryEngine`-only state (`self.db_path`/`self.cold`,
//! via `search_archives_fallback`) that `MemoryCtx` does not expose and that `me-query`
//! has no dependency to reach (archive stays a facade concern). `execute_search_path`
//! here ends where the original's archive block began; the facade's `execute_query`
//! delegate applies that block as a post-step over the [`QueryResponse`] carried by
//! [`QueryExecution::Executed`].
//!
//! The original enforced **two** guards on that block by its *position* inside
//! `execute_search_path`: it ran only when (a) the query carried a search term
//! (`has_search()`), and (b) scope resolution had **not** early-returned on a
//! provided-but-missing scope (#117 — a missing scope yields no results, never an
//! unscoped search). The carve re-expresses (a) as an explicit
//! `query.has_search() && query.include_archives` check in the facade, and (b) via
//! [`QueryExecution`]: a scope-miss returns [`QueryExecution::ScopeMissing`], on which
//! the facade returns empty and skips the fallback. Without (b) the archive search —
//! which is itself unscoped (it never filters on `query.scope`) — would leak
//! cross-scope archived facts on a scope-miss, silently breaking the #117 contract.
//!
//! The four bi-temporal/reranker helper functions (`validate_reranker_output`,
//! `passes_temporal_cutoff`, `fact_overlaps_period`, `fact_to_search_result`) lived in
//! the facade's `engine/mod.rs` rather than `engine/query.rs`, but are consumed
//! exclusively by the query bodies above (verified: no other call site in the
//! workspace) — they move here too, along with their `proptest_temporal` coverage.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use me_index::ScopeTree;
use me_storage::MemoryCtx;
use me_traits::Reranker;
use me_types::error::{ConflictError, MemoryError, RerankerError, Result};
use me_types::types::Fact;
use me_types::types::ScopeQuery;
use me_types::types::search::{
    MatchType, MemoryQuery, QueryDiagnostics, QueryResponse, SearchMode, SearchQuery, SearchResult,
};

use crate::hybrid::port_hybrid_search;

/// Map a `tokio::task::spawn_blocking` join failure (a panic or cancellation in the
/// offloaded reranker call) to a `MemoryError`. Private copy of the facade's
/// `engine::spawn_join_err` (`pub(super)`, used by other engine modules too — not
/// moved), mirroring `me-ingest`'s own copy.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as map_err(spawn_join_err) fn pointer"
)]
fn spawn_join_err(e: tokio::task::JoinError) -> MemoryError {
    MemoryError::Internal(format!("offloaded task failed: {e}"))
}

/// Resolve a query's optional scope into concrete scope IDs.
///
/// Returns:
/// - `Some(scope_ids)` — scope resolved (or absent, giving `scope_ids == None`
///   for an unscoped search).
/// - `None` — a scope query was provided but the path does not exist. Callers
///   MUST surface this as empty results rather than falling through to an
///   unscoped search.
#[allow(
    clippy::option_option,
    reason = "the two `None`s are distinct: outer None = a scope was requested but \
              does not exist (caller returns no results); inner None = no scope was \
              requested (unscoped search). Collapsing them would lose that distinction."
)]
fn resolve_query_scope_ids(
    scope_tree: &RwLock<ScopeTree>,
    scope: Option<&ScopeQuery>,
) -> Option<Option<Vec<i64>>> {
    // Resolve scope IDs from cache (short-lived read lock). `None` scope → unscoped
    // (`Some(None)`); a requested-but-missing scope → outer `None` (no results); a hit
    // carries its resolved ids (`Some(Some(ids))`).
    scope.map_or(Some(None), |sq| {
        scope_tree.read().resolve_query(sq).map(Some)
    })
}

/// Validate reranker output indices and scores.
///
/// Checks four invariants:
/// 1. Output length does not exceed input length
/// 2. Every index is within `0..num_candidates`
/// 3. No duplicate indices
/// 4. All scores are finite (not NaN or Inf)
fn validate_reranker_output(num_candidates: usize, output: &[(usize, f64)]) -> Result<()> {
    use std::collections::HashSet;

    if output.len() > num_candidates {
        return Err(RerankerError::OutputTooLong {
            output_len: output.len(),
            input_len: num_candidates,
        }
        .into());
    }

    let mut seen = HashSet::with_capacity(output.len());

    for &(idx, score) in output {
        if idx >= num_candidates {
            return Err(RerankerError::OutOfBoundsIndex {
                index: idx,
                num_candidates,
            }
            .into());
        }
        if !seen.insert(idx) {
            return Err(RerankerError::DuplicateIndex { index: idx }.into());
        }
        if !score.is_finite() {
            return Err(RerankerError::NonFiniteScore { score, index: idx }.into());
        }
    }

    Ok(())
}

/// Check if a fact's validity window passes the temporal cutoff.
/// A fact passes if it's valid at the cutoff instant:
/// `(t_valid IS NULL OR t_valid <= cutoff) AND (t_invalid IS NULL OR t_invalid > cutoff)`
fn passes_temporal_cutoff(fact: &Fact, cutoff: DateTime<Utc>) -> bool {
    if let Some(t_valid) = fact.t_valid
        && t_valid > cutoff
    {
        return false;
    }
    if let Some(t_invalid) = fact.t_invalid
        && t_invalid <= cutoff
    {
        return false;
    }
    true
}

/// Check if a fact's `[t_valid, t_invalid)` interval overlaps `[start, end)`.
fn fact_overlaps_period(fact: &Fact, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    // (t_valid IS NULL OR t_valid < end) AND (t_invalid IS NULL OR t_invalid > start)
    fact.t_valid.is_none_or(|tv| tv < end) && fact.t_invalid.is_none_or(|ti| ti > start)
}

/// Wrap a `Fact` into a `SearchResult` with `MatchType::ImportanceRank`.
const fn fact_to_search_result(fact: Fact) -> SearchResult {
    SearchResult {
        score: fact.importance_score,
        match_type: MatchType::ImportanceRank,
        fact,
    }
}

/// Query facts using hybrid search (FTS5 + vector + RRF).
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure.
/// Returns `MemoryError::Reranker` if a configured [`Reranker`] fails or returns an
/// invalid permutation.
///
/// # Panics
///
/// Panics if internal candidate de-duplication yields an inconsistent
/// index — an invariant that should never be violated in practice.
pub async fn query(
    ctx: MemoryCtx<'_>,
    scope_tree: &RwLock<ScopeTree>,
    reranker: Option<&Arc<dyn Reranker>>,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>> {
    ctx.ensure_open()?;
    // Resolve scope. A provided-but-missing scope yields no results rather than
    // an unscoped search (#117).
    let Some(scope_ids) = resolve_query_scope_ids(scope_tree, query.scope.as_ref()) else {
        return Ok(vec![]); // scope doesn't exist → no results
    };

    // Hybrid search runs entirely below the seam (lexical + vector channels +
    // RRF fusion); the backend owns the HNSW-vs-brute dispatch (Stage C).
    let (mut results, _diagnostics) =
        port_hybrid_search(&**ctx.storage, query, scope_ids.as_deref()).await?;

    // Apply reranker if present and query has text (cross-encoder needs query text).
    // Offloaded to the blocking pool — reranking may be slow inference / a blocking
    // HTTP round-trip, and must not park the async executor (also makes a
    // `reqwest::blocking` reranker nested-runtime-safe).
    if let (Some(reranker), Some(text)) = (reranker, query.text.as_ref()) {
        let reranker = Arc::clone(reranker);
        let text = text.clone();
        let (ranked, returned) = tokio::task::spawn_blocking(move || {
            let ranked = reranker.rerank(&text, &results);
            (ranked, results)
        })
        .await
        .map_err(spawn_join_err)?;
        let ranked = ranked?;
        results = returned;
        validate_reranker_output(results.len(), &ranked)?;
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

/// Outcome of [`execute_query`], distinguishing a #117 scope-miss from an executed query.
///
/// A *provided-but-missing* scope must yield no results — and, critically, callers must
/// **not** fall back to any *unscoped* augmentation (e.g. the facade's archive search),
/// which would leak cross-scope facts. In the pre-carve engine this was enforced
/// *structurally*: the scope-miss early-return preceded `execute_search_path`, inside which
/// the archive block lived, so a scope-miss made the archive path unreachable. Splitting the
/// query body (`me-query`) from the archive augmentation (facade) dissolved that structural
/// guard, so the outcome is now carried explicitly in a type the caller cannot ignore
/// (Wave 2 #816 / S4 — regression caught in sub-PR 2 review).
///
/// The [`ScopeMissing`](QueryExecution::ScopeMissing) variant deliberately carries **no**
/// [`QueryResponse`]: a caller physically cannot reach a response to augment on the
/// scope-miss path, which is what makes the #117 contract un-bypassable at the type level.
#[derive(Debug)]
pub enum QueryExecution {
    /// A scope was provided but does not exist (#117). No results; the caller returns an
    /// empty response and MUST skip any unscoped augmentation such as the archive fallback.
    ScopeMissing,
    /// The query executed (unscoped, or a scope that resolved). Carries the response, which
    /// the caller MAY augment (e.g. with archived facts).
    Executed(QueryResponse),
}

/// Execute a composed query using the [`MemoryQuery`] builder.
///
/// Routes to hybrid search (FTS + vector) when text/embedding is present,
/// or to store-level queries (importance, pinned, period) otherwise.
/// All code paths enforce the temporal safety invariant: future-dated facts
/// (`t_valid > now`) are excluded unless overridden by `valid_at` or `period`.
///
/// Returns a [`QueryExecution`]: [`Executed`](QueryExecution::Executed) with the response
/// on a normal run, or [`ScopeMissing`](QueryExecution::ScopeMissing) when a scope was
/// provided but does not exist (#117). Does not apply the archive-fallback augmentation
/// (`MemoryQuery::include_archives`) — that stays a facade-only post-step over the
/// [`Executed`](QueryExecution::Executed) response; see the module docs.
///
/// # Errors
///
/// Returns `MemoryError::Conflict` if:
/// - `period_start` is set without `period_end` (or vice versa)
/// - `valid_at` and `period` are both set (mutually exclusive)
/// - `search_mode` conflicts with available text/embedding inputs
///
/// Returns `MemoryError::Storage` on query failure.
pub async fn execute_query(
    ctx: MemoryCtx<'_>,
    scope_tree: &RwLock<ScopeTree>,
    query: &MemoryQuery,
) -> Result<QueryExecution> {
    ctx.ensure_open()?;
    // --- Validation ---
    validate_memory_query(query)?;

    // --- Resolve scope ---
    // A provided-but-missing scope yields no results rather than an unscoped search
    // (#117), AND — because the caller's archive fallback is itself unscoped — that
    // fallback must be suppressed too. Signalled via `QueryExecution::ScopeMissing`.
    let Some(scope_ids) = resolve_query_scope_ids(scope_tree, query.scope.as_ref()) else {
        return Ok(QueryExecution::ScopeMissing);
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

    let response = if query.has_search() {
        execute_search_path(ctx, query, scope_ids.as_deref(), effective_cutoff, limit).await?
    } else {
        execute_store_path(ctx, query, scope_ids.as_deref(), effective_cutoff, limit).await?
    };
    Ok(QueryExecution::Executed(response))
}

/// Validate a `MemoryQuery` for conflicting or incomplete options.
fn validate_memory_query(query: &MemoryQuery) -> Result<()> {
    // Period: both start and end must be set, or neither
    if query.period_start.is_some() != query.period_end.is_some() {
        return Err(MemoryError::Conflict(ConflictError::QueryValidation(
            "period_start and period_end must both be set or both unset".into(),
        )));
    }

    // valid_at and period are mutually exclusive
    if query.valid_at.is_some() && query.has_period() {
        return Err(MemoryError::Conflict(ConflictError::QueryValidation(
            "valid_at and period are mutually exclusive".into(),
        )));
    }

    // Search mode compatibility (D7)
    if let Some(ref mode) = query.search_mode {
        match mode {
            SearchMode::Fts if query.text.is_none() => {
                return Err(MemoryError::Conflict(ConflictError::QueryValidation(
                    "SearchMode::Fts requires text to be set".into(),
                )));
            }
            SearchMode::Vector if query.embedding.is_none() => {
                return Err(MemoryError::Conflict(ConflictError::QueryValidation(
                    "SearchMode::Vector requires embedding to be set".into(),
                )));
            }
            SearchMode::Hybrid if query.text.is_none() || query.embedding.is_none() => {
                return Err(MemoryError::Conflict(ConflictError::QueryValidation(
                    "SearchMode::Hybrid requires both text and embedding".into(),
                )));
            }
            _ => {}
        }
    }

    Ok(())
}

/// Infer search mode from available inputs (D7).
fn infer_search_mode(query: &MemoryQuery) -> SearchMode {
    if let Some(ref mode) = query.search_mode {
        return *mode;
    }
    match (query.text.is_some(), query.embedding.is_some()) {
        (true, true) => SearchMode::Hybrid,
        (true, false) => SearchMode::Fts,
        (false, true) => SearchMode::Vector,
        // Invariant: callers gate on `has_search()`, so this arm is
        // unreachable in correct usage. Assert it in debug/test builds to
        // catch invariant violations, but fall back to the natural
        // empty-query default in release rather than panicking (#119).
        (false, false) => {
            debug_assert!(false, "infer_search_mode called without has_search()");
            SearchMode::Fts
        }
    }
}

/// Search path: delegate to [`port_hybrid_search`], then post-filter.
///
/// When post-filters are active (period, importance, pinned), we pass the
/// raw `limit` (not inflated) because `port_hybrid_search` already does its own
/// internal 3x overfetch before its temporal filter.
///
/// Ends where the facade's original archive-fallback block began — see the module
/// docs.
async fn execute_search_path(
    ctx: MemoryCtx<'_>,
    query: &MemoryQuery,
    scope_ids: Option<&[i64]>,
    effective_cutoff: Option<DateTime<Utc>>,
    limit: usize,
) -> Result<QueryResponse> {
    let mode = infer_search_mode(query);

    // When period is set, effective_cutoff is None — but port_hybrid_search defaults
    // valid_at=None to Utc::now(), which would hide facts outside the current
    // instant. We need port_hybrid_search to return ALL temporally-viable candidates
    // so our period post-filter can apply exact interval overlap semantics.
    // Fix: pass MAX_UTC as the cutoff to effectively disable port_hybrid_search's
    // built-in temporal filter. The period post-filter handles the real semantics.
    let search_cutoff = if effective_cutoff.is_none() && query.has_period() {
        Some(DateTime::<Utc>::MAX_UTC)
    } else {
        effective_cutoff
    };

    // port_hybrid_search already does its own 3x overfetch internally, so we
    // pass the raw limit here — no double inflation.
    let mut search_query = SearchQuery::new(mode, limit);
    search_query.text = query.text.clone();
    search_query.embedding = query.embedding.clone();
    search_query.valid_at = search_cutoff;
    search_query.fact_type = query.fact_type;
    search_query.scope = query.scope.clone();

    let (mut results, mut diagnostics) =
        port_hybrid_search(&**ctx.storage, &search_query, scope_ids).await?;

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

    // Expired-facts probe (opt-in, FTS-only). Vector-only queries leave
    // `expired_matches` as None (documented limitation).
    if query.include_expired_probe
        && let Some(text) = &query.text
    {
        let expired_count = ctx
            .storage
            .lexical_count_expired(text, query.fact_type.as_ref(), scope_ids)
            .await?;
        diagnostics.expired_matches = Some(expired_count);
    }

    Ok(QueryResponse {
        results,
        diagnostics,
    })
}

/// Store path: no text/vector search, use the storage port directly.
///
/// Strategy: fetch a broad candidate set from the most selective SQL query,
/// then apply ALL remaining filters as post-filters to guarantee AND semantics.
async fn execute_store_path(
    ctx: MemoryCtx<'_>,
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
            ctx.storage
                .list_active_facts_in_period(start, end, scope_slice, query.fact_type.as_ref())
                .await?
        } else {
            // Default: fetch by importance_score DESC (most useful ordering).
            // Use min_importance_score if set, otherwise 0.0 (all active facts).
            let min_score = query.min_importance_score.unwrap_or(0.0);
            ctx.storage
                .list_facts_by_importance_score(
                    scope_slice,
                    min_score,
                    fetch_limit,
                    &std::collections::HashSet::new(),
                )
                .await?
        };

    // --- Post-filters: apply ALL filters for AND semantics ---

    // Capture candidate count before post-filters for diagnostics.
    let candidates_before_filter = facts.len();

    // Temporal safety: exclude future-dated facts
    if let Some(cutoff) = effective_cutoff {
        facts.retain(|f| passes_temporal_cutoff(f, cutoff));
    }

    // Fact type (period branch already handles this in SQL; skip to avoid double-filter)
    if !query.has_period()
        && let Some(ref ft) = query.fact_type
    {
        facts.retain(|f| &f.fact_type == ft);
    }

    // Importance score (the default branch already handles this in SQL via min_score;
    // but the period branch does NOT, so we must apply it here for AND semantics)
    if query.has_period()
        && let Some(min_score) = query.min_importance_score
    {
        facts.retain(|f| f.importance_score >= min_score);
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

/// Property-based coverage for the pure bi-temporal filter helpers (#450).
///
/// `passes_temporal_cutoff` and `fact_overlaps_period` encode interval
/// containment / overlap algebra — exactly the place `<` vs `<=` off-by-one
/// errors hide and example tests routinely miss. These proptests pin the
/// helpers to their algebraic spec and to monotonicity laws.
#[cfg(test)]
mod proptest_temporal {
    use chrono::{DateTime, Utc};
    use proptest::prelude::*;

    use super::{fact_overlaps_period, passes_temporal_cutoff};
    use me_types::types::{Fact, FactType};

    // kept: returns `Fact` (not `NewFact`), takes t_valid/t_invalid — bi-temporal
    // test fixture; semantics differ entirely from the me_types::test_util factories.
    /// Build a `Fact` carrying only the valid-time fields the helpers read;
    /// every other field is an inert placeholder.
    fn make_fact(t_valid: Option<DateTime<Utc>>, t_invalid: Option<DateTime<Utc>>) -> Fact {
        Fact {
            id: 1,
            content: String::new(),
            content_hash: String::new(),
            embedding: vec![],
            fact_type: FactType::Semantic,
            t_created: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            t_expired: None,
            t_valid,
            t_invalid,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            metadata: serde_json::Value::Null,
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.0,
            surfaced_at: None,
        }
    }

    prop_compose! {
        /// Timestamps as whole seconds in a wide but always-valid range, so
        /// `from_timestamp` never returns `None`.
        fn arb_ts()(s in 0i64..=4_000_000_000) -> DateTime<Utc> {
            DateTime::<Utc>::from_timestamp(s, 0).unwrap()
        }
    }

    /// Reference spec for `passes_temporal_cutoff`, written independently of the
    /// implementation so the proptest is a genuine cross-check, not a tautology.
    fn spec_passes(
        t_valid: Option<DateTime<Utc>>,
        t_invalid: Option<DateTime<Utc>>,
        cutoff: DateTime<Utc>,
    ) -> bool {
        let valid_ok = t_valid.is_none_or(|tv| tv <= cutoff);
        let invalid_ok = t_invalid.is_none_or(|ti| ti > cutoff);
        valid_ok && invalid_ok
    }

    /// Reference spec for `fact_overlaps_period`.
    fn spec_overlaps(
        t_valid: Option<DateTime<Utc>>,
        t_invalid: Option<DateTime<Utc>>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> bool {
        t_valid.is_none_or(|tv| tv < end) && t_invalid.is_none_or(|ti| ti > start)
    }

    proptest! {
        /// The helper agrees with its independent spec for all inputs (catches
        /// any `<`/`<=`/`>`/`>=` off-by-one regression).
        #[test]
        fn cutoff_matches_spec(
            t_valid in proptest::option::of(arb_ts()),
            t_invalid in proptest::option::of(arb_ts()),
            cutoff in arb_ts(),
        ) {
            let fact = make_fact(t_valid, t_invalid);
            prop_assert_eq!(
                passes_temporal_cutoff(&fact, cutoff),
                spec_passes(t_valid, t_invalid, cutoff)
            );
        }

        /// With `t_invalid` unbounded, passing is monotone in `cutoff`: once a
        /// fact is valid at `cutoff` it stays valid at every later instant.
        #[test]
        fn cutoff_monotone_when_open_ended(
            t_valid in proptest::option::of(arb_ts()),
            cutoff in arb_ts(),
            delta in 0i64..=1_000_000,
        ) {
            let fact = make_fact(t_valid, None);
            let later = DateTime::<Utc>::from_timestamp(cutoff.timestamp() + delta, 0).unwrap();
            if passes_temporal_cutoff(&fact, cutoff) {
                prop_assert!(passes_temporal_cutoff(&fact, later));
            }
        }

        /// A fact whose validity starts strictly after `cutoff` never passes.
        #[test]
        fn cutoff_rejects_future_t_valid(
            cutoff in arb_ts(),
            delta in 1i64..=1_000_000,
            t_invalid in proptest::option::of(arb_ts()),
        ) {
            let t_valid = DateTime::<Utc>::from_timestamp(cutoff.timestamp() + delta, 0).unwrap();
            let fact = make_fact(Some(t_valid), t_invalid);
            prop_assert!(!passes_temporal_cutoff(&fact, cutoff));
        }

        /// The helper agrees with its independent spec for all inputs.
        #[test]
        fn overlap_matches_spec(
            t_valid in proptest::option::of(arb_ts()),
            t_invalid in proptest::option::of(arb_ts()),
            a in arb_ts(),
            b in arb_ts(),
        ) {
            // Normalize to a non-empty half-open window [start, end).
            let (start, end) = if a < b { (a, b) } else { (b, a) };
            prop_assume!(start < end);
            let fact = make_fact(t_valid, t_invalid);
            prop_assert_eq!(
                fact_overlaps_period(&fact, start, end),
                spec_overlaps(t_valid, t_invalid, start, end)
            );
        }

        /// Overlap is monotone under window widening: if a fact overlaps
        /// `[start, end)` it overlaps any superset window `[start', end')` with
        /// `start' <= start` and `end' >= end`.
        #[test]
        fn overlap_monotone_under_widening(
            t_valid in proptest::option::of(arb_ts()),
            t_invalid in proptest::option::of(arb_ts()),
            a in arb_ts(),
            b in arb_ts(),
            grow_left in 0i64..=1_000_000,
            grow_right in 0i64..=1_000_000,
        ) {
            let (start, end) = if a < b { (a, b) } else { (b, a) };
            prop_assume!(start < end);
            let fact = make_fact(t_valid, t_invalid);
            if fact_overlaps_period(&fact, start, end) {
                let wider_start =
                    DateTime::<Utc>::from_timestamp(start.timestamp() - grow_left, 0).unwrap();
                let wider_end =
                    DateTime::<Utc>::from_timestamp(end.timestamp() + grow_right, 0).unwrap();
                prop_assert!(fact_overlaps_period(&fact, wider_start, wider_end));
            }
        }

        /// A fact unbounded on both valid-time ends overlaps every non-empty
        /// window.
        #[test]
        fn unbounded_fact_overlaps_everything(a in arb_ts(), b in arb_ts()) {
            let (start, end) = if a < b { (a, b) } else { (b, a) };
            prop_assume!(start < end);
            let fact = make_fact(None, None);
            prop_assert!(fact_overlaps_period(&fact, start, end));
        }
    }
}
