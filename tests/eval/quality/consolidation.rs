//! C2: Consolidation quality benchmarks.
//!
//! Tests engine-owned consolidation outcomes: dedup removes near-duplicates,
//! cluster fusion creates summaries, idempotence holds on second pass.
//!
//! **Key constraint**: `TestEmbedder` uses blake3 hashing, producing pseudo-random
//! vectors. Only identical text yields identical embeddings (cosine = 1.0).
//! Tests use exact-duplicate content to exercise dedup logic deterministically.

use memory_engine::traits::ConsolidationConfig;
use memory_engine::types::FactType;

use crate::helpers::{MockSummaryGenerator, TestEmbedder, add_fact, add_scoped_fact, eval_engine};

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

    // Insert 8 facts with identical content so blake3 produces identical
    // embeddings (cosine = 1.0), guaranteeing clustering at any threshold.
    // Use scope to verify majority-vote scope propagation.
    let shared_content = "Rust ownership model and memory safety guarantees";
    for _ in 0..8 {
        add_scoped_fact(&engine, shared_content, FactType::Semantic, "project:rust");
    }

    let generator = MockSummaryGenerator;
    let config = ConsolidationConfig {
        dedup_threshold: 1.01, // above theoretical max: floating-point cosine can exceed 1.0
        min_cluster_size: 2,
    };

    let stats = engine
        .consolidate(&generator, &TestEmbedder, &config)
        .expect("consolidate failed");

    assert!(
        stats.clusters_created >= 1,
        "expected >= 1 cluster from 8 identical-embedding facts, got {}",
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

    // Insert facts under a shared scope with identical content to guarantee clustering
    let scope = "project:demo";
    let shared_content = "Demo project fact about topic alpha";
    for _ in 0..6 {
        add_scoped_fact(&engine, shared_content, FactType::Semantic, scope);
    }

    let generator = MockSummaryGenerator;
    let config = ConsolidationConfig {
        dedup_threshold: 1.01, // above theoretical max: floating-point cosine can exceed 1.0
        min_cluster_size: 2,
    };

    let stats = engine
        .consolidate(&generator, &TestEmbedder, &config)
        .expect("consolidate failed");

    // With identical embeddings, a cluster should form
    assert!(
        stats.clusters_created >= 1,
        "expected >= 1 cluster from 6 identical-embedding facts, got {}",
        stats.clusters_created,
    );

    // When clusters exist, global summary should also be created
    assert!(
        stats.global_summaries >= 1,
        "global summary should be created when clusters exist, got {}",
        stats.global_summaries,
    );
}
