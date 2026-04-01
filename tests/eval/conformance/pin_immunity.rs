//! B4: Pin immunity conformance tests.
//!
//! Verifies that pinned facts survive aggressive forgetting, that
//! `PersistenceClassifier` auto-pins by type, and that explicit
//! `pinned=false` overrides the classifier.

use memory_engine::types::{AddFactOptions, AddFactRequest, FactType};

use crate::helpers::{PinByType, TestEmbedder, add_fact, aggressive_forget_policy, eval_engine};

#[test]
fn all_pinned_survive_aggressive_forget() {
    let engine = eval_engine();

    // 3 pinned facts.
    let mut pinned_ids = Vec::new();
    for i in 0..3 {
        let id = add_fact(
            &engine,
            &format!("critical pinned fact number {i}"),
            FactType::Semantic,
        );
        engine.pin_fact(id).expect("pin_fact failed");
        pinned_ids.push(id);
    }

    // 7 unpinned facts.
    let mut unpinned_ids = Vec::new();
    for i in 0..7 {
        let id = add_fact(
            &engine,
            &format!("disposable unpinned fact number {i}"),
            FactType::Episodic,
        );
        unpinned_ids.push(id);
    }

    let policy = aggressive_forget_policy();
    let stats = engine.forget(&policy).expect("forget failed");

    // All pinned facts should survive.
    for &pid in &pinned_ids {
        let fact = engine.get_fact(pid).expect("get pinned fact");
        assert!(
            fact.t_expired.is_none(),
            "pinned fact {pid} should survive aggressive forget"
        );
    }

    // All unpinned facts should be expired.
    for &uid in &unpinned_ids {
        let fact = engine.get_fact(uid).expect("get unpinned fact");
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

#[test]
fn persistence_classifier_auto_pins_by_type() {
    let engine = eval_engine();
    let embedder = TestEmbedder;
    let classifier = PinByType {
        pinned_type: FactType::Procedural,
    };

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
            &embedder,
            Some(&classifier),
        )
        .expect("add procedural fact");

    // Add a Semantic fact — classifier should NOT pin it.
    let sem_id = engine
        .add_fact(
            &AddFactRequest {
                content: "general knowledge about servers".to_string(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            Some(&classifier),
        )
        .expect("add semantic fact");

    let proc_fact = engine.get_fact(proc_id).expect("get procedural fact");
    let sem_fact = engine.get_fact(sem_id).expect("get semantic fact");

    assert!(
        proc_fact.is_pinned,
        "Procedural fact should be auto-pinned by classifier"
    );
    assert!(
        !sem_fact.is_pinned,
        "Semantic fact should NOT be auto-pinned"
    );

    // Verify the pinned fact survives forget.
    let policy = aggressive_forget_policy();
    engine.forget(&policy).expect("forget failed");

    let proc_after = engine
        .get_fact(proc_id)
        .expect("get procedural after forget");
    let sem_after = engine.get_fact(sem_id).expect("get semantic after forget");

    assert!(
        proc_after.t_expired.is_none(),
        "auto-pinned Procedural fact should survive forget"
    );
    assert!(
        sem_after.t_expired.is_some(),
        "unpinned Semantic fact should be expired"
    );
}

#[test]
fn explicit_pinned_false_overrides_classifier() {
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
            &embedder,
            Some(&classifier),
        )
        .expect("add fact with pinned=false");

    let fact = engine.get_fact(id).expect("get fact");
    assert!(
        !fact.is_pinned,
        "explicit pinned=false should override PersistenceClassifier"
    );

    // Verify it gets expired by aggressive forget.
    let policy = aggressive_forget_policy();
    engine.forget(&policy).expect("forget failed");

    let fact_after = engine.get_fact(id).expect("get fact after forget");
    assert!(
        fact_after.t_expired.is_some(),
        "fact with explicit pinned=false should be expired by forget"
    );
}

#[test]
fn unpin_allows_forgetting() {
    let engine = eval_engine();

    let id = add_fact(&engine, "once pinned, now unpinned", FactType::Semantic);
    engine.pin_fact(id).expect("pin_fact");

    // Verify pinned.
    assert!(engine.get_fact(id).unwrap().is_pinned);

    // Unpin.
    engine.unpin_fact(id).expect("unpin_fact");
    assert!(!engine.get_fact(id).unwrap().is_pinned);

    // Now it should be forgettable.
    let policy = aggressive_forget_policy();
    engine.forget(&policy).expect("forget");

    let fact = engine.get_fact(id).expect("get after forget");
    assert!(
        fact.t_expired.is_some(),
        "unpinned fact should be expired after forget"
    );
}
