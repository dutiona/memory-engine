use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::search::fts::fts_search;
use crate::search::strategy::VectorSearchStrategy;
use crate::store::facts::FactStore;
use crate::types::{Fact, FactType};

/// How to combine search sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMode {
    Fts,
    Vector,
    Hybrid,
}

/// Which source(s) contributed to a result.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MatchType {
    Fts,
    Vector,
    Both,
    /// Result came from importance-ranked store query (no text/vector search).
    ImportanceRank,
    /// Result came from a decompressed archive `.pak` file (slow fallback).
    /// Always present in the enum for serde ABI stability across feature combinations.
    Archive,
}

/// A unified search query across FTS5 and vector sources.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub mode: SearchMode,
    pub limit: usize,
    /// How many candidates to pass to the reranker before truncating to `limit`.
    /// Clamped to at least `limit` — can only widen the candidate pool, never shrink it.
    /// When `None`, falls back to `limit` (no over-fetch).
    pub rerank_depth: Option<usize>,
    pub valid_at: Option<DateTime<Utc>>,
    pub fact_type: Option<FactType>,
    pub scope: Option<crate::types::ScopeQuery>,
}

/// A search result with the full fact, combined score, and match source.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub fact: Fact,
    pub score: f64,
    pub match_type: MatchType,
}

/// Diagnostic signals from a query execution, enabling consumer-side
/// abstention classification.
///
/// The engine computes mechanical retrieval signals. The consumer interprets
/// them alongside content understanding to classify the four abstention types
/// (Retrieval / Evidence / Reasoning / Decay) from research note 18.
///
/// ## Interpreting `expired_matches`
///
/// `expired_matches` counts ALL expired facts matching the query, regardless
/// of expiry reason (Ebbinghaus decay, conflict resolution, deduplication).
/// The engine does not currently track expiry provenance — `t_expired` is a
/// generic tombstone. Consumers wanting true decay-only counts should
/// cross-reference with `ExpiredReason` via `explain_fact()`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct QueryDiagnostics {
    /// Total candidates found before post-filters (temporal, importance, pinned).
    pub candidates_before_filter: usize,
    /// Total results returned after all filters and truncation.
    pub results_returned: usize,
    /// Number of expired facts matching the FTS5 query text with the same
    /// `fact_type` and `scope` filters. Does NOT apply `min_importance_score`
    /// or `pinned_only` — the probe answers "how many expired facts match
    /// the search terms?" not "how many would survive the full filter chain?"
    ///
    /// `None` = probe not run (opt-in via `include_expired_probe`).
    /// `None` also when query is vector-only (no FTS5 terms to probe).
    pub expired_matches: Option<usize>,
    /// Number of FTS candidates before merge.
    pub fts_candidates: usize,
    /// Number of vector candidates before merge.
    pub vector_candidates: usize,
    /// Number of archive `.pak` files scanned. `0` when archives not searched.
    #[cfg(feature = "archive")]
    pub archive_paks_scanned: usize,
    /// Total milliseconds spent decompressing and searching archives.
    #[cfg(feature = "archive")]
    pub archive_search_ms: u64,
}

/// Complete query response including results and diagnostic metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryResponse {
    pub results: Vec<SearchResult>,
    pub diagnostics: QueryDiagnostics,
}

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
pub fn hybrid_search(
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

    let fts_candidate_count = fts_results.len();
    let vec_candidate_count = vec_results.len();

    // Build ID sets for match_type determination
    let fts_ids: HashSet<i64> = fts_results.iter().map(|r| r.fact_id).collect();
    let vec_ids: HashSet<i64> = vec_results.iter().map(|r| r.fact_id).collect();

    // Determine ranked IDs with scores
    let ranked: Vec<(i64, f64)> = match query.mode {
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
            rrf_merge(&fts_pairs, &vec_pairs, 60)
        }
    };

    // Load full facts. t_expired, fact_type, and scope are now SQL-level filters.
    // Only valid_at remains as a post-filter (complex temporal semantics).
    // Collect up to effective_target candidates (rerank_depth widens the pool).
    // Clamped to at least query.limit so rerank_depth can never shrink results.
    let effective_limit = effective_target;
    let ranked_count = ranked.len();
    let store = FactStore::new(conn, embed_dim);
    let mut results = Vec::new();
    for (id, score) in ranked {
        if results.len() >= effective_limit {
            break;
        }
        let Ok(fact) = store.get(id) else {
            tracing::warn!("failed to load fact id={id} during result collection, skipping");
            continue;
        };

        // Apply temporal filter (post-filter — complex temporal semantics).
        // Use explicit valid_at if provided, otherwise default to now.
        // This ensures future-dated facts (t_valid > now) are invisible to
        // regular queries — they only surface via list_due()/resume_context().
        let cutoff = query.valid_at.unwrap_or_else(Utc::now);
        if let Some(t_valid) = fact.t_valid {
            if t_valid > cutoff {
                continue;
            }
        }
        if let Some(t_invalid) = fact.t_invalid {
            if t_invalid <= cutoff {
                continue;
            }
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

    Ok((results, diagnostics))
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

    fn make_fact_with(content: &str, embedding: Vec<f32>, fact_type: FactType) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding,
            fact_type,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
    }

    fn make_fact(content: &str, embedding: Vec<f32>) -> NewFact {
        make_fact_with(content, embedding, FactType::Episodic)
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

        let query = SearchQuery {
            text: Some("Rust".into()),
            embedding: Some(vec![1.0, 0.0, 0.0, 0.0]),
            mode: SearchMode::Hybrid,
            limit: 3,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        };

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

        let query = SearchQuery {
            text: Some("Rust".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        };

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

        let query = SearchQuery {
            text: None,
            embedding: Some(vec![1.0, 0.0, 0.0, 0.0]),
            mode: SearchMode::Vector,
            limit: 1,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        };

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

        let query = SearchQuery {
            text: Some("fact".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: Some(FactType::Semantic),
            scope: None,
        };

        let (results, _diag) = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.fact_type, FactType::Semantic);
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
                k in 1..120u32,
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
                k in 1..120u32,
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
                k in 1..120u32,
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
