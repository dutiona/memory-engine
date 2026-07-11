//! Hybrid search: FTS5 (BM25) + vector (cosine) + Reciprocal Rank Fusion.
//!
//! Extracted from the facade's `search/hybrid.rs` in Wave 2 #816 / S4, sub-PR 2.
//! The sync `&Connection` twin (`hybrid_search`/`collect_candidates`, #631 Stage-C
//! scaffolding kept only as the sync↔port parity oracle) is deleted with this move
//! rather than carried over: the engine has driven [`port_hybrid_search`] exclusively
//! since the #631 cutover (`engine/query.rs`'s `query`/`execute_query`), so the sync
//! oracle and its `port_hybrid_search_parity_with_sync` fusion test served no further
//! purpose. `rank_candidates` (the sync twin's own FTS/vector ranking helper) is
//! deleted for the same reason — `port_hybrid_search` ranks its `(id, score)` pairs
//! inline and never called it.

use std::collections::{HashMap, HashSet};

use chrono::Utc;

use me_storage::{FactFilter, StorageBackend, TemporalFilter};
use me_types::error::Result;
use me_types::types::Fact;
use me_types::types::search::{
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

/// Pure fusion stage of [`port_hybrid_search`]: re-order `ranked` to its rank order,
/// apply the `valid_at`/`t_invalid` temporal post-filter, assign `match_type`,
/// truncate to the effective limit, and build the diagnostics. The only inputs
/// are the ranked `(id, score)` list, the per-source id sets, the candidate
/// counts, and the materialized facts — no I/O.
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

/// Port-driven hybrid search: the engine's live retrieval path.
///
/// FTS5/vector channels + fact-fetch driven through `&dyn StorageBackend`, then
/// RRF-fused by the pure `assemble_results`. `engine/query.rs`'s `query`/
/// `execute_query` call this exclusively (moved to `me-query` alongside it —
/// Wave 2 #816 / S4).
///
/// # Errors
/// Propagates backend errors (`MemoryError::Storage`/`Database`,
/// `EmbeddingDimension` on a wrong-length query embedding).
pub async fn port_hybrid_search(
    storage: &dyn StorageBackend,
    query: &SearchQuery,
    scope_ids: Option<&[i64]>,
) -> Result<(Vec<SearchResult>, QueryDiagnostics)> {
    let effective_target = query.rerank_depth.unwrap_or(query.limit).max(query.limit);
    let overfetch = effective_target.saturating_mul(3).max(effective_target);

    // The lexical/vector channels honor exactly `fact_type` + `scope_ids` +
    // Active (the same SQL-level predicates the sync free functions used to push down).
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
    use me_types::types::{FactType, ScopeQuery};

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

    // --- SearchQuery builder tests (pure; moved with rrf_merge, not repointed) ---

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
            .scope(ScopeQuery::Exact("user:alice".into()));

        assert_eq!(q.text.as_deref(), Some("rust"));
        assert_eq!(q.embedding, Some(vec![1.0, 0.0, 0.0, 0.0]));
        // `mode`/`limit` setters override the `new` arguments (last write wins).
        assert_eq!(q.mode, SearchMode::Hybrid);
        assert_eq!(q.limit, 9);
        assert_eq!(q.rerank_depth, Some(20));
        assert_eq!(q.valid_at, Some(at));
        assert_eq!(q.fact_type, Some(FactType::Semantic));
        assert_eq!(q.scope, Some(ScopeQuery::Exact("user:alice".into())));
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
