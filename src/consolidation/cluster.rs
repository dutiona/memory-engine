use rusqlite::Connection;

use crate::error::Result;
use crate::search::vector::cosine_similarity;
use crate::store::facts::FactStore;
use crate::store::summaries::SummaryStore;
use crate::traits::{EmbeddingProvider, SummarizableContent, SummaryGenerator};
use crate::types::{ConsolidationLevel, Fact, NewSummary};

/// Cluster fusion pass.
///
/// Clears prior cluster-level summaries before creating new ones (idempotent).
/// Groups active facts by similarity (greedy single-linkage clustering) at
/// `cluster_threshold` (cosine; lower than the dedup threshold so clustering is
/// looser than dedup). For each cluster >= `min_cluster_size`, calls
/// `SummaryGenerator` to create a summary, then `EmbeddingProvider` to embed it
/// into the fact vector space. Stores summaries via `SummaryStore` with
/// `level=Cluster`.
///
/// Returns number of clusters created.
///
/// # Errors
///
/// Returns `MemoryError::Database` on SQL failure, or propagates errors from
/// the `SummaryGenerator` or `EmbeddingProvider`.
/// Returns `MemoryError::EmbeddingDimension` if the embedder returns an embedding
/// whose length does not match `embed_dim`.
/// Returns `MemoryError::Serialization` on JSON serialization failure.
pub fn cluster_fusion(
    conn: &Connection,
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    min_cluster_size: usize,
    cluster_threshold: f32,
) -> Result<usize> {
    // Safety cap lifted to the shared `super::MAX_FACTS_FOR_CLUSTERING` (#345) so
    // the dedup and cluster caps live in one place with their divergent skip
    // policies documented side by side. Checked BEFORE deleting existing summaries
    // to avoid wiping them without producing replacements.
    let fact_store = FactStore::new(conn, embed_dim);
    let active_facts = fact_store.list_active(None)?;

    if active_facts.len() > super::MAX_FACTS_FOR_CLUSTERING {
        tracing::warn!(
            count = active_facts.len(),
            max = super::MAX_FACTS_FOR_CLUSTERING,
            "clustering skipped: too many active facts for O(N^2) comparison"
        );
        return Ok(0);
    }

    let summary_store = SummaryStore::new(conn, embed_dim);

    // Idempotent: clear previous cluster summaries (safe — we know we'll rebuild them)
    summary_store.delete_by_level(&ConsolidationLevel::Cluster)?;

    // Greedy single-linkage clustering
    let clusters = greedy_cluster(&active_facts, cluster_threshold);

    let mut clusters_created = 0;
    for cluster in &clusters {
        if cluster.len() < min_cluster_size {
            continue;
        }

        let cluster_facts: Vec<Fact> = cluster
            .iter()
            .map(|&idx| active_facts[idx].clone())
            .collect();
        let source_ids: Vec<i64> = cluster_facts.iter().map(|f| f.id).collect();

        let items: Vec<SummarizableContent<'_>> = cluster_facts
            .iter()
            .map(|f| SummarizableContent {
                text: &f.content,
                embedding: &f.embedding,
            })
            .collect();
        let (summary_text, summary_embedding) =
            super::summarize_and_embed(generator, embedder, &items, embed_dim)?;

        // Determine scope_id from majority vote of source facts.
        // Deterministic tie-break: lowest scope_id wins on equal counts.
        let scope_id = {
            let mut scope_counts: std::collections::HashMap<i64, usize> =
                std::collections::HashMap::new();
            for fact in &cluster_facts {
                *scope_counts.entry(fact.scope_id).or_default() += 1;
            }
            scope_counts
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                .map_or(1, |(id, _)| id)
        };

        summary_store.insert(&NewSummary {
            content: summary_text,
            embedding: summary_embedding,
            level: ConsolidationLevel::Cluster,
            source_fact_ids: source_ids,
            scope_id,
            created_at: chrono::Utc::now(),
        })?;

        clusters_created += 1;
    }

    Ok(clusters_created)
}

/// Greedy single-linkage clustering.
///
/// For each unassigned fact, find all facts with cosine similarity > threshold.
/// Group them into a cluster.
fn greedy_cluster(facts: &[Fact], threshold: f32) -> Vec<Vec<usize>> {
    let n = facts.len();
    let mut assigned = vec![false; n];
    let mut clusters = Vec::new();

    for i in 0..n {
        if assigned[i] {
            continue;
        }

        let mut cluster = vec![i];
        assigned[i] = true;

        // Expand: find all unassigned facts similar to any fact in the cluster
        let mut j = 0;
        while j < cluster.len() {
            let anchor_idx = cluster[j];
            for k in 0..n {
                if assigned[k] {
                    continue;
                }
                let sim = cosine_similarity(&facts[anchor_idx].embedding, &facts[k].embedding);
                if sim > threshold {
                    cluster.push(k);
                    assigned[k] = true;
                }
            }
            j += 1;
        }

        clusters.push(cluster);
    }

    clusters
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{FactType, NewFact};

    /// Mock generator that concatenates fact contents.
    struct MockGenerator;

    impl SummaryGenerator for MockGenerator {
        fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String> {
            Ok(items.iter().map(|i| i.text).collect::<Vec<_>>().join(" + "))
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

    fn insert_fact(conn: &Connection, dim: usize, content: &str, embedding: Vec<f32>) -> i64 {
        let store = FactStore::new(conn, dim);
        store
            .insert(&NewFact {
                content: content.into(),
                content_hash: blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string(),
                embedding,
                fact_type: FactType::Semantic,
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
            })
            .unwrap()
    }

    #[test]
    fn cluster_formation() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;

        // Cluster 1: 3 similar facts (all near [1,0,0,0])
        insert_fact(&conn, dim, "c1a", vec![1.0, 0.0, 0.0, 0.0]);
        insert_fact(&conn, dim, "c1b", vec![0.98, 0.02, 0.0, 0.0]);
        insert_fact(&conn, dim, "c1c", vec![0.97, 0.03, 0.0, 0.0]);

        // Cluster 2: 3 similar facts (all near [0,1,0,0])
        insert_fact(&conn, dim, "c2a", vec![0.0, 1.0, 0.0, 0.0]);
        insert_fact(&conn, dim, "c2b", vec![0.02, 0.98, 0.0, 0.0]);
        insert_fact(&conn, dim, "c2c", vec![0.03, 0.97, 0.0, 0.0]);

        let mock_gen = MockGenerator;
        let mock_embed = MockEmbedder { embed_dim: dim };
        let clusters = cluster_fusion(&conn, &mock_gen, &mock_embed, dim, 3, 0.85).unwrap();
        assert_eq!(clusters, 2);

        let summaries = SummaryStore::new(&conn, dim)
            .list_by_level(&ConsolidationLevel::Cluster)
            .unwrap();
        assert_eq!(summaries.len(), 2);
    }

    #[test]
    fn min_cluster_size_respected() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;

        // Only 2 similar facts, min_cluster_size=3
        insert_fact(&conn, dim, "a", vec![1.0, 0.0, 0.0, 0.0]);
        insert_fact(&conn, dim, "b", vec![0.99, 0.01, 0.0, 0.0]);

        let mock_gen = MockGenerator;
        let mock_embed = MockEmbedder { embed_dim: dim };
        let clusters = cluster_fusion(&conn, &mock_gen, &mock_embed, dim, 3, 0.85).unwrap();
        assert_eq!(clusters, 0);
    }

    #[test]
    fn cluster_summaries_stored() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;

        insert_fact(&conn, dim, "alpha", vec![1.0, 0.0, 0.0, 0.0]);
        insert_fact(&conn, dim, "beta", vec![0.99, 0.01, 0.0, 0.0]);
        insert_fact(&conn, dim, "gamma", vec![0.98, 0.02, 0.0, 0.0]);

        let mock_gen = MockGenerator;
        let mock_embed = MockEmbedder { embed_dim: dim };
        cluster_fusion(&conn, &mock_gen, &mock_embed, dim, 2, 0.85).unwrap();

        let summaries = SummaryStore::new(&conn, dim)
            .list_by_level(&ConsolidationLevel::Cluster)
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].content.contains("alpha"));
        assert_eq!(summaries[0].source_fact_ids.len(), 3);
    }

    #[test]
    fn idempotent_rebuild() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;

        insert_fact(&conn, dim, "x", vec![1.0, 0.0, 0.0, 0.0]);
        insert_fact(&conn, dim, "y", vec![0.99, 0.01, 0.0, 0.0]);
        insert_fact(&conn, dim, "z", vec![0.98, 0.02, 0.0, 0.0]);

        let mock_gen = MockGenerator;
        let mock_embed = MockEmbedder { embed_dim: dim };

        // Run twice — should have exactly the same result
        cluster_fusion(&conn, &mock_gen, &mock_embed, dim, 2, 0.85).unwrap();
        cluster_fusion(&conn, &mock_gen, &mock_embed, dim, 2, 0.85).unwrap();

        let summaries = SummaryStore::new(&conn, dim)
            .list_by_level(&ConsolidationLevel::Cluster)
            .unwrap();
        assert_eq!(summaries.len(), 1); // not 2 — idempotent
    }

    /// #344: the injected `cluster_threshold` actually drives clustering. Two facts
    /// at cosine ≈ 0.92 cluster together under a loose threshold (0.85) but split
    /// into separate singletons under a strict one (0.95) — so a loose run yields a
    /// 2-fact cluster (1 summary) while a strict run yields none.
    #[test]
    fn cluster_threshold_is_honored() {
        let dim = 4;
        // a · b = 0.92 (unit vectors) → between 0.85 (loose) and 0.95 (strict).
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.92, 0.3923, 0.0, 0.0];

        // Loose threshold 0.85: the pair is similar enough → one cluster of 2.
        let loose = open_memory().unwrap();
        init_schema(&loose).unwrap();
        insert_fact(&loose, dim, "a", a.clone());
        insert_fact(&loose, dim, "b", b.clone());
        let made = cluster_fusion(
            &loose,
            &MockGenerator,
            &MockEmbedder { embed_dim: dim },
            dim,
            2,
            0.85,
        )
        .unwrap();
        assert_eq!(made, 1, "0.92-similar pair clusters under a 0.85 threshold");

        // Strict threshold 0.95: the same pair is NOT similar enough → no cluster.
        let strict = open_memory().unwrap();
        init_schema(&strict).unwrap();
        insert_fact(&strict, dim, "a", a);
        insert_fact(&strict, dim, "b", b);
        let made = cluster_fusion(
            &strict,
            &MockGenerator,
            &MockEmbedder { embed_dim: dim },
            dim,
            2,
            0.95,
        )
        .unwrap();
        assert_eq!(
            made, 0,
            "the same pair does NOT cluster under a 0.95 threshold"
        );
    }
}
