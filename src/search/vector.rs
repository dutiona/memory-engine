use rusqlite::Connection;

use crate::error::Result;
use crate::store::deserialize_embedding;

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
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
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
) -> Result<Vec<VectorResult>> {
    let mut stmt = conn.prepare("SELECT id, embedding FROM facts WHERE t_expired IS NULL")?;

    let rows = stmt.query_map([], |row| {
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
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
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

        let results = vector_search(&conn, &query, DIM, 3).unwrap();
        assert_eq!(results.len(), 3);
        // Descending order by score
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
        // Top result should be the exact match
        assert!((results[0].score - 1.0).abs() < 1e-5);
    }
}
