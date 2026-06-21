//! End-to-end integration for the Phase-5a dream cycle (#49).
//!
//! Exercises the public API only: seed facts → `run_dream_cycle` (produce) →
//! `apply_cycle_report` (apply) → assert end state → re-run is a near no-op. The
//! broader Phase-5a integration matrix (multi-cycle convergence, cross-scope,
//! large-N) is owned by #231 — this file ships the single happy-path proof plus
//! idempotency and read-only rejection.

#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::inspect::{ExpiredReason, FactState};
use memory_engine::{
    AddFactOptions, AddFactRequest, CycleDelta, DefaultDreamCycle, EmbeddingFingerprint,
    EmbeddingProvider, FactType, MemoryEngine, MemoryError, Outcome,
};
use tempfile::tempdir;

const DIM: usize = 4;

/// Deterministic embedder: content prefix selects an orthogonal basis vector so
/// "cluster*" facts group together and the lone facts stay separable.
struct TagEmbed;
impl EmbeddingProvider for TagEmbed {
    fn embed(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        let v = if text.starts_with("cluster") {
            [1.0, 0.0, 0.0, 0.0]
        } else if text.starts_with("neg") {
            [0.0, 1.0, 0.0, 0.0]
        } else {
            [0.0, 0.0, 1.0, 0.0]
        };
        Ok(v.to_vec())
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", 4)
    }
}

async fn add(engine: &MemoryEngine, content: &str, ft: FactType, importance: f64) -> i64 {
    let req = AddFactRequest {
        content: content.into(),
        fact_type: ft,
        source_event_id: None,
        scope: None,
        opts: Some(AddFactOptions {
            importance: Some(importance),
            ..Default::default()
        }),
    };
    engine
        .add_fact(
            &req,
            std::sync::Arc::new(TagEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn full_cycle_produces_and_applies_then_reruns_as_noop() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    // A promotable cluster: 3 high-importance Semantic facts sharing an embedding.
    for i in 0..3 {
        add(
            &engine,
            &format!("cluster fact {i}"),
            FactType::Semantic,
            0.8,
        )
        .await;
    }
    // A consistently-bad fact → quarantine.
    let neg_id = add(&engine, "neg fact", FactType::Episodic, 0.5).await;
    for _ in 0..3 {
        engine
            .record_outcome(neg_id, Outcome::Negative)
            .await
            .unwrap();
    }
    // A useful fact → rescore up.
    let pos_id = add(&engine, "pos fact", FactType::Episodic, 0.5).await;
    engine
        .record_outcome(pos_id, Outcome::Positive)
        .await
        .unwrap();
    engine
        .record_outcome(pos_id, Outcome::Positive)
        .await
        .unwrap();

    // PRODUCE (unapplied).
    let cycle = DefaultDreamCycle::with_defaults();
    let report = engine.run_dream_cycle(&cycle).await.unwrap();

    assert_eq!(report.metadata.facts_selected, 5);
    let promotes = report
        .deltas
        .iter()
        .filter(|d| matches!(d, CycleDelta::Promote { .. }))
        .count();
    assert_eq!(
        promotes, 1,
        "the Semantic cluster should yield one promotion"
    );
    assert!(
        report.deltas.iter().any(|d| matches!(
            d,
            CycleDelta::Quarantine { fact_id, .. } if *fact_id == neg_id
        )),
        "the consistently-negative fact should be quarantined"
    );
    assert!(
        report.deltas.iter().any(|d| matches!(
            d,
            CycleDelta::AdjustScore { fact_id, adjustment: 2 } if *fact_id == pos_id
        )),
        "the positively-rated fact should be rescored +2"
    );

    // APPLY.
    let applied = engine.apply_cycle_report(&report).await.unwrap();
    assert_eq!(applied.promoted, 1);
    assert_eq!(applied.quarantined, 1);
    assert_eq!(applied.scores_adjusted, 1);

    // End state: the quarantined fact is distinguishable from forgetting.
    assert_eq!(
        engine.explain_fact(neg_id).await.unwrap().state,
        FactState::Expired {
            reason: ExpiredReason::Quarantined
        }
    );

    // RE-RUN: the processed facts are excluded by the dream-cycle marker (and the
    // quarantined one is expired), so the second cycle proposes nothing.
    let report2 = engine.run_dream_cycle(&cycle).await.unwrap();
    assert!(
        report2.deltas.is_empty(),
        "a sequential re-run must be a near no-op, got: {:?}",
        report2.deltas
    );
    // The next cycle's id advances (history persisted on apply).
    assert_eq!(report2.metadata.cycle_id, report.metadata.cycle_id + 1);
}

#[tokio::test]
async fn run_dream_cycle_on_read_only_engine_is_rejected() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ro.db");
    {
        let _engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
    }
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();

    let err = engine
        .run_dream_cycle(&DefaultDreamCycle::with_defaults())
        .await
        .unwrap_err();
    assert!(
        matches!(err, MemoryError::ReadOnly),
        "read-only engine must reject a dream cycle, got: {err}"
    );
}
