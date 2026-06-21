//! B3: Fact lifecycle conformance tests.
//!
//! Verifies content hashing, dedup consolidation, state transitions
//! (create → pin → forget, create → forget), and `get_fact` round-trip.

use memory_engine::traits::{ConsolidationConfig, EmbeddingProvider, SummaryGenerator};
use memory_engine::types::FactType;

use crate::helpers::{
    MockSummaryGenerator, TestEmbedder, add_fact, aggressive_forget_policy, eval_engine,
};

#[tokio::test]
async fn same_content_produces_identical_content_hash() {
    let engine = eval_engine();

    let id1 = add_fact(&engine, "Rust is a systems language", FactType::Semantic).await;
    let id2 = add_fact(&engine, "Rust is a systems language", FactType::Semantic).await;

    let fact1 = engine.get_fact(id1).await.expect("get fact1");
    let fact2 = engine.get_fact(id2).await.expect("get fact2");

    assert_eq!(
        fact1.content_hash, fact2.content_hash,
        "identical content should produce identical content_hash"
    );
    assert_ne!(
        fact1.id, fact2.id,
        "different inserts should have different IDs"
    );
}

#[tokio::test]
async fn different_content_produces_different_content_hash() {
    let engine = eval_engine();

    let id1 = add_fact(&engine, "Rust is a systems language", FactType::Semantic).await;
    let id2 = add_fact(
        &engine,
        "Python is a scripting language",
        FactType::Semantic,
    )
    .await;

    let fact1 = engine.get_fact(id1).await.expect("get fact1");
    let fact2 = engine.get_fact(id2).await.expect("get fact2");

    assert_ne!(
        fact1.content_hash, fact2.content_hash,
        "different content should produce different content_hash"
    );
}

#[tokio::test]
async fn dedup_consolidation_expires_one_duplicate() {
    let engine = eval_engine();
    let generator = MockSummaryGenerator;

    let id1 = add_fact(&engine, "The server runs on port 8080", FactType::Semantic).await;
    let id2 = add_fact(&engine, "The server runs on port 8080", FactType::Semantic).await;

    // Both should be active before consolidation.
    assert!(engine.get_fact(id1).await.unwrap().t_expired.is_none());
    assert!(engine.get_fact(id2).await.unwrap().t_expired.is_none());

    let config = ConsolidationConfig::builder()
        .dedup_threshold(1.0) // exact match only
        .min_cluster_size(100) // prevent cluster pass from running
        .build();

    let stats = engine
        .consolidate(
            std::sync::Arc::new(generator) as std::sync::Arc<dyn SummaryGenerator>,
            std::sync::Arc::new(TestEmbedder) as std::sync::Arc<dyn EmbeddingProvider>,
            &config,
        )
        .await
        .expect("consolidate failed");
    assert!(
        stats.duplicates_removed > 0,
        "consolidation should remove at least one duplicate"
    );

    // Exactly one should survive, one should be expired.
    let f1 = engine.get_fact(id1).await.unwrap();
    let f2 = engine.get_fact(id2).await.unwrap();
    let expired_count = [&f1, &f2].iter().filter(|f| f.t_expired.is_some()).count();

    assert_eq!(
        expired_count, 1,
        "exactly one of the two duplicates should be expired"
    );
}

#[tokio::test]
async fn pinned_fact_survives_aggressive_forget() {
    let engine = eval_engine();

    let id = add_fact(&engine, "critical system fact", FactType::Semantic).await;
    engine.pin_fact(id).await.expect("pin_fact failed");

    let policy = aggressive_forget_policy();
    engine.forget(&policy).await.expect("forget failed");

    let fact = engine.get_fact(id).await.expect("get_fact after forget");
    assert!(
        fact.t_expired.is_none(),
        "pinned fact should survive aggressive forget"
    );
    assert!(fact.is_pinned, "fact should still be marked as pinned");
}

#[tokio::test]
async fn unpinned_fact_expired_by_aggressive_forget() {
    let engine = eval_engine();

    let id = add_fact(&engine, "ephemeral observation", FactType::Episodic).await;

    let policy = aggressive_forget_policy();
    engine.forget(&policy).await.expect("forget failed");

    let fact = engine.get_fact(id).await.expect("get_fact after forget");
    assert!(
        fact.t_expired.is_some(),
        "unpinned fact should be expired by aggressive forget"
    );
}

#[tokio::test]
async fn get_fact_round_trip_correctness() {
    let engine = eval_engine();

    let content = "The deployment runs on Kubernetes v1.28";
    let id = add_fact(&engine, content, FactType::Procedural).await;

    let fact = engine.get_fact(id).await.expect("get_fact failed");

    assert_eq!(fact.id, id);
    assert_eq!(fact.content, content);
    assert_eq!(fact.fact_type, FactType::Procedural);
    assert!(fact.t_expired.is_none(), "new fact should not be expired");
    assert!(!fact.content_hash.is_empty(), "content_hash should be set");
    assert_eq!(
        fact.embedding.len(),
        crate::helpers::DIM,
        "embedding dimension should match"
    );
}
