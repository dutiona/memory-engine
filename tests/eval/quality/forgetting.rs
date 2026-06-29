//! C3: Forgetting quality benchmarks.
//!
//! Tests Ebbinghaus decay ordering, half-life overrides, importance scoring
//! monotonicity, and pin immunity interaction with the forget pipeline.

use std::collections::HashMap;

use chrono::Utc;
use memory_engine::ForgetPolicy;
use memory_engine::traits::EmbeddingProvider;
use memory_engine::types::{AddFactOptions, AddFactRequest, EventType, FactType, NewEvent};

use crate::helpers::{TestEmbedder, add_fact_with_opts, days_ago, eval_engine};

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
            base_importance: Some(0.3),
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
            base_importance: Some(0.3),
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
            base_importance: Some(0.3),
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
            base_importance: Some(0.3),
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
            base_importance: Some(0.9),
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
            base_importance: Some(0.1),
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
            base_importance: Some(0.05),
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
            base_importance: Some(0.05),
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

/// #312: end-to-end exercise of the `graph_degree_weight` survival signal.
///
/// Two Episodic facts with EQUAL `base_importance`, EQUAL age, and EQUAL access
/// count differ only in graph connectivity: one is the hub of a co-session clique
/// (high degree), the other is isolated (degree 0). Under a threshold tuned between
/// their scores, the graph-degree term is the sole differentiator — the connected
/// fact must survive while the isolated one is pruned.
///
/// Every prior forgetting test runs with an empty graph (every fact's degree is 0,
/// so the 3rd of 4 scoring signals never moves the needle); this is the gap #312
/// flagged. The clique is built through the real engine API (`ingest` +
/// `link_session_facts`), so the in-memory graph the prune walk reads is populated
/// the same way production populates it.
#[tokio::test]
async fn well_connected_fact_survives_prune() {
    let engine = eval_engine();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(TestEmbedder);

    // One session whose facts will be wired into a co-session clique.
    let session = "sess-hub";
    let event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"k": "v"}),
        source: "test".to_string(),
        session_id: Some(session.to_string()),
        scope_id: 1,
        origin_node_id: "node-1".to_string(),
        sequence_id: 1,
        created_at: None,
    };
    let event_id = engine.ingest(&event).await.expect("ingest event");

    // Shared decay profile: ancient + never accessed, so recency/frequency are
    // identical for the hub and the isolated fact and cannot mask the degree term.
    let aged = || AddFactOptions {
        base_importance: Some(0.3),
        t_created: Some(days_ago(100)),
        last_accessed: Some(days_ago(100)),
        ..Default::default()
    };

    // Hub: Episodic (so it CAN decay), linked to the session event so
    // `link_session_facts` connects it.
    let hub_id = engine
        .add_fact(
            &AddFactRequest {
                content: "Hub fact connected to many others in its session".to_string(),
                fact_type: FactType::Episodic,
                source_event_id: Some(event_id),
                scope: None,
                opts: Some(aged()),
            },
            embedder.clone(),
            None,
        )
        .await
        .expect("add hub");

    // Two Semantic neighbours (decay-exempt → they survive, keeping the hub's
    // degree stable through the prune) sharing the same session as the hub.
    for i in 0..2 {
        engine
            .add_fact(
                &AddFactRequest {
                    content: format!("Neighbour {i} sharing the hub's session"),
                    fact_type: FactType::Semantic,
                    source_event_id: Some(event_id),
                    scope: None,
                    opts: Some(AddFactOptions {
                        base_importance: Some(0.9),
                        ..Default::default()
                    }),
                },
                embedder.clone(),
                None,
            )
            .await
            .expect("add neighbour");
    }

    // Isolated fact: same type/importance/age as the hub, but NOT linked to any
    // session — it never gains a co-session edge, so its degree stays 0.
    let isolated_id = add_fact_with_opts(
        &engine,
        "Isolated fact with no graph connections at all",
        FactType::Episodic,
        None,
        aged(),
    )
    .await;

    // Build the co-session clique. Hub + 2 neighbours → the hub gains 4 directed
    // edges (in+out to each neighbour); the isolated fact gains none.
    let created = engine
        .link_session_facts(session, None)
        .await
        .expect("link session facts");
    assert!(created > 0, "co-session edges must have been created");

    // Sanity: the degree signal is actually non-trivial for the hub and zero for
    // the isolated fact — otherwise the test would pass for the wrong reason.
    assert!(
        engine.graph_degree(hub_id) >= 2,
        "hub must be well-connected, got degree {}",
        engine.graph_degree(hub_id)
    );
    assert_eq!(
        engine.graph_degree(isolated_id),
        0,
        "isolated fact must have no edges"
    );

    // Computed scores (default weights recency=0.3, freq=0.2, graph=0.3, base=0.2):
    //   isolated (deg 0): 0.3·2^(-100/69) + 0 + 0          + 0.2·0.3 ≈ 0.170
    //   hub      (deg 4): 0.3·2^(-100/69) + 0 + 0.3·0.409  + 0.2·0.3 ≈ 0.293
    // A threshold strictly between them isolates the graph-degree term as the
    // decisive survival signal.
    let policy = ForgetPolicy {
        min_importance: 0.23,
        ..ForgetPolicy::default()
    };
    let _stats = engine.forget(&policy).await.expect("forget failed");

    let hub = engine.get_fact(hub_id).await.expect("get hub");
    let isolated = engine.get_fact(isolated_id).await.expect("get isolated");

    assert!(
        isolated.t_expired.is_some(),
        "the isolated fact (degree 0) must be pruned under the threshold"
    );
    assert!(
        hub.t_expired.is_none(),
        "the well-connected fact must survive — its graph-degree signal lifts it \
         above the threshold despite equal base importance, age, and access"
    );
}
