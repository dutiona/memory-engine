//! B3: Fact lifecycle conformance tests.
//!
//! Verifies content hashing, dedup consolidation, state transitions
//! (create → pin → forget, create → forget), and `get_fact` round-trip.

use memory_engine::traits::ConsolidationConfig;
use memory_engine::types::FactType;

use crate::helpers::{MockSummaryGenerator, add_fact, aggressive_forget_policy, eval_engine};

#[test]
fn same_content_produces_identical_content_hash() {
    let engine = eval_engine();

    let id1 = add_fact(&engine, "Rust is a systems language", FactType::Semantic);
    let id2 = add_fact(&engine, "Rust is a systems language", FactType::Semantic);

    let fact1 = engine.get_fact(id1).expect("get fact1");
    let fact2 = engine.get_fact(id2).expect("get fact2");

    assert_eq!(
        fact1.content_hash, fact2.content_hash,
        "identical content should produce identical content_hash"
    );
    assert_ne!(
        fact1.id, fact2.id,
        "different inserts should have different IDs"
    );
}

#[test]
fn different_content_produces_different_content_hash() {
    let engine = eval_engine();

    let id1 = add_fact(&engine, "Rust is a systems language", FactType::Semantic);
    let id2 = add_fact(
        &engine,
        "Python is a scripting language",
        FactType::Semantic,
    );

    let fact1 = engine.get_fact(id1).expect("get fact1");
    let fact2 = engine.get_fact(id2).expect("get fact2");

    assert_ne!(
        fact1.content_hash, fact2.content_hash,
        "different content should produce different content_hash"
    );
}

#[test]
fn dedup_consolidation_expires_one_duplicate() {
    let engine = eval_engine();
    let generator = MockSummaryGenerator;

    let id1 = add_fact(&engine, "The server runs on port 8080", FactType::Semantic);
    let id2 = add_fact(&engine, "The server runs on port 8080", FactType::Semantic);

    // Both should be active before consolidation.
    assert!(engine.get_fact(id1).unwrap().t_expired.is_none());
    assert!(engine.get_fact(id2).unwrap().t_expired.is_none());

    let config = ConsolidationConfig {
        dedup_threshold: 1.0,  // exact match only
        min_cluster_size: 100, // prevent cluster pass from running
    };

    let stats = engine
        .consolidate(&generator, &config)
        .expect("consolidate failed");
    assert!(
        stats.duplicates_removed > 0,
        "consolidation should remove at least one duplicate"
    );

    // Exactly one should survive, one should be expired.
    let f1 = engine.get_fact(id1).unwrap();
    let f2 = engine.get_fact(id2).unwrap();
    let expired_count = [&f1, &f2].iter().filter(|f| f.t_expired.is_some()).count();

    assert_eq!(
        expired_count, 1,
        "exactly one of the two duplicates should be expired"
    );
}

#[test]
fn pinned_fact_survives_aggressive_forget() {
    let engine = eval_engine();

    let id = add_fact(&engine, "critical system fact", FactType::Semantic);
    engine.pin_fact(id).expect("pin_fact failed");

    let policy = aggressive_forget_policy();
    engine.forget(&policy).expect("forget failed");

    let fact = engine.get_fact(id).expect("get_fact after forget");
    assert!(
        fact.t_expired.is_none(),
        "pinned fact should survive aggressive forget"
    );
    assert!(fact.is_pinned, "fact should still be marked as pinned");
}

#[test]
fn unpinned_fact_expired_by_aggressive_forget() {
    let engine = eval_engine();

    let id = add_fact(&engine, "ephemeral observation", FactType::Episodic);

    let policy = aggressive_forget_policy();
    engine.forget(&policy).expect("forget failed");

    let fact = engine.get_fact(id).expect("get_fact after forget");
    assert!(
        fact.t_expired.is_some(),
        "unpinned fact should be expired by aggressive forget"
    );
}

#[test]
fn get_fact_round_trip_correctness() {
    let engine = eval_engine();

    let content = "The deployment runs on Kubernetes v1.28";
    let id = add_fact(&engine, content, FactType::Procedural);

    let fact = engine.get_fact(id).expect("get_fact failed");

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
