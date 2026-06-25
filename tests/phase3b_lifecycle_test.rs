//! Phase 3b integration test: full lifecycle with pinned facts, future memory, and forgetting.

#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use std::sync::Arc;

use chrono::Utc;
use memory_engine::EmbeddingFingerprint;
use memory_engine::ResumeConfig;
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::traits::{EmbeddingProvider, ForgetPolicy, PersistenceClassifier};
use memory_engine::types::{AddFactOptions, AddFactRequest, ClassifierInput, FactType};

const DIM: usize = 8;

struct TestEmbedder;

impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();
        let mut embedding = vec![0.0_f32; DIM];
        for (i, val) in embedding.iter_mut().enumerate() {
            let byte = bytes[i % 32];
            *val = (f32::from(byte) - 128.0) / 128.0;
        }
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut embedding {
                *val /= norm;
            }
        }
        Ok(embedding)
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", DIM)
    }
}

#[tokio::test]
async fn full_lifecycle_pinned_and_future_memory() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder);
    let now = Utc::now();

    // 1. Add pinned identity fact
    let opts_pin = AddFactOptions {
        pinned: Some(true),
        base_importance: Some(0.95),
        ..Default::default()
    };
    let pin_id = engine
        .add_fact(
            &AddFactRequest {
                content: "I am an AI assistant".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_pin),
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // 2. Add future reminder (t_valid in 24h)
    let future = now + chrono::Duration::hours(24);
    let opts_future = AddFactOptions {
        t_valid: Some(future),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Check release notes tomorrow".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_future),
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // 3. Add normal fact (forgettable)
    engine
        .add_fact(
            &AddFactRequest {
                content: "Had coffee today".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // 4. resume_context at current time — future fact should NOT appear in due tier
    let ctx = engine
        .resume_context(&ResumeConfig {
            now: Some(now),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !ctx.pinned.is_empty(),
        "pinned tier should have identity fact"
    );
    assert!(
        ctx.due.is_empty(),
        "due tier should be empty (future fact not yet due)"
    );

    // 5. list_due at current time — nothing due yet
    assert!(engine.list_due(now, None).await.unwrap().is_empty());

    // 6. list_due at future time — reminder surfaces
    let later = now + chrono::Duration::hours(25);
    let due = engine.list_due(later, None).await.unwrap();
    assert_eq!(due.len(), 1);
    assert!(due[0].content.contains("release notes"));

    // 7. next_due_time should return the future fact's t_valid
    let next = engine.next_due_time(None).await.unwrap();
    assert!(next.is_some(), "should have a next due time");

    // 8. Forget with aggressive policy — pinned fact survives
    let policy = ForgetPolicy {
        min_importance: 0.99,
        ..ForgetPolicy::default()
    };
    let stats = engine.forget(&policy).await.unwrap();
    // At least the coffee fact should be expired (low importance, no graph edges)
    assert!(
        stats.facts_expired >= 1,
        "at least one fact should be expired"
    );

    let fact = engine.get_fact(pin_id).await.unwrap();
    assert!(
        fact.t_expired.is_none(),
        "pinned fact must survive aggressive forget"
    );
    assert!(fact.is_pinned);

    // 9. Pin/unpin lifecycle
    engine.unpin_fact(pin_id).await.unwrap();
    let fact = engine.get_fact(pin_id).await.unwrap();
    assert!(!fact.is_pinned);
    engine.pin_fact(pin_id).await.unwrap();
    let fact = engine.get_fact(pin_id).await.unwrap();
    assert!(fact.is_pinned);
}

#[tokio::test]
async fn classifier_auto_pins_semantic_facts() {
    struct PinSemantic;
    impl PersistenceClassifier for PinSemantic {
        fn should_pin(&self, input: &ClassifierInput) -> bool {
            input.fact_type == FactType::Semantic
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder);
    let classifier: Arc<dyn PersistenceClassifier> = Arc::new(PinSemantic);

    // Semantic fact → auto-pinned by classifier
    let id1 = engine
        .add_fact(
            &AddFactRequest {
                content: "core identity".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            Some(classifier.clone()),
        )
        .await
        .unwrap();
    assert!(engine.get_fact(id1).await.unwrap().is_pinned);

    // Episodic fact → not pinned
    let id2 = engine
        .add_fact(
            &AddFactRequest {
                content: "ephemeral event".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            Some(classifier.clone()),
        )
        .await
        .unwrap();
    assert!(!engine.get_fact(id2).await.unwrap().is_pinned);

    // Explicit pinned=false overrides classifier
    let opts = AddFactOptions {
        pinned: Some(false),
        ..Default::default()
    };
    let id3 = engine
        .add_fact(
            &AddFactRequest {
                content: "not pinned despite classifier".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            embedder.clone(),
            Some(classifier.clone()),
        )
        .await
        .unwrap();
    assert!(!engine.get_fact(id3).await.unwrap().is_pinned);
}

#[tokio::test]
async fn resume_context_4_tier_integration() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(TestEmbedder);
    let now = Utc::now();

    // Tier 1: Pinned fact
    let opts_pin = AddFactOptions {
        pinned: Some(true),
        base_importance: Some(0.95),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "pinned identity".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_pin),
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Tier 3: Due fact (t_valid in the past)
    let past = now - chrono::Duration::hours(1);
    let opts_due = AddFactOptions {
        t_valid: Some(past),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "past reminder".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_due),
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Tier 4: Regular recent fact
    engine
        .add_fact(
            &AddFactRequest {
                content: "recent observation".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let config = ResumeConfig {
        now: Some(now),
        ..ResumeConfig::default()
    };
    let ctx = engine.resume_context(&config).await.unwrap();

    assert_eq!(ctx.pinned.len(), 1, "should have 1 pinned fact");
    assert_eq!(ctx.due.len(), 1, "should have 1 due fact");
    assert!(!ctx.recent.is_empty(), "should have recent facts");

    // Verify mutual exclusivity — no fact ID appears in multiple tiers
    let all_ids: Vec<i64> = ctx
        .pinned
        .iter()
        .chain(ctx.high_importance.iter())
        .chain(ctx.due.iter())
        .chain(ctx.recent.iter())
        .map(|f| f.id)
        .collect();
    let unique: std::collections::HashSet<i64> = all_ids.iter().copied().collect();
    assert_eq!(
        all_ids.len(),
        unique.len(),
        "no fact should appear in multiple tiers"
    );
}
