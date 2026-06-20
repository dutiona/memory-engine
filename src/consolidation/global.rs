use rusqlite::Connection;

use crate::error::Result;
use crate::store::summaries::SummaryStore;
use crate::traits::{EmbeddingProvider, SummarizableContent, SummaryGenerator};
use crate::types::{ConsolidationLevel, NewSummary};

/// Global integration pass.
///
/// Summarizes all cluster-level summaries into a single global summary.
/// Deletes prior global summaries first (idempotent).
///
/// Returns 1 if a global summary was created, 0 if no clusters exist.
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure, or propagates errors from
/// the `SummaryGenerator` or `EmbeddingProvider`.
/// Returns `MemoryError::EmbeddingDimension` if the embedder returns an embedding
/// whose length does not match `embed_dim`.
/// Returns `MemoryError::Serialization` on JSON serialization failure.
pub fn global_integration(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
) -> Result<usize> {
    let summary_store = SummaryStore::new(conn, embed_dim);

    // Idempotent: clear previous global summaries
    summary_store.delete_by_level(&ConsolidationLevel::Global)?;

    let cluster_summaries = summary_store.list_by_level(&ConsolidationLevel::Cluster)?;
    if cluster_summaries.is_empty() {
        return Ok(0);
    }

    // Summarize the cluster summaries directly — borrow each summary's text and
    // embedding, with no throwaway `Fact` structs and no clones (#273, #495).
    let items: Vec<SummarizableContent<'_>> = cluster_summaries
        .iter()
        .map(|s| SummarizableContent::new(&s.content, &s.embedding))
        .collect();

    let (global_text, global_embedding) =
        super::summarize_and_embed(generator, embedder, &items, embed_dim)?;

    // Collect all source fact ids from all cluster summaries.
    // Pre-size to avoid repeated reallocations: flat_map has no upper-bound hint.
    let total_source_ids: usize = cluster_summaries
        .iter()
        .map(|s| s.source_fact_ids.len())
        .sum();
    let mut all_source_ids: Vec<i64> = Vec::with_capacity(total_source_ids);
    for s in &cluster_summaries {
        all_source_ids.extend_from_slice(&s.source_fact_ids);
    }

    // Global summaries are intentionally root-scoped (scope_id=1).
    // They aggregate across all cluster-level summaries regardless of
    // individual cluster scopes.
    summary_store.insert(&NewSummary {
        content: global_text,
        embedding: global_embedding,
        level: ConsolidationLevel::Global,
        source_fact_ids: all_source_ids,
        scope_id: 1,
        created_at: chrono::Utc::now(),
    })?;

    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::store::schema::{init_schema, open_memory};

    /// Mock generator that concatenates cluster-summary contents.
    struct MockGenerator;

    impl SummaryGenerator for MockGenerator {
        fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String> {
            Ok(items.iter().map(|i| i.text).collect::<Vec<_>>().join(" | "))
        }
    }

    /// Mock embedder returning a fixed-dimension constant vector.
    struct MockEmbedder {
        embed_dim: usize,
    }

    impl EmbeddingProvider for MockEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; self.embed_dim])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", self.embed_dim)
        }
    }

    #[test]
    fn global_integration_summarizes_clusters() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;
        let store = SummaryStore::new(&conn, dim);

        // Insert 3 cluster summaries
        for i in 0..3 {
            store
                .insert(&NewSummary {
                    content: format!("cluster {i}"),
                    embedding: vec![0.1; dim],
                    level: ConsolidationLevel::Cluster,
                    source_fact_ids: vec![i64::from(i)],
                    scope_id: 1,
                    created_at: Utc::now(),
                })
                .unwrap();
        }

        let mock_gen = MockGenerator;
        let mock_embed = MockEmbedder { embed_dim: dim };
        let count = global_integration(&conn, &mock_gen, &mock_embed, dim).unwrap();
        assert_eq!(count, 1);

        let global = store.list_by_level(&ConsolidationLevel::Global).unwrap();
        assert_eq!(global.len(), 1);
        assert!(global[0].content.contains("cluster 0"));
        assert!(global[0].content.contains("cluster 2"));
        // Order-independent: global summary must contain all three source IDs.
        let mut got = global[0].source_fact_ids.clone();
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2]);
    }

    #[test]
    fn no_clusters_returns_zero() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;

        let mock_gen = MockGenerator;
        let mock_embed = MockEmbedder { embed_dim: dim };
        let count = global_integration(&conn, &mock_gen, &mock_embed, dim).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn idempotent_global() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;
        let store = SummaryStore::new(&conn, dim);

        store
            .insert(&NewSummary {
                content: "cluster A".into(),
                embedding: vec![0.1; dim],
                level: ConsolidationLevel::Cluster,
                source_fact_ids: vec![1, 2],
                scope_id: 1,
                created_at: Utc::now(),
            })
            .unwrap();

        let mock_gen = MockGenerator;
        let mock_embed = MockEmbedder { embed_dim: dim };

        // Run twice
        global_integration(&conn, &mock_gen, &mock_embed, dim).unwrap();
        global_integration(&conn, &mock_gen, &mock_embed, dim).unwrap();

        let global = store.list_by_level(&ConsolidationLevel::Global).unwrap();
        assert_eq!(global.len(), 1); // Not 2
    }
}
