use rusqlite::{Connection, params};

use crate::error::Result;
use crate::store::deserialize_embedding;
use crate::store::facts::fact_type_to_str;
use crate::types::FactType;

/// A single vector search result with fact id and cosine similarity score.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorResult {
    pub fact_id: i64,
    pub score: f32,
}

/// Compute the cosine similarity between two vectors.
///
/// Returns 0.0 if either vector has zero magnitude (avoids NaN).
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut norm_a, mut norm_b) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot = x.mul_add(y, dot);
        norm_a = x.mul_add(x, norm_a);
        norm_b = y.mul_add(y, norm_b);
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

/// Brute-force vector similarity search over active facts.
///
/// Streams all active facts (`t_expired IS NULL`) from `SQLite`, deserializes
/// their embeddings, computes cosine similarity against `query_embedding`,
/// and returns the top `limit` results sorted descending by score.
///
/// # Errors
///
/// Returns `MemoryError::Database` on query failure, or
/// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
pub fn vector_search(
    conn: &Connection,
    query_embedding: &[f32],
    embed_dim: usize,
    limit: usize,
    fact_type: Option<&FactType>,
    scope_ids: Option<&[i64]>,
) -> Result<Vec<VectorResult>> {
    if query_embedding.len() != embed_dim {
        return Err(crate::error::MemoryError::EmbeddingDimension {
            expected: embed_dim,
            actual: query_embedding.len(),
        });
    }

    let ft_str: Option<&str> = fact_type.map(fact_type_to_str);
    let scope_json: Option<String> = scope_ids.map(serde_json::to_string).transpose()?;

    let mut stmt = conn.prepare(
        "SELECT id, embedding FROM facts
         WHERE t_expired IS NULL
           AND (?1 IS NULL OR fact_type = ?1)
           AND (?2 IS NULL OR scope_id IN (SELECT value FROM json_each(?2)))",
    )?;

    let rows = stmt.query_map(params![ft_str, scope_json], |row| {
        let id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        Ok((id, blob))
    })?;

    let mut scored: Vec<VectorResult> = Vec::new();
    for row in rows {
        let (id, blob) = row?;
        let embedding = deserialize_embedding(&blob, embed_dim)?;
        let score = cosine_similarity(query_embedding, &embedding);
        scored.push(VectorResult { fact_id: id, score });
    }

    // Partial sort: O(N) partition then sort only top `limit` elements
    if scored.len() > limit {
        scored.select_nth_unstable_by(limit, |a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);
    }
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{FactType, NewFact};
    use chrono::Utc;

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_fact_with_embedding(content: &str, embedding: Vec<f32>) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding,
            fact_type: FactType::Episodic,
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

    #[test]
    fn cosine_identical_vectors() {
        let a = [1.0_f32, 0.0];
        let b = [1.0_f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = [1.0_f32, 0.0];
        let b = [0.0_f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let a = [1.0_f32, 0.0];
        let b = [-1.0_f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_zero_vector_returns_zero() {
        let a = [0.0_f32, 0.0];
        let b = [1.0_f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < f32::EPSILON);
        // Also both zero
        let sim2 = cosine_similarity(&a, &a);
        assert!(sim2.abs() < f32::EPSILON);
    }

    #[test]
    fn vector_search_rejects_wrong_query_dimension() {
        let conn = setup();
        let wrong_dim_query = [1.0_f32, 0.0]; // DIM is 4, query is 2
        let result = vector_search(&conn, &wrong_dim_query, DIM, 3, None, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            crate::error::MemoryError::EmbeddingDimension {
                expected: 4,
                actual: 2
            }
        ));
    }

    #[test]
    fn vector_search_returns_top_k_descending() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        // Query embedding: [1, 0, 0, 0]
        let query = [1.0_f32, 0.0, 0.0, 0.0];

        // Insert 5 facts with known embeddings of varying similarity to query
        store
            .insert(&make_fact_with_embedding("exact", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap(); // cosine = 1.0
        store
            .insert(&make_fact_with_embedding("close", vec![0.9, 0.1, 0.0, 0.0]))
            .unwrap(); // high
        store
            .insert(&make_fact_with_embedding(
                "medium",
                vec![0.5, 0.5, 0.0, 0.0],
            ))
            .unwrap(); // medium
        store
            .insert(&make_fact_with_embedding("far", vec![0.0, 1.0, 0.0, 0.0]))
            .unwrap(); // cosine = 0.0
        store
            .insert(&make_fact_with_embedding(
                "opposite",
                vec![-1.0, 0.0, 0.0, 0.0],
            ))
            .unwrap(); // cosine = -1.0

        let results = vector_search(&conn, &query, DIM, 3, None, None).unwrap();
        assert_eq!(results.len(), 3);
        // Descending order by score
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
        // Top result should be the exact match
        assert!((results[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn vector_search_filters_by_scope() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        // Need scope 2 to exist for FK
        conn.execute(
            "INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (2, 1, 'test', 1)",
            [],
        )
        .unwrap();

        let mut fact1 = make_fact_with_embedding("fact one", vec![1.0, 0.0, 0.0, 0.0]);
        fact1.scope_id = 1;
        store.insert(&fact1).unwrap();
        let mut fact2 = make_fact_with_embedding("fact two", vec![0.9, 0.1, 0.0, 0.0]);
        fact2.scope_id = 2;
        store.insert(&fact2).unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results = vector_search(&conn, &query, DIM, 10, None, Some(&[1])).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn vector_search_filters_by_fact_type() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);
        let mut fact1 = make_fact_with_embedding("fact one", vec![1.0, 0.0, 0.0, 0.0]);
        fact1.fact_type = FactType::Semantic;
        store.insert(&fact1).unwrap();
        store
            .insert(&make_fact_with_embedding(
                "fact two",
                vec![0.9, 0.1, 0.0, 0.0],
            ))
            .unwrap(); // Episodic

        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let results =
            vector_search(&conn, &query, DIM, 10, Some(&FactType::Semantic), None).unwrap();
        assert_eq!(results.len(), 1);
    }

    mod proptest_cosine {
        use super::*;
        use proptest::prelude::*;

        // Bounded to avoid f32 overflow in squared-sum (x*x).
        // Real embeddings are typically in [-1, 1] or small magnitudes.
        fn bounded_f32() -> impl Strategy<Value = f32> {
            -1e18_f32..1e18_f32
        }

        fn nonzero_vec(max_len: usize) -> impl Strategy<Value = Vec<f32>> {
            proptest::collection::vec(bounded_f32(), 1..max_len)
                .prop_filter("at least one nonzero", |v| v.iter().any(|&x| x != 0.0))
        }

        proptest! {
            #[test]
            fn symmetry(
                a in proptest::collection::vec(bounded_f32(), 1..64usize),
                b in proptest::collection::vec(bounded_f32(), 1..64usize),
            ) {
                let len = a.len().min(b.len());
                let a = &a[..len];
                let b = &b[..len];
                let ab = cosine_similarity(a, b);
                let ba = cosine_similarity(b, a);
                prop_assert!((ab - ba).abs() < 1e-6,
                    "symmetry violated: cos(a,b)={ab} != cos(b,a)={ba}");
            }

            #[test]
            fn identical_vectors_equal_one(v in nonzero_vec(64)) {
                let sim = cosine_similarity(&v, &v);
                prop_assert!((sim - 1.0).abs() < 1e-5,
                    "identical vectors should give 1.0, got {sim}");
            }

            #[test]
            fn bounded(
                a in proptest::collection::vec(bounded_f32(), 1..64usize),
                b in proptest::collection::vec(bounded_f32(), 1..64usize),
            ) {
                let len = a.len().min(b.len());
                let sim = cosine_similarity(&a[..len], &b[..len]);
                prop_assert!((-1.0 - 1e-5..=1.0 + 1e-5).contains(&sim),
                    "cosine similarity {sim} out of [-1, 1] bounds");
            }
        }
    }
}
