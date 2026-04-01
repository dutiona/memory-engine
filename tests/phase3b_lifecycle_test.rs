//! Phase 3b integration test: full lifecycle with pinned facts, future memory, and forgetting.

use chrono::Utc;
use memory_engine::ResumeConfig;
use memory_engine::engine::MemoryEngine;
use memory_engine::error::Result;
use memory_engine::traits::{EmbeddingProvider, ForgetPolicy, PersistenceClassifier};
use memory_engine::types::{AddFactOptions, Fact, FactType};

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
}

#[test]
fn full_lifecycle_pinned_and_future_memory() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = TestEmbedder;
    let now = Utc::now();

    // 1. Add pinned identity fact
    let opts_pin = AddFactOptions {
        pinned: Some(true),
        importance: Some(0.95),
        ..Default::default()
    };
    let pin_id = engine
        .add_fact(
            "I am an AI assistant",
            FactType::Semantic,
            None,
            &embedder,
            None,
            Some(&opts_pin),
            None,
        )
        .unwrap();

    // 2. Add future reminder (t_valid in 24h)
    let future = now + chrono::Duration::hours(24);
    let opts_future = AddFactOptions {
        t_valid: Some(future),
        ..Default::default()
    };
    engine
        .add_fact(
            "Check release notes tomorrow",
            FactType::Episodic,
            None,
            &embedder,
            None,
            Some(&opts_future),
            None,
        )
        .unwrap();

    // 3. Add normal fact (forgettable)
    engine
        .add_fact(
            "Had coffee today",
            FactType::Episodic,
            None,
            &embedder,
            None,
            None,
            None,
        )
        .unwrap();

    // 4. resume_context at current time — future fact should NOT appear in due tier
    let ctx = engine
        .resume_context(&ResumeConfig {
            now,
            ..Default::default()
        })
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
    assert!(engine.list_due(now, None).unwrap().is_empty());

    // 6. list_due at future time — reminder surfaces
    let later = now + chrono::Duration::hours(25);
    let due = engine.list_due(later, None).unwrap();
    assert_eq!(due.len(), 1);
    assert!(due[0].content.contains("release notes"));

    // 7. next_due_time should return the future fact's t_valid
    let next = engine.next_due_time(None).unwrap();
    assert!(next.is_some(), "should have a next due time");

    // 8. Forget with aggressive policy — pinned fact survives
    let policy = ForgetPolicy {
        min_importance: 0.99,
        ..ForgetPolicy::default()
    };
    let stats = engine.forget(&policy).unwrap();
    // At least the coffee fact should be expired (low importance, no graph edges)
    assert!(
        stats.facts_expired >= 1,
        "at least one fact should be expired"
    );

    let fact = engine.get_fact(pin_id).unwrap();
    assert!(
        fact.t_expired.is_none(),
        "pinned fact must survive aggressive forget"
    );
    assert!(fact.is_pinned);

    // 9. Pin/unpin lifecycle
    engine.unpin_fact(pin_id).unwrap();
    let fact = engine.get_fact(pin_id).unwrap();
    assert!(!fact.is_pinned);
    engine.pin_fact(pin_id).unwrap();
    let fact = engine.get_fact(pin_id).unwrap();
    assert!(fact.is_pinned);
}

#[test]
fn classifier_auto_pins_semantic_facts() {
    struct PinSemantic;
    impl PersistenceClassifier for PinSemantic {
        fn should_pin(&self, fact: &Fact) -> bool {
            fact.fact_type == FactType::Semantic
        }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = TestEmbedder;
    let classifier = PinSemantic;

    // Semantic fact → auto-pinned by classifier
    let id1 = engine
        .add_fact(
            "core identity",
            FactType::Semantic,
            None,
            &embedder,
            None,
            None,
            Some(&classifier),
        )
        .unwrap();
    assert!(engine.get_fact(id1).unwrap().is_pinned);

    // Episodic fact → not pinned
    let id2 = engine
        .add_fact(
            "ephemeral event",
            FactType::Episodic,
            None,
            &embedder,
            None,
            None,
            Some(&classifier),
        )
        .unwrap();
    assert!(!engine.get_fact(id2).unwrap().is_pinned);

    // Explicit pinned=false overrides classifier
    let opts = AddFactOptions {
        pinned: Some(false),
        ..Default::default()
    };
    let id3 = engine
        .add_fact(
            "not pinned despite classifier",
            FactType::Semantic,
            None,
            &embedder,
            None,
            Some(&opts),
            Some(&classifier),
        )
        .unwrap();
    assert!(!engine.get_fact(id3).unwrap().is_pinned);
}

#[test]
fn resume_context_5_tier_integration() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = TestEmbedder;
    let now = Utc::now();

    // Tier 1: Pinned fact
    let opts_pin = AddFactOptions {
        pinned: Some(true),
        importance: Some(0.95),
        ..Default::default()
    };
    engine
        .add_fact(
            "pinned identity",
            FactType::Semantic,
            None,
            &embedder,
            None,
            Some(&opts_pin),
            None,
        )
        .unwrap();

    // Tier 3: Due fact (t_valid in the past)
    let past = now - chrono::Duration::hours(1);
    let opts_due = AddFactOptions {
        t_valid: Some(past),
        ..Default::default()
    };
    engine
        .add_fact(
            "past reminder",
            FactType::Episodic,
            None,
            &embedder,
            None,
            Some(&opts_due),
            None,
        )
        .unwrap();

    // Tier 4: Regular recent fact
    engine
        .add_fact(
            "recent observation",
            FactType::Episodic,
            None,
            &embedder,
            None,
            None,
            None,
        )
        .unwrap();

    let config = ResumeConfig {
        now,
        ..ResumeConfig::default()
    };
    let ctx = engine.resume_context(&config).unwrap();

    assert_eq!(ctx.pinned.len(), 1, "should have 1 pinned fact");
    assert_eq!(ctx.due.len(), 1, "should have 1 due fact");
    assert!(!ctx.recent.is_empty(), "should have recent facts");
    assert!(
        ctx.kb_stubs.is_empty(),
        "kb_stubs placeholder should be empty"
    );

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
