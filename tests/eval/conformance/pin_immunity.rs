//! B4: Pin immunity conformance tests.
//!
//! Verifies that pinned facts survive aggressive forgetting, that
//! `PersistenceClassifier` auto-pins by type, and that explicit
//! `pinned=false` overrides the classifier.

use memory_engine::traits::{EmbeddingProvider, PersistenceClassifier};
use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};

use crate::helpers::{PinByType, TestEmbedder, add_fact, aggressive_forget_policy, eval_engine};

#[tokio::test]
async fn all_pinned_survive_aggressive_forget() {
    let engine = eval_engine();

    // 3 pinned facts.
    let mut pinned_ids = Vec::new();
    for i in 0..3 {
        let id = add_fact(
            &engine,
            &format!("critical pinned fact number {i}"),
            FactType::Semantic,
        )
        .await;
        engine.pin_fact(id).await.expect("pin_fact failed");
        pinned_ids.push(id);
    }

    // 7 unpinned facts.
    let mut unpinned_ids = Vec::new();
    for i in 0..7 {
        let id = add_fact(
            &engine,
            &format!("disposable unpinned fact number {i}"),
            FactType::Episodic,
        )
        .await;
        unpinned_ids.push(id);
    }

    let policy = aggressive_forget_policy();
    let stats = engine.forget(&policy).await.expect("forget failed");

    // All pinned facts should survive.
    for &pid in &pinned_ids {
        let fact = engine.get_fact(pid).await.expect("get pinned fact");
        assert!(
            fact.t_expired.is_none(),
            "pinned fact {pid} should survive aggressive forget"
        );
    }

    // All unpinned facts should be expired.
    for &uid in &unpinned_ids {
        let fact = engine.get_fact(uid).await.expect("get unpinned fact");
        assert!(
            fact.t_expired.is_some(),
            "unpinned fact {uid} should be expired by aggressive forget"
        );
    }

    assert_eq!(
        stats.facts_expired,
        unpinned_ids.len(),
        "prune stats should reflect the number of expired facts"
    );
}

#[tokio::test]
async fn persistence_classifier_auto_pins_by_type() {
    let engine = eval_engine();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(TestEmbedder);
    let classifier: std::sync::Arc<dyn PersistenceClassifier> = std::sync::Arc::new(PinByType {
        pinned_type: FactType::Procedural,
    });

    // Add a Procedural fact — classifier should auto-pin it.
    let proc_id = engine
        .add_fact(
            &AddFactRequest {
                content: "step-by-step deployment procedure".to_string(),
                fact_type: FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            Some(classifier.clone()),
        )
        .await
        .expect("add procedural fact");

    // Add an Episodic fact — classifier should NOT pin it, and as a
    // decaying type it stays forgettable (Semantic would be decay-exempt).
    let epi_id = engine
        .add_fact(
            &AddFactRequest {
                content: "routine log entry about servers".to_string(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            Some(classifier.clone()),
        )
        .await
        .expect("add episodic fact");

    let proc_fact = engine.get_fact(proc_id).await.expect("get procedural fact");
    let epi_fact = engine.get_fact(epi_id).await.expect("get episodic fact");

    assert!(
        proc_fact.is_pinned,
        "Procedural fact should be auto-pinned by classifier"
    );
    assert!(
        !epi_fact.is_pinned,
        "Episodic fact should NOT be auto-pinned"
    );

    // Verify the pinned fact survives forget.
    let policy = aggressive_forget_policy();
    engine.forget(&policy).await.expect("forget failed");

    let proc_after = engine
        .get_fact(proc_id)
        .await
        .expect("get procedural after forget");
    let epi_after = engine
        .get_fact(epi_id)
        .await
        .expect("get episodic after forget");

    assert!(
        proc_after.t_expired.is_none(),
        "auto-pinned Procedural fact should survive forget"
    );
    assert!(
        epi_after.t_expired.is_some(),
        "unpinned Episodic fact should be expired"
    );
}

#[tokio::test]
async fn explicit_pinned_false_overrides_classifier() {
    let engine = eval_engine();
    let embedder = TestEmbedder;
    let classifier = PinByType {
        pinned_type: FactType::Procedural,
    };

    // Explicitly set pinned=false on a Procedural fact — should override classifier.
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "procedure that should not be pinned".to_string(),
                fact_type: FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    pinned: Some(false),
                    ..Default::default()
                }),
            },
            std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
            Some(std::sync::Arc::new(classifier) as std::sync::Arc<dyn PersistenceClassifier>),
        )
        .await
        .expect("add fact with pinned=false");

    let fact = engine.get_fact(id).await.expect("get fact");
    assert!(
        !fact.is_pinned,
        "explicit pinned=false should override PersistenceClassifier"
    );

    // Verify it gets expired by aggressive forget. Procedural is decay-exempt
    // by default, so re-enable its decay via an explicit half-life override
    // (an explicit override wins over the exemption) — the property under
    // test is the pinned=false override, not persistence semantics.
    let mut policy = aggressive_forget_policy();
    policy
        .half_life_overrides
        .insert(FactType::Procedural, 69.0);
    engine.forget(&policy).await.expect("forget failed");

    let fact_after = engine.get_fact(id).await.expect("get fact after forget");
    assert!(
        fact_after.t_expired.is_some(),
        "fact with explicit pinned=false should be expired by forget"
    );
}

#[tokio::test]
async fn unpin_allows_forgetting() {
    let engine = eval_engine();

    let id = add_fact(&engine, "once pinned, now unpinned", FactType::Episodic).await;
    engine.pin_fact(id).await.expect("pin_fact");

    // Verify pinned.
    assert!(engine.get_fact(id).await.unwrap().is_pinned);

    // Unpin.
    engine.unpin_fact(id).await.expect("unpin_fact");
    assert!(!engine.get_fact(id).await.unwrap().is_pinned);

    // Now it should be forgettable.
    let policy = aggressive_forget_policy();
    engine.forget(&policy).await.expect("forget");

    let fact = engine.get_fact(id).await.expect("get after forget");
    assert!(
        fact.t_expired.is_some(),
        "unpinned fact should be expired after forget"
    );
}
