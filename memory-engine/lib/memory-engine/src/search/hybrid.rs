use std::collections::{HashMap, HashSet};

#[cfg(test)]
use chrono::DateTime;
use chrono::Utc;
use rusqlite::Connection;

use crate::error::Result;
use crate::search::fts::{FtsResult, fts_search};
use crate::search::strategy::VectorSearchStrategy;
use crate::search::vector::VectorResult;
use crate::store::facts::FactStore;
use crate::types::Fact;
#[cfg(test)]
use crate::types::FactType;
use crate::types::search::{
    MatchType, QueryDiagnostics, RRF_K, SearchMode, SearchQuery, SearchResult,
};

/// Reciprocal Rank Fusion merge of two ranked result lists.
///
/// Each item's RRF score = sum of `1 / (k + rank + 1)` across all lists
/// where it appears (rank is 0-based). Returns merged results sorted
/// descending by RRF score.
#[must_use]
pub fn rrf_merge(fts: &[(i64, f64)], vec: &[(i64, f32)], k: u32) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for (rank, &(id, _)) in fts.iter().enumerate() {
        let rank_u32 = u32::try_from(rank).unwrap_or(u32::MAX);
        *scores.entry(id).or_default() +=
            1.0 / f64::from(k.saturating_add(rank_u32).saturating_add(1));
    }
    for (rank, &(id, _)) in vec.iter().enumerate() {
        let rank_u32 = u32::try_from(rank).unwrap_or(u32::MAX);
        *scores.entry(id).or_default() +=
            1.0 / f64::from(k.saturating_add(rank_u32).saturating_add(1));
    }
    let mut merged: Vec<_> = scores.into_iter().collect();
    merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    merged
}

/// Perform a hybrid search combining FTS5 and vector similarity.
///
/// Over-fetches 3x the requested limit from each source before merging
/// to improve result quality after RRF fusion.
///
/// # Errors
///
/// Returns `MemoryError::Database` on query failure.
/// Returns `MemoryError::EmbeddingDimension` if the query embedding or a stored
/// embedding does not match the configured dimension during vector search.
//
// Test-only oracle: the engine's live path is the async [`port_hybrid_search`]
// (#631 Stage C); this synchronous twin survives only as the in-process
// reference the `search` unit tests fuse against. It is `dead_code` in a release
// build, so suppress the lint there rather than ship/widen it.
#[cfg_attr(not(test), allow(dead_code))]
// `search` is a crate-private module, so `pub(crate)` is the honest visibility;
// the lint only fires because the module isn't `pub`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn hybrid_search(
    conn: &Connection,
    query: &SearchQuery,
    embed_dim: usize,
    scope_ids: Option<&[i64]>,
    vector_strategy: &dyn VectorSearchStrategy,
) -> Result<(Vec<SearchResult>, QueryDiagnostics)> {
    // Effective candidate target: rerank_depth widens the pool, but never below limit.
    let effective_target = query.rerank_depth.unwrap_or(query.limit).max(query.limit);
    // Over-fetch 3x to compensate for post-filter attrition.
    // The temporal post-filter always runs (cutoff defaults to Utc::now()
    // when valid_at is None), so 3x is always needed. In Hybrid mode,
    // overfetch also ensures RRF has enough candidates from each source
    // for meaningful rank fusion.
    let overfetch = effective_target.saturating_mul(3).max(effective_target);

    let (fts_results, vec_results) = collect_candidates(
        conn,
        query,
        embed_dim,
        scope_ids,
        vector_strategy,
        overfetch,
    )?;

    let fts_candidate_count = fts_results.len();
    let vec_candidate_count = vec_results.len();

    // Build ID sets for match_type determination
    let fts_ids: HashSet<i64> = fts_results.iter().map(|r| r.fact_id).collect();
    let vec_ids: HashSet<i64> = vec_results.iter().map(|r| r.fact_id).collect();

    let ranked = rank_candidates(query.mode, &fts_results, &vec_results);

    // Load full facts (t_expired/fact_type/scope are SQL-level filters; only
    // valid_at remains a post-filter). Batch-materialize all ranked ids in one
    // round-trip; `assemble_results` re-orders to ranked order, applies the
    // temporal post-filter + match_type, and truncates to the effective limit.
    let ranked_ids: Vec<i64> = ranked.iter().map(|&(id, _)| id).collect();
    let facts_by_id = FactStore::new(conn, embed_dim).get_many(&ranked_ids)?;
    Ok(assemble_results(
        query,
        ranked,
        &fts_ids,
        &vec_ids,
        fts_candidate_count,
        vec_candidate_count,
        facts_by_id,
    ))
}

/// Candidate-collection stage of [`hybrid_search`]: run the FTS5 and vector
/// source queries for the active `SearchMode`, pushing the `t_expired` /
/// `fact_type` / `scope` filters into SQL via `overfetch`. A source query is
/// skipped (returning an empty vec) when the mode excludes it or its query
/// input (`text` / `embedding`) is absent — exactly the inline behavior this
/// extraction replaces.
///
/// # Errors
///
/// Propagates `MemoryError::Database` on query failure and
/// `MemoryError::EmbeddingDimension` on a wrong-length embedding (vector path).
fn collect_candidates(
    conn: &Connection,
    query: &SearchQuery,
    embed_dim: usize,
    scope_ids: Option<&[i64]>,
    vector_strategy: &dyn VectorSearchStrategy,
    overfetch: usize,
) -> Result<(Vec<FtsResult>, Vec<VectorResult>)> {
    let fact_type_ref = query.fact_type.as_ref();

    // Collect FTS results (t_expired, fact_type, scope pushed into SQL)
    let fts_results = if matches!(query.mode, SearchMode::Fts | SearchMode::Hybrid) {
        match query.text.as_ref() {
            Some(text) => fts_search(conn, text, overfetch, fact_type_ref, scope_ids)?,
            None => vec![],
        }
    } else {
        vec![]
    };

    // Collect vector results (t_expired, fact_type, scope pushed into SQL)
    let vec_results = if matches!(query.mode, SearchMode::Vector | SearchMode::Hybrid) {
        match query.embedding.as_ref() {
            Some(emb) => {
                vector_strategy.search(conn, emb, embed_dim, overfetch, fact_type_ref, scope_ids)?
            }
            None => vec![],
        }
    } else {
        vec![]
    };

    Ok((fts_results, vec_results))
}

/// Ranking stage of [`hybrid_search`]: dispatch on `SearchMode` to produce the
/// ranked `(fact_id, score)` list. `Fts`/`Vector` modes map their single source
/// straight through (vector scores widened `f32`→`f64`); `Hybrid` fuses both
/// rank lists with [`rrf_merge`]. Pure — no I/O — so it mirrors the per-source
/// projections the inline code performed.
fn rank_candidates(
    mode: SearchMode,
    fts_results: &[FtsResult],
    vec_results: &[VectorResult],
) -> Vec<(i64, f64)> {
    match mode {
        SearchMode::Fts => fts_results.iter().map(|r| (r.fact_id, r.score)).collect(),
        SearchMode::Vector => vec_results
            .iter()
            .map(|r| (r.fact_id, f64::from(r.score)))
            .collect(),
        SearchMode::Hybrid => {
            let fts_pairs: Vec<(i64, f64)> =
                fts_results.iter().map(|r| (r.fact_id, r.score)).collect();
            let vec_pairs: Vec<(i64, f32)> =
                vec_results.iter().map(|r| (r.fact_id, r.score)).collect();
            rrf_merge(&fts_pairs, &vec_pairs, RRF_K)
        }
    }
}

/// Pure fusion stage shared by the sync (`hybrid_search`) and async
/// (`port_hybrid_search`) I/O paths: re-order `ranked` to its rank order,
/// apply the `valid_at`/`t_invalid` temporal post-filter, assign `match_type`,
/// truncate to the effective limit, and build the diagnostics. The only inputs
/// are the ranked `(id, score)` list, the per-source id sets, the candidate
/// counts, and the materialized facts — no I/O — so both backends produce
/// bit-identical results from identical channel outputs.
fn assemble_results(
    query: &SearchQuery,
    ranked: Vec<(i64, f64)>,
    fts_ids: &HashSet<i64>,
    vec_ids: &HashSet<i64>,
    fts_candidate_count: usize,
    vec_candidate_count: usize,
    mut facts_by_id: HashMap<i64, Fact>,
) -> (Vec<SearchResult>, QueryDiagnostics) {
    // Clamped to at least `limit` so rerank_depth can only widen, never shrink.
    let effective_limit = query.rerank_depth.unwrap_or(query.limit).max(query.limit);
    let ranked_count = ranked.len();
    // Temporal cutoff is loop-invariant — compute once. Explicit `valid_at` if
    // provided, otherwise now, so future-dated facts (t_valid > now) stay
    // invisible to regular queries (they surface only via list_due()/resume).
    let cutoff = query.valid_at.unwrap_or_else(Utc::now);
    let mut results = Vec::new();
    for (id, score) in ranked {
        if results.len() >= effective_limit {
            break;
        }
        let Some(fact) = facts_by_id.remove(&id) else {
            tracing::warn!("failed to load fact id={id} during result collection, skipping");
            continue;
        };

        // Apply temporal filter (post-filter — complex temporal semantics).
        if let Some(t_valid) = fact.t_valid
            && t_valid > cutoff
        {
            continue;
        }
        if let Some(t_invalid) = fact.t_invalid
            && t_invalid <= cutoff
        {
            continue;
        }

        let match_type = if fts_ids.contains(&id) && vec_ids.contains(&id) {
            MatchType::Both
        } else if fts_ids.contains(&id) {
            MatchType::Fts
        } else {
            MatchType::Vector
        };

        results.push(SearchResult {
            fact,
            score,
            match_type,
        });
    }

    let diagnostics = QueryDiagnostics {
        candidates_before_filter: ranked_count,
        results_returned: results.len(),
        fts_candidates: fts_candidate_count,
        vector_candidates: vec_candidate_count,
        ..QueryDiagnostics::default()
    };

    (results, diagnostics)
}

/// Port-driven hybrid search (#631 Stage C) — the async twin of [`hybrid_search`].
///
/// Drives the lexical/vector channels + fact-fetch through
/// `Arc<dyn StorageBackend>` instead of a raw `&Connection`, then feeds the
/// identical pure [`assemble_results`] fusion. The engine adopts this in the
/// Stage E cutover; until then it is proven bit-identical to the sync path by a
/// parity oracle (see tests). RRF still fuses by rank only.
///
/// # Errors
/// Propagates backend errors (`MemoryError::Storage`/`Database`,
/// `EmbeddingDimension` on a wrong-length query embedding).
pub async fn port_hybrid_search(
    storage: &dyn crate::storage::StorageBackend,
    query: &SearchQuery,
    scope_ids: Option<&[i64]>,
) -> Result<(Vec<SearchResult>, QueryDiagnostics)> {
    use crate::storage::{FactFilter, TemporalFilter};

    let effective_target = query.rerank_depth.unwrap_or(query.limit).max(query.limit);
    let overfetch = effective_target.saturating_mul(3).max(effective_target);

    // The lexical/vector channels honor exactly `fact_type` + `scope_ids` +
    // Active (the same SQL-level predicates the sync free functions push down).
    let filter = FactFilter {
        fact_type: query.fact_type,
        scope_ids: scope_ids.map(<[i64]>::to_vec),
        temporal: TemporalFilter::Active,
        ..FactFilter::default()
    };

    let fts_results: Vec<(i64, f64)> = if matches!(query.mode, SearchMode::Fts | SearchMode::Hybrid)
    {
        match query.text.as_ref() {
            Some(text) => storage.lexical_search(text, &filter, overfetch).await?,
            None => vec![],
        }
    } else {
        vec![]
    };

    let vec_results: Vec<(i64, f64)> =
        if matches!(query.mode, SearchMode::Vector | SearchMode::Hybrid) {
            match query.embedding.as_ref() {
                Some(emb) => storage.vector_search(emb, &filter, overfetch).await?,
                None => vec![],
            }
        } else {
            vec![]
        };

    let fts_candidate_count = fts_results.len();
    let vec_candidate_count = vec_results.len();
    let fts_ids: HashSet<i64> = fts_results.iter().map(|&(id, _)| id).collect();
    let vec_ids: HashSet<i64> = vec_results.iter().map(|&(id, _)| id).collect();

    let ranked: Vec<(i64, f64)> = match query.mode {
        SearchMode::Fts => fts_results,
        SearchMode::Vector => vec_results,
        SearchMode::Hybrid => {
            // `rrf_merge` fuses by rank position and ignores the vector score
            // value, so the placeholder f32 is irrelevant — the vec rank order
            // (the only thing that matters) is preserved.
            let vec_pairs: Vec<(i64, f32)> = vec_results.iter().map(|&(id, _)| (id, 0.0)).collect();
            rrf_merge(&fts_results, &vec_pairs, RRF_K)
        }
    };

    let ranked_ids: Vec<i64> = ranked.iter().map(|&(id, _)| id).collect();
    let facts_by_id = storage.get_facts(&ranked_ids).await?;
    Ok(assemble_results(
        query,
        ranked,
        &fts_ids,
        &vec_ids,
        fts_candidate_count,
        vec_candidate_count,
        facts_by_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::strategy::BruteForce;
    use crate::store::schema::{init_schema, open_memory};
    use crate::types::NewFact;

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn search_query_new_defaults_all_optionals_to_none() {
        let q = SearchQuery::new(SearchMode::Hybrid, 7);
        assert_eq!(q.mode, SearchMode::Hybrid);
        assert_eq!(q.limit, 7);
        assert!(q.text.is_none());
        assert!(q.embedding.is_none());
        assert!(q.rerank_depth.is_none());
        assert!(q.valid_at.is_none());
        assert!(q.fact_type.is_none());
        assert!(q.scope.is_none());
    }

    #[test]
    fn search_query_builder_chains_set_every_field() {
        let at = chrono::Utc::now();
        let q = SearchQuery::new(SearchMode::Fts, 3)
            .text("rust")
            .embedding(vec![1.0, 0.0, 0.0, 0.0])
            .mode(SearchMode::Hybrid)
            .limit(9)
            .rerank_depth(20)
            .valid_at(at)
            .fact_type(FactType::Semantic)
            .scope(crate::types::ScopeQuery::Exact("user:alice".into()));

        assert_eq!(q.text.as_deref(), Some("rust"));
        assert_eq!(q.embedding, Some(vec![1.0, 0.0, 0.0, 0.0]));
        // `mode`/`limit` setters override the `new` arguments (last write wins).
        assert_eq!(q.mode, SearchMode::Hybrid);
        assert_eq!(q.limit, 9);
        assert_eq!(q.rerank_depth, Some(20));
        assert_eq!(q.valid_at, Some(at));
        assert_eq!(q.fact_type, Some(FactType::Semantic));
        assert_eq!(
            q.scope,
            Some(crate::types::ScopeQuery::Exact("user:alice".into()))
        );
    }

    fn make_fact_with(content: &str, embedding: Vec<f32>, fact_type: FactType) -> NewFact {
        crate::test_utils::new_fact_with_type(content, embedding, fact_type)
    }

    fn make_fact(content: &str, embedding: Vec<f32>) -> NewFact {
        crate::test_utils::new_fact(content, embedding)
    }

    /// Like [`make_fact`], but lets a fixture carry explicit valid-time bounds.
    ///
    /// `make_fact`/`make_fact_with` hardcode `t_valid: None, t_invalid: None`, so
    /// the temporal post-filter in [`assemble_results`] is never exercised by the
    /// other unit tests (the `Utc::now()` cutoff never excludes a `None`-bounded
    /// fact). This override is the seam the bi-temporal tests need to drive a fact
    /// that is future-dated (`t_valid > cutoff`) or already invalidated
    /// (`t_invalid <= cutoff`).
    // kept: distinct — sets t_valid/t_invalid for bi-temporal filter tests.
    fn make_fact_temporal(
        content: &str,
        embedding: Vec<f32>,
        t_valid: Option<DateTime<Utc>>,
        t_invalid: Option<DateTime<Utc>>,
    ) -> NewFact {
        NewFact {
            t_valid,
            t_invalid,
            ..make_fact(content, embedding)
        }
    }

    /// Stage C parity oracle: the async `port_hybrid_search` (driving the
    /// channels through `SqliteBackend`) must be **bit-identical** to the sync
    /// `hybrid_search` (raw `&Connection` + `BruteForce`) on the same data,
    /// across all three modes. Distinct bm25/cosine scores avoid RRF tie
    /// non-determinism; facts carry no `t_valid`/`t_invalid` so the `Utc::now()`
    /// cutoff never filters (sidestepping its per-call timing). This is what
    /// proves the port I/O channels (post-#684 `*_filtered` SQL) match the sync
    /// free functions before the Stage E cutover consumes the port path.
    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the write guard is intentionally held across the seed block"
    )]
    async fn port_hybrid_search_parity_with_sync() {
        use std::sync::Arc;

        use crate::pool::ConnectionPool;
        use crate::storage::SqliteBackend;
        use crate::store::facts::FactStore;
        use crate::store::upcaster::UpcasterRegistry;

        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            // Distinct content (distinct bm25) + distinct embeddings (distinct cosine).
            store
                .insert(&make_fact(
                    "rust systems programming language",
                    vec![1.0, 0.0, 0.0, 0.0],
                ))
                .unwrap();
            store
                .insert(&make_fact(
                    "rust ownership and borrowing model",
                    vec![0.0, 1.0, 0.0, 0.0],
                ))
                .unwrap();
            store
                .insert(&make_fact(
                    "rust async await with tokio",
                    vec![0.9, 0.1, 0.0, 0.0],
                ))
                .unwrap();
        }
        let backend =
            SqliteBackend::from_pool(Arc::clone(&pool), Arc::new(UpcasterRegistry::new()));

        for mode in [SearchMode::Fts, SearchMode::Vector, SearchMode::Hybrid] {
            let query = SearchQuery::new(mode, 10)
                .text("rust")
                .embedding(vec![1.0, 0.0, 0.0, 0.0]);

            let (sync_results, sync_diag) = {
                let conn = pool.read().unwrap();
                hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap()
            };
            let (port_results, port_diag) =
                port_hybrid_search(&backend, &query, None).await.unwrap();

            // Compare under a canonical (score desc, id asc) order. RRF ties are
            // ordered non-deterministically by `rrf_merge` (HashMap-seeded), so
            // the sync path is itself tie-order-non-deterministic; canonicalizing
            // asserts the most that is meaningful — the I/O channels + RRF scores
            // match. (rrf_merge tie-determinism tracked as a follow-up.)
            let canon = |rs: &[SearchResult]| -> Vec<(i64, MatchType, f64)> {
                let mut v: Vec<(i64, MatchType, f64)> = rs
                    .iter()
                    .map(|r| (r.fact.id, r.match_type, r.score))
                    .collect();
                v.sort_by(|a, b| {
                    b.2.partial_cmp(&a.2)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.0.cmp(&b.0))
                });
                v
            };
            let sync_c = canon(&sync_results);
            let port_c = canon(&port_results);
            assert_eq!(sync_c.len(), port_c.len(), "mode {mode:?}: result count");
            for (s, p) in sync_c.iter().zip(port_c.iter()) {
                assert_eq!(
                    (s.0, s.1),
                    (p.0, p.1),
                    "mode {mode:?}: id/match_type must match (canonical order)"
                );
                assert!(
                    (s.2 - p.2).abs() < 1e-9,
                    "mode {mode:?}: score differs for id {} ({} vs {})",
                    s.0,
                    s.2,
                    p.2
                );
            }
            assert_eq!(
                sync_diag, port_diag,
                "mode {mode:?}: diagnostics must match"
            );
            assert!(
                !sync_results.is_empty(),
                "mode {mode:?}: fixture should match"
            );
        }
    }

    // --- rrf_merge pure function tests ---

    #[test]
    fn rrf_merge_known_scores() {
        let fts = vec![(1_i64, -1.5_f64), (2, -1.0)];
        let vec_results = vec![(2_i64, 0.9_f32), (3, 0.8)];

        let merged = rrf_merge(&fts, &vec_results, 60);

        // ID 1: FTS rank 0 → 1/(60+0+1) = 1/61
        // ID 2: FTS rank 1 → 1/(60+1+1) = 1/62, Vec rank 0 → 1/(60+0+1) = 1/61
        // ID 3: Vec rank 1 → 1/(60+1+1) = 1/62
        let scores: HashMap<i64, f64> = merged.into_iter().collect();
        let expected_2 = 1.0 / 62.0 + 1.0 / 61.0;
        let expected_1 = 1.0 / 61.0;
        let expected_3 = 1.0 / 62.0;

        assert!((scores[&2] - expected_2).abs() < 1e-10);
        assert!((scores[&1] - expected_1).abs() < 1e-10);
        assert!((scores[&3] - expected_3).abs() < 1e-10);
    }

    #[test]
    fn rrf_merge_descending_order() {
        let fts = vec![(1_i64, -2.0_f64), (2, -1.0)];
        let vec_results = vec![(2_i64, 0.9_f32), (3, 0.8)];

        let merged = rrf_merge(&fts, &vec_results, 60);
        // ID 2 should be first (appears in both lists)
        assert_eq!(merged[0].0, 2);
    }

    #[test]
    fn rrf_merge_empty_fts() {
        let fts: Vec<(i64, f64)> = vec![];
        let vec_results = vec![(1_i64, 0.9_f32), (2, 0.8)];

        let merged = rrf_merge(&fts, &vec_results, 60);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].0, 1);
    }

    #[test]
    fn rrf_merge_empty_vec() {
        let fts = vec![(1_i64, -2.0_f64), (2, -1.0)];
        let vec_results: Vec<(i64, f32)> = vec![];

        let merged = rrf_merge(&fts, &vec_results, 60);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].0, 1);
    }

    // --- rrf_merge edge cases (#478) ---

    #[test]
    fn rrf_merge_both_empty() {
        // Nothing in either list → empty fusion (no panic, no spurious entry).
        let fts: Vec<(i64, f64)> = vec![];
        let vec_results: Vec<(i64, f32)> = vec![];
        assert_eq!(rrf_merge(&fts, &vec_results, 60), vec![]);
    }

    #[test]
    fn rrf_merge_k_zero() {
        // k=0 is a valid smoothing constant: the denominator is `0 + rank + 1`,
        // so the rank-0 item scores `1/(0+0+1) = 1.0`. The proptest range starts
        // at 1, so this boundary is otherwise unexercised.
        let fts = vec![(1_i64, -1.0_f64)];
        let vec_results: Vec<(i64, f32)> = vec![];
        let merged = rrf_merge(&fts, &vec_results, 0);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].1 - 1.0).abs() < 1e-10);
    }

    #[test]
    fn rrf_merge_duplicate_ids_within_one_list() {
        // Documents the accumulator behavior: a duplicate id inside ONE input
        // list is NOT deduplicated before scoring — `scores.entry(id)` is hit
        // once per occurrence, so the id's RRF score is the SUM of both ranks'
        // contributions (an unintended double-count, intentionally captured here
        // so a future dedup change is a visible, reviewed behavior break). The
        // HashMap still collapses the id to a single output row.
        let fts = vec![(1_i64, -1.0_f64), (1, -2.0)];
        let vec_results: Vec<(i64, f32)> = vec![];
        let merged = rrf_merge(&fts, &vec_results, 60);
        // Single output row (deduped by the HashMap)...
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].0, 1);
        // ...but its score is rank-0 + rank-1 contributions summed:
        // 1/(60+0+1) + 1/(60+1+1) = 1/61 + 1/62.
        let expected = 1.0 / 61.0 + 1.0 / 62.0;
        assert!((merged[0].1 - expected).abs() < 1e-10);
    }

    // --- hybrid_search integration tests ---

    #[test]
    fn hybrid_search_integration() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact(
                "Rust systems programming",
                vec![1.0, 0.0, 0.0, 0.0],
            ))
            .unwrap();
        store
            .insert(&make_fact(
                "Python machine learning",
                vec![0.0, 1.0, 0.0, 0.0],
            ))
            .unwrap();
        store
            .insert(&make_fact("Rust memory safety", vec![0.9, 0.1, 0.0, 0.0]))
            .unwrap();
        store
            .insert(&make_fact(
                "JavaScript web development",
                vec![0.0, 0.0, 1.0, 0.0],
            ))
            .unwrap();
        store
            .insert(&make_fact(
                "Rust zero cost abstractions",
                vec![0.8, 0.2, 0.0, 0.0],
            ))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Hybrid, 3)
            .text("Rust")
            .embedding(vec![1.0, 0.0, 0.0, 0.0]);

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        assert!(!results.is_empty());
        assert!(results.len() <= 3);
        // Results matching both FTS and vector should have MatchType::Both
        let has_both = results.iter().any(|r| r.match_type == MatchType::Both);
        assert!(has_both);
    }

    #[test]
    fn search_mode_fts_only() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact("Rust language", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        store
            .insert(&make_fact("Python language", vec![0.0, 1.0, 0.0, 0.0]))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Fts, 10).text("Rust");

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_type, MatchType::Fts);
    }

    #[test]
    fn search_mode_vector_only() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact("fact one", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        store
            .insert(&make_fact("fact two", vec![0.0, 1.0, 0.0, 0.0]))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Vector, 1).embedding(vec![1.0, 0.0, 0.0, 0.0]);

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_type, MatchType::Vector);
        assert_eq!(results[0].fact.content, "fact one");
    }

    #[test]
    fn hybrid_search_fact_type_filter() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        store
            .insert(&make_fact_with(
                "semantic fact",
                vec![1.0, 0.0, 0.0, 0.0],
                FactType::Semantic,
            ))
            .unwrap();
        store
            .insert(&make_fact_with(
                "episodic fact",
                vec![0.9, 0.1, 0.0, 0.0],
                FactType::Episodic,
            ))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Fts, 10)
            .text("fact")
            .fact_type(FactType::Semantic);

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.fact_type, FactType::Semantic);
    }

    // --- bi-temporal post-filter tests (#325) ---
    //
    // The `t_valid`/`t_invalid` post-filter in `assemble_results` (the only place
    // `hybrid_search` enforces valid-time semantics) is otherwise never exercised
    // by the unit tests: every other fixture is `t_valid: None, t_invalid: None`,
    // so the `Utc::now()` cutoff never excludes anything. `make_fact_temporal`
    // supplies the explicit bounds these three branches need.

    #[test]
    fn hybrid_search_excludes_future_t_valid() {
        // A fact whose valid-time starts in the future (`t_valid > cutoff`) is a
        // scheduled/not-yet-true fact: it must NOT surface in a regular query
        // (valid_at = None → cutoff = now).
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let future = Utc::now() + chrono::Duration::hours(1);
        store
            .insert(&make_fact("present rust fact", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        store
            .insert(&make_fact_temporal(
                "future rust fact",
                vec![0.9, 0.1, 0.0, 0.0],
                Some(future),
                None,
            ))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Fts, 10).text("rust");

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        // The future-dated fact is filtered out; only the present one survives.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "present rust fact");
    }

    #[test]
    fn hybrid_search_excludes_past_t_invalid() {
        // A fact invalidated in the past (`t_invalid <= cutoff`) is no longer
        // true at query time and must be excluded.
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let past = Utc::now() - chrono::Duration::hours(1);
        store
            .insert(&make_fact("current rust fact", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        store
            .insert(&make_fact_temporal(
                "stale rust fact",
                vec![0.9, 0.1, 0.0, 0.0],
                None,
                Some(past),
            ))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Fts, 10).text("rust");

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "current rust fact");
    }

    #[test]
    fn hybrid_search_valid_at_time_travel() {
        // Time-travel: an explicit `valid_at` in the future moves the cutoff so a
        // future-dated fact (`t_valid <= valid_at`) becomes visible, while the
        // same fact stays invisible to a now-cutoff query (proved by the
        // `excludes_future_t_valid` test above).
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let future_valid = Utc::now() + chrono::Duration::hours(1);
        let travel_to = future_valid + chrono::Duration::seconds(1);
        store
            .insert(&make_fact_temporal(
                "scheduled rust fact",
                vec![1.0, 0.0, 0.0, 0.0],
                Some(future_valid),
                None,
            ))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Fts, 10)
            .text("rust")
            .valid_at(travel_to);

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        // With the cutoff advanced past `t_valid`, the future fact is now visible.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "scheduled rust fact");
    }

    #[test]
    fn hybrid_search_present_survives_overfetch_attrition() {
        // Overfetch boundary: the source queries widen to `overfetch =
        // effective_target * 3` candidates, and the temporal post-filter runs
        // *after* that widening. Seed many more than `3 * limit` future/stale
        // facts (all post-filter-excluded) plus one present (valid) fact; the
        // present fact must still surface. This pins that the post-filter and
        // the overfetch widening interact correctly when nearly every fetched
        // candidate is filtered out — the survivor is not lost to attrition.
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let future = Utc::now() + chrono::Duration::hours(1);
        let past = Utc::now() - chrono::Duration::hours(1);

        // limit = 3 ⇒ overfetch = 9; seed 20 excluded candidates (> 3 * limit)
        // so the post-filter would empty the pool without the present fact.
        for i in 0..10 {
            store
                .insert(&make_fact_temporal(
                    &format!("future rust fact {i}"),
                    vec![1.0, 0.0, 0.0, 0.0],
                    Some(future),
                    None,
                ))
                .unwrap();
            store
                .insert(&make_fact_temporal(
                    &format!("stale rust fact {i}"),
                    vec![1.0, 0.0, 0.0, 0.0],
                    None,
                    Some(past),
                ))
                .unwrap();
        }
        store
            .insert(&make_fact("present rust fact", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();

        let query = SearchQuery::new(SearchMode::Fts, 3).text("rust");

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        // Every future/stale candidate is filtered out; only the present one survives.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "present rust fact");
    }

    // --- rerank_depth widening tests (#480) ---
    //
    // Invariant: `effective_limit = rerank_depth.unwrap_or(limit).max(limit)` —
    // the candidate pool can only widen, never shrink. The existing coverage is a
    // full-engine test; these drive the invariant directly through `hybrid_search`.

    #[test]
    fn rerank_depth_smaller_than_limit_clamped_to_limit() {
        // rerank_depth (1) < limit (3): the `.max(limit)` clamps the effective
        // limit back up to `limit`, so the result count is NOT shrunk to 1.
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        for i in 0..5 {
            store
                .insert(&make_fact(
                    &format!("rust fact number {i}"),
                    vec![1.0, 0.0, 0.0, 0.0],
                ))
                .unwrap();
        }

        let query = SearchQuery::new(SearchMode::Fts, 3)
            .text("rust")
            .rerank_depth(1);

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        // Clamped UP to limit (3), not shrunk to rerank_depth (1).
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn rerank_depth_larger_than_limit_widens_pool() {
        // rerank_depth (9) > limit (3): the effective limit widens to 9, so up to
        // 9 candidates pass the assemble truncation (here 5 distinct facts match).
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        for i in 0..5 {
            store
                .insert(&make_fact(
                    &format!("rust fact number {i}"),
                    vec![1.0, 0.0, 0.0, 0.0],
                ))
                .unwrap();
        }

        // Baseline: limit=3, no rerank_depth → truncated to 3.
        let base_query = SearchQuery::new(SearchMode::Fts, 3).text("rust");
        let (base_results, _) = hybrid_search(&conn, &base_query, DIM, None, &BruteForce).unwrap();
        assert_eq!(base_results.len(), 3, "baseline truncates to limit");

        // Widened: limit=3, rerank_depth=9 → effective limit 9 admits all 5.
        let widened_query = SearchQuery::new(SearchMode::Fts, 3)
            .text("rust")
            .rerank_depth(9);
        let (widened_results, _) =
            hybrid_search(&conn, &widened_query, DIM, None, &BruteForce).unwrap();
        assert_eq!(
            widened_results.len(),
            5,
            "rerank_depth widens the candidate pool above limit before truncation"
        );
    }

    mod proptest_rrf {
        use super::*;
        use proptest::prelude::*;

        fn fts_list(max_len: usize) -> impl Strategy<Value = Vec<(i64, f64)>> {
            proptest::collection::vec((1..1000i64, any::<f64>()), 0..max_len)
        }

        fn vec_list(max_len: usize) -> impl Strategy<Value = Vec<(i64, f32)>> {
            proptest::collection::vec((1..1000i64, any::<f32>()), 0..max_len)
        }

        proptest! {
            #[test]
            fn all_input_ids_appear_in_output(
                fts in fts_list(32),
                vec_results in vec_list(32),
                k in 0..120u32,
            ) {
                let merged = rrf_merge(&fts, &vec_results, k);
                let merged_ids: std::collections::HashSet<i64> =
                    merged.iter().map(|&(id, _)| id).collect();
                for &(id, _) in &fts {
                    prop_assert!(merged_ids.contains(&id),
                        "FTS id {id} missing from merged output");
                }
                for &(id, _) in &vec_results {
                    prop_assert!(merged_ids.contains(&id),
                        "Vec id {id} missing from merged output");
                }
            }

            #[test]
            fn descending_order(
                fts in fts_list(16),
                vec_results in vec_list(16),
                k in 0..120u32,
            ) {
                let merged = rrf_merge(&fts, &vec_results, k);
                for window in merged.windows(2) {
                    prop_assert!(window[0].1 >= window[1].1,
                        "not descending: {} < {}", window[0].1, window[1].1);
                }
            }

            #[test]
            fn scores_are_positive(
                fts in fts_list(16),
                vec_results in vec_list(16),
                k in 0..120u32,
            ) {
                let merged = rrf_merge(&fts, &vec_results, k);
                for &(id, score) in &merged {
                    prop_assert!(score > 0.0,
                        "id {id} has non-positive RRF score {score}");
                }
            }
        }
    }
}
