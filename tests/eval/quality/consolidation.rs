//! C2: Consolidation quality benchmarks.
//!
//! Tests engine-owned consolidation outcomes: dedup removes near-duplicates,
//! cluster fusion creates summaries, idempotence holds on second pass.
//!
//! **Embedding strategy**: dedup tests use `TestEmbedder` (blake3 hashing) with
//! exact-duplicate content — identical text yields identical embeddings
//! (cosine = 1.0), exercising dedup deterministically. Cluster tests use
//! [`ClusterableEmbedder`] to produce *distinct-but-related* facts (pairwise
//! cosine ≈ 0.88): clustering groups facts that are similar but not identical,
//! since identical facts are (correctly) collapsed by the dedup pass first.

use memory_engine::EmbeddingFingerprint;
use memory_engine::error::Result;
use memory_engine::traits::{ConsolidationConfig, EmbeddingProvider};
use memory_engine::types::{AddFactRequest, FactType};

use crate::helpers::{DIM, MockSummaryGenerator, TestEmbedder, add_fact, eval_engine};

/// Embedder producing distinct-but-related vectors for the cluster tests.
///
/// Every fact shares a dominant component (position 0) plus a unique orthogonal
/// perturbation keyed by the trailing `#<n>` in its content. Two distinct facts
/// then have cosine `1 / (1 + 0.369²) ≈ 0.88` — above the 0.85 cluster threshold
/// (so they group into one cluster) but below a 0.95 dedup threshold (so they are
/// not treated as duplicates). This lets cluster tests use realistic distinct
/// facts; identical facts would be deduplicated before reaching the cluster pass.
struct ClusterableEmbedder;

impl EmbeddingProvider for ClusterableEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let idx: usize = text
            .rsplit('#')
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let mut v = vec![0.0_f32; DIM];
        v[0] = 1.0;
        v[1 + (idx % (DIM - 1))] = 0.369;
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut v {
            *x /= norm;
        }
        Ok(v)
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("clusterable", "test", DIM)
    }
}

/// Insert `n` distinct-but-clusterable facts under `scope`, embedded by
/// [`ClusterableEmbedder`]. Each gets a unique `#<i>` suffix so their embeddings
/// differ but stay ~0.88 similar.
fn insert_clusterable(engine: &memory_engine::engine::MemoryEngine, n: usize, scope: &str) {
    for i in 0..n {
        engine
            .add_fact(
                &AddFactRequest {
                    content: format!("clusterable fact #{i}"),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: Some(scope.to_string()),
                    opts: None,
                },
                &ClusterableEmbedder,
                None,
            )
            .expect("add_fact failed");
    }
}

/// Insert 5 pairs of exact-duplicate facts. Identical text produces identical
/// blake3 embeddings, giving cosine similarity = 1.0.
fn insert_exact_duplicate_pairs(engine: &memory_engine::engine::MemoryEngine) -> Vec<i64> {
    let contents = [
        "Rust ownership model prevents data races at compile time through the borrow checker",
        "Embedding models map text to dense vector representations for similarity search",
        "SQLite WAL mode provides concurrent read access without blocking writers",
        "Event sourcing stores all mutations as append-only events for audit trail",
        "The consolidation pipeline runs dedup then cluster then global summary in three passes",
    ];

    let mut ids = Vec::new();
    for content in &contents {
        ids.push(add_fact(engine, content, FactType::Semantic));
        ids.push(add_fact(engine, content, FactType::Semantic));
    }
    ids
}

#[test]
fn dedup_removes_exact_duplicates() {
    let engine = eval_engine();
    let ids = insert_exact_duplicate_pairs(&engine);
    assert_eq!(ids.len(), 10, "5 pairs = 10 facts inserted");

    let stats_before = engine.statistics().expect("statistics failed");
    assert_eq!(stats_before.facts.active, 10);

    let generator = MockSummaryGenerator;
    let config = ConsolidationConfig {
        dedup_threshold: 0.92, // standard threshold; exact duplicates have cosine=1.0
        min_cluster_size: 100, // disable clustering for this test
    };

    let stats = engine
        .consolidate(&generator, &TestEmbedder, &config)
        .expect("consolidate failed");

    assert!(
        stats.duplicates_removed >= 5,
        "expected >= 5 duplicates removed, got {}",
        stats.duplicates_removed,
    );

    // Verify active count decreased
    let stats_after = engine.statistics().expect("statistics failed");
    assert!(
        stats_after.facts.active <= stats_before.facts.active - 5,
        "active facts should decrease by at least 5: before={}, after={}",
        stats_before.facts.active,
        stats_after.facts.active,
    );

    // Verify expired facts have t_expired set (soft deletion)
    assert!(
        stats_after.facts.expired >= 5,
        "expected >= 5 expired facts, got {}",
        stats_after.facts.expired,
    );
}

#[test]
fn cluster_fusion_creates_clusters() {
    let engine = eval_engine();

    // Distinct-but-related facts (pairwise cosine ≈ 0.88, above the 0.85 cluster
    // threshold) form a single cluster. A 0.95 dedup_threshold leaves them intact
    // — they are similar but not duplicates. Identical facts would instead be
    // collapsed by the dedup pass; clustering operates on distinct survivors.
    insert_clusterable(&engine, 4, "project:rust");

    let generator = MockSummaryGenerator;
    let config = ConsolidationConfig {
        dedup_threshold: 0.95,
        min_cluster_size: 2,
    };

    let stats = engine
        .consolidate(&generator, &ClusterableEmbedder, &config)
        .expect("consolidate failed");

    assert_eq!(
        stats.duplicates_removed, 0,
        "distinct facts (cosine ≈ 0.88 < 0.95) must not be deduplicated, got {}",
        stats.duplicates_removed,
    );
    assert!(
        stats.clusters_created >= 1,
        "expected >= 1 cluster from 4 related facts, got {}",
        stats.clusters_created,
    );
}

#[test]
fn consolidation_is_idempotent() {
    let engine = eval_engine();
    let _ = insert_exact_duplicate_pairs(&engine);

    let generator = MockSummaryGenerator;
    let config = ConsolidationConfig {
        dedup_threshold: 0.92,
        min_cluster_size: 2,
    };

    // First pass
    let stats1 = engine
        .consolidate(&generator, &TestEmbedder, &config)
        .expect("first consolidate failed");
    assert!(
        stats1.duplicates_removed > 0,
        "first pass should remove duplicates"
    );

    // Second pass: no additional dedup expected
    let stats2 = engine
        .consolidate(&generator, &TestEmbedder, &config)
        .expect("second consolidate failed");
    assert_eq!(
        stats2.duplicates_removed, 0,
        "second pass should find no new duplicates, got {}",
        stats2.duplicates_removed,
    );
}

#[test]
fn cluster_and_global_summary_scoping() {
    let engine = eval_engine();

    // Distinct-but-related facts under a shared scope cluster (cosine ≈ 0.88 >
    // 0.85) without being deduplicated (< 0.95); a cluster then yields a global
    // summary.
    let scope = "project:demo";
    insert_clusterable(&engine, 4, scope);

    let generator = MockSummaryGenerator;
    let config = ConsolidationConfig {
        dedup_threshold: 0.95,
        min_cluster_size: 2,
    };

    let stats = engine
        .consolidate(&generator, &ClusterableEmbedder, &config)
        .expect("consolidate failed");

    assert!(
        stats.clusters_created >= 1,
        "expected >= 1 cluster from 4 related facts, got {}",
        stats.clusters_created,
    );

    // When clusters exist, global summary should also be created
    assert!(
        stats.global_summaries >= 1,
        "global summary should be created when clusters exist, got {}",
        stats.global_summaries,
    );
}
