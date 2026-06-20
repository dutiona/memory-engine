use rusqlite::Connection;

use crate::error::Result;
use crate::search::vector::cosine_similarity;
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
    facts: &[&Fact],
    generator: &dyn SummaryGenerator,
    embedder: &dyn EmbeddingProvider,
    embed_dim: usize,
    min_cluster_size: usize,
    cluster_threshold: f32,
) -> Result<usize> {
    // `facts` is the post-dedup active set, loaded once by the caller and shared
    // with the dedup pass (#389). Safety cap (`super::MAX_FACTS_FOR_CLUSTERING`,
    // #345) checked BEFORE deleting existing summaries so we never wipe them
    // without producing replacements.
    if facts.len() > super::MAX_FACTS_FOR_CLUSTERING {
        tracing::warn!(
            count = facts.len(),
            max = super::MAX_FACTS_FOR_CLUSTERING,
            "clustering skipped: too many active facts for O(N^2) comparison"
        );
        return Ok(0);
    }

    let summary_store = SummaryStore::new(conn, embed_dim);

    // Idempotent: clear previous cluster summaries (safe — we know we'll rebuild them)
    summary_store.delete_by_level(&ConsolidationLevel::Cluster)?;

    // Greedy single-linkage clustering
    let clusters = greedy_cluster(facts, cluster_threshold);

    let mut clusters_created = 0;
    for cluster in &clusters {
        if cluster.len() < min_cluster_size {
            continue;
        }

        // #679: index directly into the borrowed `facts` — no per-member `Fact`
        // clone (the cluster pass previously cloned every member into a `Vec<Fact>`).
        let source_ids: Vec<i64> = cluster.iter().map(|&idx| facts[idx].id).collect();

        let items: Vec<SummarizableContent<'_>> = cluster
            .iter()
            .map(|&idx| SummarizableContent::new(&facts[idx].content, &facts[idx].embedding))
            .collect();
        let (summary_text, summary_embedding) =
            super::summarize_and_embed(generator, embedder, &items, embed_dim)?;

        // Determine scope_id from majority vote of source facts.
        // Deterministic tie-break: lowest scope_id wins on equal counts.
        let scope_id = {
            let mut scope_counts: std::collections::HashMap<i64, usize> =
                std::collections::HashMap::new();
            for &idx in cluster {
                *scope_counts.entry(facts[idx].scope_id).or_default() += 1;
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
fn greedy_cluster(facts: &[&Fact], threshold: f32) -> Vec<Vec<usize>> {
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

    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{FactType, NewFact};

    /// Load the active facts and run `cluster_fusion` over borrowed references —
    /// the orchestrator now owns the single load (#389), so tests mirror that here.
    fn cluster(
        conn: &Connection,
        generator: &dyn SummaryGenerator,
        embedder: &dyn EmbeddingProvider,
        dim: usize,
        min_cluster_size: usize,
        cluster_threshold: f32,
    ) -> Result<usize> {
        let active = FactStore::new(conn, dim).list_active(None)?;
        let refs: Vec<&Fact> = active.iter().collect();
        cluster_fusion(
            conn,
            &refs,
            generator,
            embedder,
            dim,
            min_cluster_size,
            cluster_threshold,
        )
    }

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
        let clusters = cluster(&conn, &mock_gen, &mock_embed, dim, 3, 0.85).unwrap();
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
        let clusters = cluster(&conn, &mock_gen, &mock_embed, dim, 3, 0.85).unwrap();
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
        cluster(&conn, &mock_gen, &mock_embed, dim, 2, 0.85).unwrap();

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
        cluster(&conn, &mock_gen, &mock_embed, dim, 2, 0.85).unwrap();
        cluster(&conn, &mock_gen, &mock_embed, dim, 2, 0.85).unwrap();

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
        let made = cluster(
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
        let made = cluster(
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

    // --- #440: direct unit tests for the private `greedy_cluster` ---

    /// Build an in-memory `Fact` with a given id/scope/embedding. `greedy_cluster`
    /// only reads `embedding`; the cluster pass also reads `id`/`scope_id`.
    fn mk_fact(id: i64, scope_id: i64, embedding: Vec<f32>) -> Fact {
        Fact {
            id,
            content: format!("f{id}"),
            content_hash: String::new(),
            embedding,
            fact_type: FactType::Semantic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
            importance_score: Fact::UNSCORED_IMPORTANCE,
            surfaced_at: None,
        }
    }

    #[test]
    fn greedy_cluster_empty_input() {
        assert!(greedy_cluster(&[], 0.85).is_empty());
    }

    #[test]
    fn greedy_cluster_single_element() {
        let a = mk_fact(1, 1, vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(greedy_cluster(&[&a], 0.85), vec![vec![0]]);
    }

    #[test]
    fn greedy_cluster_threshold_zero_groups_all_positively_correlated() {
        // All vectors share the +x direction → every pairwise cosine > 0, so a
        // threshold of 0.0 collapses them into a single cluster.
        let a = mk_fact(1, 1, vec![1.0, 0.0, 0.0, 0.0]);
        let b = mk_fact(2, 1, vec![0.9, 0.1, 0.0, 0.0]);
        let c = mk_fact(3, 1, vec![0.8, 0.2, 0.0, 0.0]);
        let clusters = greedy_cluster(&[&a, &b, &c], 0.0);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 3);
    }

    #[test]
    fn greedy_cluster_threshold_one_yields_singletons() {
        // The predicate is strict `sim > threshold`; cosine maxes at 1.0, so even
        // identical embeddings (cos == 1.0) are NOT > 1.0 → every fact is its own
        // singleton.
        let a = mk_fact(1, 1, vec![1.0, 0.0, 0.0, 0.0]);
        let b = mk_fact(2, 1, vec![1.0, 0.0, 0.0, 0.0]); // identical to a
        let c = mk_fact(3, 1, vec![0.0, 1.0, 0.0, 0.0]);
        let clusters = greedy_cluster(&[&a, &b, &c], 1.0);
        assert_eq!(clusters.len(), 3, "threshold 1.0 → all singletons");
        assert!(clusters.iter().all(|c| c.len() == 1));
    }

    #[test]
    fn greedy_cluster_transitive_single_linkage_chain() {
        // A~B and B~C but A≁C: single-linkage transitively merges all three via B.
        // Near-unit vectors at 0°, 30°, 60° in the xy-plane (cosine normalizes):
        //   cos(A,B) = cos(B,C) = cos30 ≈ 0.866; cos(A,C) = cos60 = 0.5.
        // At threshold 0.8: A~B, B~C pass; A~C fails — yet C joins A's cluster via B.
        let a = mk_fact(1, 1, vec![1.0, 0.0, 0.0, 0.0]);
        let b = mk_fact(2, 1, vec![0.866, 0.5, 0.0, 0.0]);
        let c = mk_fact(3, 1, vec![0.5, 0.866, 0.0, 0.0]);
        let clusters = greedy_cluster(&[&a, &b, &c], 0.8);
        assert_eq!(
            clusters.len(),
            1,
            "single-linkage chains A-B-C into one cluster"
        );
        assert_eq!(clusters[0].len(), 3);
    }

    // --- #441: majority-vote scope_id selection (with deterministic tie-break) ---

    fn insert_fact_scoped(
        conn: &Connection,
        dim: usize,
        content: &str,
        embedding: Vec<f32>,
        scope_id: i64,
    ) -> i64 {
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
                scope_id,
                importance: 0.5,
                access_count: 0,
                last_accessed: Utc::now(),
                metadata: serde_json::json!({}),
                is_pinned: false,
            })
            .unwrap()
    }

    /// Create a scope with an explicit id under the root (1) so facts may reference
    /// it — `scope_id` is a FK into `scopes`.
    fn create_scope(conn: &Connection, id: i64, label: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (?1, 1, ?2, 1)",
            rusqlite::params![id, label],
        )
        .unwrap();
    }

    /// Near-identical embedding (all facts cluster) with a per-fact nudge so they
    /// stay distinct rows; cosine stays well above any test threshold.
    fn near(i: usize) -> Vec<f32> {
        // u16→f32 is a lossless `From` (avoids the usize→f32 precision-loss lint).
        let e = f32::from(u16::try_from(i).unwrap()) * 0.001;
        vec![1.0, e, 0.0, 0.0]
    }

    fn single_cluster_scope(conn: &Connection, dim: usize) -> i64 {
        cluster(
            conn,
            &MockGenerator,
            &MockEmbedder { embed_dim: dim },
            dim,
            2,
            0.85,
        )
        .unwrap();
        let summaries = SummaryStore::new(conn, dim)
            .list_by_level(&ConsolidationLevel::Cluster)
            .unwrap();
        assert_eq!(summaries.len(), 1, "fixture must form exactly one cluster");
        summaries[0].scope_id
    }

    #[test]
    fn scope_id_is_the_majority_vote() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;
        create_scope(&conn, 10, "ten");
        create_scope(&conn, 20, "twenty");
        // 3 facts in scope 10, 2 in scope 20 → majority scope 10 wins.
        for i in 0..3 {
            insert_fact_scoped(&conn, dim, &format!("a{i}"), near(i), 10);
        }
        for i in 3..5 {
            insert_fact_scoped(&conn, dim, &format!("b{i}"), near(i), 20);
        }
        assert_eq!(single_cluster_scope(&conn, dim), 10);
    }

    #[test]
    fn scope_id_tie_break_picks_lowest() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = 4;
        create_scope(&conn, 10, "ten");
        create_scope(&conn, 5, "five");
        // 2 facts in scope 10, 2 in scope 5 → tie → lowest scope_id (5) wins.
        for i in 0..2 {
            insert_fact_scoped(&conn, dim, &format!("a{i}"), near(i), 10);
        }
        for i in 2..4 {
            insert_fact_scoped(&conn, dim, &format!("b{i}"), near(i), 5);
        }
        assert_eq!(single_cluster_scope(&conn, dim), 5);
    }

    // --- #442: property-based invariants for `greedy_cluster` ---

    use proptest::prelude::*;

    proptest! {
        /// `greedy_cluster` invariants, for any embeddings and any threshold:
        /// 1. **Partition** — every input index 0..n lands in exactly one cluster
        ///    (no index dropped or duplicated).
        /// 2. **Within-cluster connectivity** — in any cluster of size ≥ 2, every
        ///    member is cosine-similar (> threshold) to at least one other member,
        ///    which single-linkage guarantees (each non-seed joined via a neighbor,
        ///    and the seed connects to whoever joined first). This catches a
        ///    member landing in the *wrong* group, which the partition check alone
        ///    cannot.
        ///
        /// A zero embedding yields cosine 0.0 (the implementation guards the
        /// zero-norm denominator — it is NOT NaN); `0.0 > t` is false for all
        /// t in [0,1], so zero vectors stay singletons — still a valid partition.
        #[test]
        fn greedy_cluster_invariants(
            embs in proptest::collection::vec(
                proptest::collection::vec(-1.0f32..=1.0, 4),
                1..=12,
            ),
            threshold in 0.0f32..=1.0,
        ) {
            let facts: Vec<Fact> = embs
                .iter()
                .enumerate()
                .map(|(i, e)| mk_fact(i64::try_from(i).unwrap(), 1, e.clone()))
                .collect();
            let refs: Vec<&Fact> = facts.iter().collect();
            let clusters = greedy_cluster(&refs, threshold);

            // (1) partition
            let mut seen: Vec<usize> = clusters.iter().flatten().copied().collect();
            seen.sort_unstable();
            prop_assert_eq!(seen, (0..facts.len()).collect::<Vec<usize>>());

            // (2) within-cluster connectivity (single-linkage)
            for c in &clusters {
                if c.len() < 2 {
                    continue;
                }
                for &m in c {
                    let connected = c.iter().any(|&o| {
                        o != m
                            && cosine_similarity(&facts[m].embedding, &facts[o].embedding) > threshold
                    });
                    prop_assert!(connected, "member {m} has no in-cluster neighbor > threshold");
                }
            }
        }
    }

    /// Deterministic companions to the connectivity proptest: a zero vector (cosine
    /// 0.0 via the denom guard, never NaN) stays a singleton even at threshold 0.0,
    /// and two anti-parallel vectors (cosine −1) never group at threshold 0.0.
    #[test]
    fn greedy_cluster_zero_and_anticorrelated_stay_singletons() {
        let zero = mk_fact(1, 1, vec![0.0, 0.0, 0.0, 0.0]);
        let pos = mk_fact(2, 1, vec![1.0, 0.0, 0.0, 0.0]);
        let neg = mk_fact(3, 1, vec![-1.0, 0.0, 0.0, 0.0]);
        let clusters = greedy_cluster(&[&zero, &pos, &neg], 0.0);
        // pos·neg = -1 (< 0), pos·zero = neg·zero = 0.0 — none exceed 0.0.
        assert_eq!(
            clusters.len(),
            3,
            "no pair exceeds threshold 0.0 → three singletons"
        );
        assert!(clusters.iter().all(|c| c.len() == 1));
    }
}
