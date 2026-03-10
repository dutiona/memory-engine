use rusqlite::Connection;

use crate::error::Result;
use crate::search::vector::VectorResult;
use crate::types::FactType;

/// Strategy for vector similarity search.
///
/// Implementations provide a single `search` method with the same contract as
/// [`crate::search::vector::vector_search`].  The engine holds a boxed strategy
/// and dispatches through it, allowing runtime selection between algorithms
/// (analogous to introsort dispatching between quicksort / heapsort / insertion
/// sort based on partition size).
///
/// # Object safety
///
/// This trait is object-safe (`&dyn VectorSearchStrategy` / `Box<dyn …>`).
/// `Send + Sync` are required because [`crate::engine::MemoryEngine`] is shared
/// across threads via `Arc`.
pub trait VectorSearchStrategy: Send + Sync {
    /// Search for the `limit` most similar facts to `query_embedding`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if query dimension mismatches,
    /// or `MemoryError::Database` on query failure.
    fn search(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        embed_dim: usize,
        limit: usize,
        fact_type: Option<&FactType>,
        scope_ids: Option<&[i64]>,
    ) -> Result<Vec<VectorResult>>;

    /// Human-readable name for logging and debug output.
    fn name(&self) -> &str;
}

/// Brute-force cosine similarity scan over all active facts.  O(N) per query.
///
/// This is the default strategy and serves as the correctness oracle when
/// testing approximate strategies.
pub struct BruteForce;

impl VectorSearchStrategy for BruteForce {
    fn search(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        embed_dim: usize,
        limit: usize,
        fact_type: Option<&FactType>,
        scope_ids: Option<&[i64]>,
    ) -> Result<Vec<VectorResult>> {
        crate::search::vector::vector_search(
            conn,
            query_embedding,
            embed_dim,
            limit,
            fact_type,
            scope_ids,
        )
    }

    fn name(&self) -> &str {
        "brute_force"
    }
}

/// Configuration for vector search dispatch.
///
/// Documents the intended threshold semantics for switching between brute-force
/// and ANN strategies.  **Not wired into the engine yet** — dispatch requires a
/// second strategy (ANN) to exist.
///
/// # Threshold semantics
///
/// `ann_threshold` is the **total active fact count** at which the engine should
/// prefer an ANN index over brute-force scan.  This is a first approximation;
/// more refined criteria (candidate-set size after SQL filters, embedding
/// dimension, top-k) are future refinements.
///
/// | Value | Effect |
/// |-------|--------|
/// | `0` | Always prefer ANN (when available) |
/// | `50_000` (default) | Switch at ~50K facts |
/// | `usize::MAX` | Always use brute-force |
///
/// The default of 50,000 is a conservative estimate based on the observation
/// that brute-force cosine similarity over 768-dim `f32` vectors stays under
/// ~50 ms up to ~50K–100K facts on modern hardware.  Run the project benchmarks
/// (`cargo bench`) at each release to validate this assumption and adjust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchConfig {
    /// Fact count at which to switch from brute-force to ANN.
    pub ann_threshold: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            ann_threshold: 50_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::types::NewFact;
    use chrono::Utc;

    const DIM: usize = 4;

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_fact(content: &str, embedding: Vec<f32>) -> NewFact {
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
        }
    }

    #[test]
    fn brute_force_through_trait_matches_direct_call() {
        let conn = setup();
        let store = FactStore::new(&conn, DIM);

        store
            .insert(&make_fact("exact", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        store
            .insert(&make_fact("close", vec![0.9, 0.1, 0.0, 0.0]))
            .unwrap();
        store
            .insert(&make_fact("far", vec![0.0, 1.0, 0.0, 0.0]))
            .unwrap();

        let query = [1.0_f32, 0.0, 0.0, 0.0];

        // Direct call
        let direct =
            crate::search::vector::vector_search(&conn, &query, DIM, 2, None, None).unwrap();

        // Through trait
        let strategy = BruteForce;
        let via_trait = strategy.search(&conn, &query, DIM, 2, None, None).unwrap();

        assert_eq!(direct.len(), via_trait.len());
        for (d, t) in direct.iter().zip(via_trait.iter()) {
            assert_eq!(d.fact_id, t.fact_id);
            assert!((d.score - t.score).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn search_config_default_threshold() {
        assert_eq!(SearchConfig::default().ann_threshold, 50_000);
    }

    #[test]
    fn search_config_threshold_boundary_semantics() {
        // ann_threshold = 0 means "always prefer ANN"
        let always_ann = SearchConfig { ann_threshold: 0 };
        assert_eq!(always_ann.ann_threshold, 0);

        // ann_threshold = usize::MAX means "always brute-force"
        let always_brute = SearchConfig {
            ann_threshold: usize::MAX,
        };
        assert_eq!(always_brute.ann_threshold, usize::MAX);
    }
}
