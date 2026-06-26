use rusqlite::Connection;

use crate::error::Result;
use crate::store::summaries::SummaryStore;
use crate::traits::{EmbeddingProvider, SummarizableContent, SummaryGenerator};
use crate::types::{ConsolidationLevel, NewSummary};

/// Compute the global summary from the **in-memory** cluster summaries, touching no
/// store (#409). Returns `None` when there are no cluster summaries to integrate.
///
/// The engine passes the cluster summaries it just computed this run (#409: not a store
/// re-read), so the consumer `summarize`/`embed` IO here runs lock-free. Semantically
/// identical to re-reading them: the global summary aggregates exactly the clusters this
/// run produced.
///
/// # Errors
///
/// Propagates errors from the `SummaryGenerator` or `EmbeddingProvider`.
/// Returns `MemoryError::EmbeddingDimension` if the embedder returns an embedding whose
/// length does not match `embed_dim`.
pub(super) fn compute_global(
    cluster_summaries: &[NewSummary],
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<NewSummary>> {
    if cluster_summaries.is_empty() {
        return Ok(None);
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
    for s in cluster_summaries {
        all_source_ids.extend_from_slice(&s.source_fact_ids);
    }

    // Global summaries are intentionally root-scoped (scope_id=1). They aggregate
    // across all cluster-level summaries regardless of individual cluster scopes.
    Ok(Some(NewSummary {
        content: global_text,
        embedding: global_embedding,
        level: ConsolidationLevel::Global,
        source_fact_ids: all_source_ids,
        scope_id: 1,
        created_at: now,
    }))
}

/// Apply the computed global summary inside the caller's write context (#409): clear the
/// prior global summary (idempotent), then insert the new one if present. `None` leaves
/// the store with no global summary (the cleared state).
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure, or `MemoryError::Serialization` on
/// JSON serialization failure.
pub(super) fn apply_global(
    conn: &Connection,
    embed_dim: usize,
    summary: Option<&NewSummary>,
) -> Result<()> {
    let summary_store = SummaryStore::new(conn, embed_dim);
    summary_store.delete_by_level(&ConsolidationLevel::Global)?;
    if let Some(s) = summary {
        summary_store.insert(s)?;
    }
    Ok(())
}

/// Global integration pass — compute + apply on a single connection.
///
/// Thin wrapper over [`compute_global`] + [`apply_global`], kept as the per-pass entry
/// point exercised by this module's unit tests (hence `#[cfg(test)]`). Reads the current
/// cluster summaries from the store, summarizes them, and applies the result. Returns 1
/// if a global summary was created, 0 if no clusters exist. The engine no longer calls
/// this — it runs [`compute_global`] over the in-memory cluster summaries it just
/// produced and applies inside the final transaction (#409) — but the behavior is
/// identical.
#[cfg(test)]
fn global_integration(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize> {
    // The store hands back `Summary` (with ids); `compute_global` consumes the id-less
    // `NewSummary` the engine path produces, so bridge the stored rows into that shape.
    let cluster_summaries: Vec<NewSummary> = SummaryStore::new(conn, embed_dim)
        .list_by_level(&ConsolidationLevel::Cluster)?
        .into_iter()
        .map(|s| NewSummary {
            content: s.content,
            embedding: s.embedding,
            level: s.level,
            source_fact_ids: s.source_fact_ids,
            scope_id: s.scope_id,
            created_at: s.created_at,
        })
        .collect();
    let global = compute_global(&cluster_summaries, generator, embedder, embed_dim, now)?;
    apply_global(conn, embed_dim, global.as_ref())?;
    Ok(usize::from(global.is_some()))
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
        let count =
            global_integration(&conn, &mock_gen, &mock_embed, dim, chrono::Utc::now()).unwrap();
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
        let count =
            global_integration(&conn, &mock_gen, &mock_embed, dim, chrono::Utc::now()).unwrap();
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
        global_integration(&conn, &mock_gen, &mock_embed, dim, chrono::Utc::now()).unwrap();
        global_integration(&conn, &mock_gen, &mock_embed, dim, chrono::Utc::now()).unwrap();

        let global = store.list_by_level(&ConsolidationLevel::Global).unwrap();
        assert_eq!(global.len(), 1); // Not 2
    }
}
