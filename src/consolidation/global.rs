use rusqlite::Connection;

use crate::error::Result;
use crate::store::summaries::SummaryStore;
use crate::traits::SummaryGenerator;
use crate::types::{ConsolidationLevel, Fact, NewSummary};

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
/// the `SummaryGenerator`.
pub fn global_integration(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embed_dim: usize,
) -> Result<usize> {
    let summary_store = SummaryStore::new(conn, embed_dim);

    // Idempotent: clear previous global summaries
    summary_store.delete_by_level(&ConsolidationLevel::Global)?;

    let cluster_summaries = summary_store.list_by_level(&ConsolidationLevel::Cluster)?;
    if cluster_summaries.is_empty() {
        return Ok(0);
    }

    // Convert summaries to Fact-like structs for the SummaryGenerator trait
    let pseudo_facts: Vec<Fact> = cluster_summaries
        .iter()
        .map(|s| Fact {
            id: s.id,
            content: s.content.clone(),
            content_hash: String::new(),
            embedding: s.embedding.clone(),
            fact_type: crate::types::FactType::Semantic,
            t_created: s.created_at,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: s.scope_id,
            importance: 1.0,
            access_count: 0,
            last_accessed: s.created_at,
            metadata: serde_json::json!({}),
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        })
        .collect();

    let global_text = generator.summarize(&pseudo_facts)?;
    let global_embedding = generator.embed(&global_text)?;

    // Collect all source fact ids from all cluster summaries
    let all_source_ids: Vec<i64> = cluster_summaries
        .iter()
        .flat_map(|s| s.source_fact_ids.iter().copied())
        .collect();

    // Global summaries are intentionally root-scoped (scope_id=1).
    // They aggregate across all cluster-level summaries regardless of
    // individual cluster scopes. See issue #11.
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

    struct MockGenerator {
        embed_dim: usize,
    }

    impl SummaryGenerator for MockGenerator {
        fn summarize(&self, facts: &[Fact]) -> Result<String> {
            Ok(facts
                .iter()
                .map(|f| f.content.as_str())
                .collect::<Vec<_>>()
                .join(" | "))
        }

        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; self.embed_dim])
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

        let mock_gen = MockGenerator { embed_dim: dim };
        let count = global_integration(&conn, &mock_gen, dim).unwrap();
        assert_eq!(count, 1);

        let global = store.list_by_level(&ConsolidationLevel::Global).unwrap();
        assert_eq!(global.len(), 1);
        assert!(global[0].content.contains("cluster 0"));
        assert!(global[0].content.contains("cluster 2"));
        assert_eq!(global[0].source_fact_ids, vec![0, 1, 2]);
    }

    #[test]
    fn no_clusters_returns_zero() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;

        let mock_gen = MockGenerator { embed_dim: dim };
        let count = global_integration(&conn, &mock_gen, dim).unwrap();
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

        let mock_gen = MockGenerator { embed_dim: dim };

        // Run twice
        global_integration(&conn, &mock_gen, dim).unwrap();
        global_integration(&conn, &mock_gen, dim).unwrap();

        let global = store.list_by_level(&ConsolidationLevel::Global).unwrap();
        assert_eq!(global.len(), 1); // Not 2
    }
}
