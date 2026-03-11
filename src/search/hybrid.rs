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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchType {
    Fts,
    Vector,
    Both,
}

/// A unified search query across FTS5 and vector sources.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub mode: SearchMode,
    pub limit: usize,
    pub valid_at: Option<DateTime<Utc>>,
    pub fact_type: Option<FactType>,
    pub scope: Option<crate::types::ScopeQuery>,
}

/// A search result with the full fact, combined score, and match source.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub fact: Fact,
    pub score: f64,
    pub match_type: MatchType,
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
        #[allow(clippy::cast_possible_truncation)]
        let rank_u32 = rank as u32;
        *scores.entry(id).or_default() += 1.0 / f64::from(k + rank_u32 + 1);
    }
    for (rank, &(id, _)) in vec.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let rank_u32 = rank as u32;
        *scores.entry(id).or_default() += 1.0 / f64::from(k + rank_u32 + 1);
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
) -> Result<Vec<SearchResult>> {
    // When valid_at is set, over-fetch 3x to compensate for post-filter attrition
    let overfetch = query.limit.saturating_mul(3).max(query.limit);

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
    let store = FactStore::new(conn, embed_dim);
    let mut results = Vec::new();
    for (id, score) in ranked {
        if results.len() >= query.limit {
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

    Ok(results)
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
            valid_at: None,
            fact_type: None,
            scope: None,
        };

        let results = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
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
            valid_at: None,
            fact_type: None,
            scope: None,
        };

        let results = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
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
            valid_at: None,
            fact_type: None,
            scope: None,
        };

        let results = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
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
            valid_at: None,
            fact_type: Some(FactType::Semantic),
            scope: None,
        };

        let results = hybrid_search(&conn, &query, DIM, None, &BruteForce).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.fact_type, FactType::Semantic);
    }
}
