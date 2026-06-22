//! C3: Forgetting quality benchmarks.
//!
//! Tests Ebbinghaus decay ordering, half-life overrides, importance scoring
//! monotonicity, and pin immunity interaction with the forget pipeline.

use std::collections::HashMap;

use memory_engine::traits::ForgetPolicy;
use memory_engine::types::{AddFactOptions, FactType};

use crate::helpers::{add_fact_with_opts, days_ago, eval_engine};

#[tokio::test]
async fn ebbinghaus_decay_ordering_old_before_young() {
    let engine = eval_engine();

    // 120-day-old fact with 0 access (Episodic: the decaying type)
    let old_id = add_fact_with_opts(
        &engine,
        "Old fact that should decay first due to age",
        FactType::Episodic,
        None,
        AddFactOptions {
            importance: Some(0.3),
            t_created: Some(days_ago(120)),
            last_accessed: Some(days_ago(120)),
            ..Default::default()
        },
    )
    .await;

    // 0-day-old (fresh) fact with 0 access
    let young_id = add_fact_with_opts(
        &engine,
        "Young fact that should survive longer than old one",
        FactType::Episodic,
        None,
        AddFactOptions {
            importance: Some(0.3),
            ..Default::default()
        },
    )
    .await;

    // Computed importance scores (default weights: recency=0.3, freq=0.2, graph=0.3, base=0.2):
    // Old (120d): 0.3*2^(-120/69) + 0 + 0 + 0.2*0.3 = 0.089 + 0.06 = ~0.149
    // Young (0d): 0.3*1.0 + 0 + 0 + 0.2*0.3 = 0.3 + 0.06 = ~0.36
    // Threshold between them: 0.30
    let policy = ForgetPolicy {
        min_importance: 0.30,
        ..ForgetPolicy::default()
    };

    let _stats = engine.forget(&policy).await.expect("forget failed");

    // At least the old fact should be expired
    let old_fact = engine.get_fact(old_id).await.expect("get old fact failed");
    let young_fact = engine
        .get_fact(young_id)
        .await
        .expect("get young fact failed");

    // The old fact should expire before (or at the same time as) the young one.
    // With min_importance=0.50, the 120-day-old fact should fall below threshold.
    assert!(
        old_fact.t_expired.is_some(),
        "120-day-old fact should be expired under high min_importance policy"
    );
    assert!(
        young_fact.t_expired.is_none(),
        "fresh fact should survive (its recency score is high)"
    );
}

#[tokio::test]
async fn half_life_override_episodic_decays_faster() {
    let engine = eval_engine();

    let mut overrides = HashMap::new();
    overrides.insert(FactType::Episodic, 30.0); // 30-day half-life for episodic

    // Episodic fact, 45 days old
    let episodic_id = add_fact_with_opts(
        &engine,
        "Episodic fact from 45 days ago about a meeting",
        FactType::Episodic,
        None,
        AddFactOptions {
            importance: Some(0.3),
            t_created: Some(days_ago(45)),
            last_accessed: Some(days_ago(45)),
            ..Default::default()
        },
    )
    .await;

    // Semantic fact, also 45 days old (decay-exempt by default: recency
    // stays 1.0, so it survives regardless of age)
    let semantic_id = add_fact_with_opts(
        &engine,
        "Semantic fact from 45 days ago about architecture",
        FactType::Semantic,
        None,
        AddFactOptions {
            importance: Some(0.3),
            t_created: Some(days_ago(45)),
            last_accessed: Some(days_ago(45)),
            ..Default::default()
        },
    )
    .await;

    // Computed importance scores:
    // Episodic (45d, 30d HL): 0.3*2^(-45/30) + 0 + 0 + 0.2*0.3 = 0.106 + 0.06 = ~0.166
    // Semantic (decay-exempt): 0.3*1.0 + 0 + 0 + 0.2*0.3 = 0.3 + 0.06 = ~0.36
    // Threshold between them: 0.20
    let policy = ForgetPolicy {
        half_life_overrides: overrides,
        min_importance: 0.20,
        ..ForgetPolicy::default()
    };

    let _stats = engine.forget(&policy).await.expect("forget failed");

    let episodic = engine
        .get_fact(episodic_id)
        .await
        .expect("get episodic failed");
    let semantic = engine
        .get_fact(semantic_id)
        .await
        .expect("get semantic failed");

    // At 45 days with 30-day half-life, episodic should have decayed significantly
    // (past 1.5 half-lives). With default 69-day half-life, semantic should still
    // retain enough score.
    assert!(
        episodic.t_expired.is_some(),
        "episodic fact (45d old, 30d half-life) should be expired"
    );
    assert!(
        semantic.t_expired.is_none(),
        "semantic fact (45d old, 69d half-life) should survive"
    );
}

#[tokio::test]
async fn importance_scoring_monotonicity() {
    let engine = eval_engine();

    // High-importance fact, 60 days old (Episodic: the decaying type)
    let high_id = add_fact_with_opts(
        &engine,
        "High importance fact about critical architecture decision",
        FactType::Episodic,
        None,
        AddFactOptions {
            importance: Some(0.9),
            t_created: Some(days_ago(60)),
            last_accessed: Some(days_ago(60)),
            ..Default::default()
        },
    )
    .await;

    // Low-importance fact, 60 days old
    let low_id = add_fact_with_opts(
        &engine,
        "Low importance fact about minor configuration change",
        FactType::Episodic,
        None,
        AddFactOptions {
            importance: Some(0.1),
            t_created: Some(days_ago(60)),
            last_accessed: Some(days_ago(60)),
            ..Default::default()
        },
    )
    .await;

    // Computed importance scores (60d old, 0 access, 0 graph):
    // High (imp=0.9): 0.3*2^(-60/69) + 0 + 0 + 0.2*0.9 = 0.164 + 0.18 = ~0.344
    // Low (imp=0.1):  0.3*2^(-60/69) + 0 + 0 + 0.2*0.1 = 0.164 + 0.02 = ~0.184
    // Threshold between them: 0.25
    let policy = ForgetPolicy {
        min_importance: 0.25,
        ..ForgetPolicy::default()
    };

    let _stats = engine.forget(&policy).await.expect("forget failed");

    let high = engine.get_fact(high_id).await.expect("get high failed");
    let low = engine.get_fact(low_id).await.expect("get low failed");

    // Same age, same access count — importance should be the differentiator.
    // Low-importance fact should expire first.
    assert!(
        low.t_expired.is_some(),
        "low-importance fact (0.1) should be expired at 60 days"
    );
    assert!(
        high.t_expired.is_none(),
        "high-importance fact (0.9) should survive at 60 days"
    );
}

#[tokio::test]
async fn pin_immunity_survives_aggressive_forget() {
    let engine = eval_engine();

    // Pinned fact, very old, low importance — should still survive
    // (Episodic so that decay, not type exemption, is the survival test)
    let pinned_id = add_fact_with_opts(
        &engine,
        "Pinned fact that must never be forgotten despite age and low importance",
        FactType::Episodic,
        None,
        AddFactOptions {
            importance: Some(0.05),
            pinned: Some(true),
            t_created: Some(days_ago(365)),
            last_accessed: Some(days_ago(365)),
            ..Default::default()
        },
    )
    .await;

    // Unpinned fact with same profile — should be expired
    let unpinned_id = add_fact_with_opts(
        &engine,
        "Unpinned fact with same low importance and old age should be forgotten",
        FactType::Episodic,
        None,
        AddFactOptions {
            importance: Some(0.05),
            t_created: Some(days_ago(365)),
            last_accessed: Some(days_ago(365)),
            ..Default::default()
        },
    )
    .await;

    // Aggressive policy: high min_importance threshold
    let policy = ForgetPolicy {
        min_importance: 0.99,
        ..ForgetPolicy::default()
    };

    let _stats = engine.forget(&policy).await.expect("forget failed");

    let pinned = engine.get_fact(pinned_id).await.expect("get pinned failed");
    let unpinned = engine
        .get_fact(unpinned_id)
        .await
        .expect("get unpinned failed");

    assert!(
        pinned.t_expired.is_none(),
        "pinned fact should be immune to forgetting regardless of age/importance"
    );
    assert!(
        unpinned.t_expired.is_some(),
        "unpinned fact with low importance should be expired under aggressive policy"
    );
}
