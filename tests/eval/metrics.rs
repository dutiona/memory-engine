//! Information Retrieval quality metrics for the evaluation harness.
//!
//! All functions operate on fact IDs (not `SearchResult`), keeping the
//! metrics module decoupled from engine types.

use std::collections::{HashMap, HashSet};

/// Precision@K: fraction of top-K retrieved that are relevant.
pub fn precision_at_k(retrieved: &[i64], relevant: &HashSet<i64>, k: usize) -> f64 {
    let top_k = &retrieved[..k.min(retrieved.len())];
    if top_k.is_empty() {
        return 0.0;
    }
    let hits = top_k.iter().filter(|id| relevant.contains(id)).count();
    hits as f64 / top_k.len() as f64
}

/// Recall@K: fraction of relevant docs found in top-K.
///
/// Returns 1.0 when `relevant` is empty (vacuously true).
pub fn recall_at_k(retrieved: &[i64], relevant: &HashSet<i64>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    let top_k = &retrieved[..k.min(retrieved.len())];
    let hits = top_k.iter().filter(|id| relevant.contains(id)).count();
    hits as f64 / relevant.len() as f64
}

/// Mean Reciprocal Rank: 1/rank of first relevant result.
///
/// Returns 0.0 when no relevant result is found.
pub fn mrr(retrieved: &[i64], relevant: &HashSet<i64>) -> f64 {
    for (i, id) in retrieved.iter().enumerate() {
        if relevant.contains(id) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Normalized Discounted Cumulative Gain at K.
///
/// `grades` maps fact IDs to graded relevance (0=irrelevant, 1=marginal,
/// 2=relevant, 3=highly relevant). Ungraded IDs default to 0.
///
/// Returns 0.0 when IDCG is 0 (no relevant documents).
pub fn ndcg_at_k(retrieved: &[i64], grades: &HashMap<i64, u32>, k: usize) -> f64 {
    let dcg = dcg_at_k(retrieved, grades, k);

    // Ideal DCG: sort all grades descending, compute DCG over that ordering
    let mut ideal_grades: Vec<u32> = grades.values().copied().collect();
    ideal_grades.sort_unstable_by(|a, b| b.cmp(a));
    let ideal_ids: Vec<i64> = (0..ideal_grades.len() as i64).collect();
    let ideal_map: HashMap<i64, u32> = ideal_ids.iter().copied().zip(ideal_grades).collect();
    let idcg = dcg_at_k(&ideal_ids, &ideal_map, k);

    if idcg == 0.0 {
        return 0.0;
    }
    dcg / idcg
}

/// Discounted Cumulative Gain at K (internal).
fn dcg_at_k(retrieved: &[i64], grades: &HashMap<i64, u32>, k: usize) -> f64 {
    retrieved[..k.min(retrieved.len())]
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let g = f64::from(*grades.get(id).unwrap_or(&0));
            (g.exp2() - 1.0) / (i as f64 + 2.0).log2()
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[i64]) -> HashSet<i64> {
        ids.iter().copied().collect()
    }

    fn grades(pairs: &[(i64, u32)]) -> HashMap<i64, u32> {
        pairs.iter().copied().collect()
    }

    // --- precision_at_k ---

    #[test]
    fn precision_perfect() {
        assert!((precision_at_k(&[1, 2, 3], &set(&[1, 2, 3]), 3) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn precision_half() {
        assert!((precision_at_k(&[1, 2, 3, 4], &set(&[2, 4]), 4) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn precision_empty_retrieved() {
        assert!((precision_at_k(&[], &set(&[1, 2]), 5)).abs() < f64::EPSILON);
    }

    #[test]
    fn precision_k_larger_than_retrieved() {
        assert!(
            (precision_at_k(&[1, 2], &set(&[1, 2, 3]), 10) - 1.0).abs() < f64::EPSILON,
            "k > len should use actual length"
        );
    }

    #[test]
    fn precision_no_relevant_in_topk() {
        assert!((precision_at_k(&[10, 20, 30], &set(&[1, 2]), 3)).abs() < f64::EPSILON);
    }

    // --- recall_at_k ---

    #[test]
    fn recall_perfect() {
        assert!((recall_at_k(&[1, 2, 3], &set(&[1, 2, 3]), 3) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn recall_partial() {
        assert!((recall_at_k(&[1, 2], &set(&[1, 2, 3, 4]), 2) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn recall_empty_relevant() {
        assert!(
            (recall_at_k(&[1, 2], &set(&[]), 2) - 1.0).abs() < f64::EPSILON,
            "vacuously true"
        );
    }

    // --- mrr ---

    #[test]
    fn mrr_first_hit() {
        assert!((mrr(&[1, 2, 3], &set(&[1])) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn mrr_second_hit() {
        assert!((mrr(&[10, 1, 2], &set(&[1])) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn mrr_no_hit() {
        assert!((mrr(&[10, 20, 30], &set(&[1]))).abs() < f64::EPSILON);
    }

    // --- ndcg_at_k ---

    #[test]
    fn ndcg_perfect_ranking() {
        // Ideal order: grade 3 first, then 2, then 1
        let retrieved = [1, 2, 3];
        let g = grades(&[(1, 3), (2, 2), (3, 1)]);
        let score = ndcg_at_k(&retrieved, &g, 3);
        assert!(
            (score - 1.0).abs() < 1e-10,
            "perfect ranking should yield nDCG=1.0, got {score}"
        );
    }

    #[test]
    fn ndcg_no_relevant_docs() {
        let g = grades(&[]);
        assert!((ndcg_at_k(&[1, 2, 3], &g, 3)).abs() < f64::EPSILON);
    }

    #[test]
    fn ndcg_empty_retrieved() {
        let g = grades(&[(1, 3)]);
        assert!((ndcg_at_k(&[], &g, 5)).abs() < f64::EPSILON);
    }

    #[test]
    fn ndcg_suboptimal_ranking() {
        // Worst order: grade 1 first, then 2, then 3
        let retrieved = [3, 2, 1];
        let g = grades(&[(1, 3), (2, 2), (3, 1)]);
        let score = ndcg_at_k(&retrieved, &g, 3);
        assert!(
            score < 1.0 && score > 0.0,
            "suboptimal ranking should yield 0 < nDCG < 1, got {score}"
        );
    }
}
