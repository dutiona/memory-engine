//! The forget retraction through the facade (#1023): Homes resolution, every read path, the
//! reversal, `explain`, and the read-only refusal.
#![allow(clippy::unwrap_used)] // test code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::engine::MemoryEngine;
use memory_engine::inspect::{ExpiredReason, FactState};
use memory_engine::types::{
    AddFactRequest, EventType, FactType, ForgetOutcome, NewEvent, UnforgetOutcome,
};
use memory_engine::{EmbeddingFingerprint, MemoryQuery};

const DIM: usize = 4;
const EMBEDDING: [f32; DIM] = [0.3, 0.1, 0.4, 0.1];

fn record() -> NewEvent {
    NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: EventType::MemoryOp,
        payload: serde_json::json!({ "tool": "forget" }),
        source: "test".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "test".into(),
        sequence_id: 0,
        created_at: None,
    }
}

fn homes() -> Vec<String> {
    vec!["unscoped:laptop".into(), "user:alice".into()]
}

async fn remember(engine: &MemoryEngine, content: &str, scope: &str) -> i64 {
    let fingerprint = EmbeddingFingerprint::new("test", "test", DIM);
    engine
        .add_fact_precomputed(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some(scope.into()),
                opts: None,
            },
            EMBEDDING.to_vec(),
            &fingerprint,
            None,
        )
        .await
        .unwrap()
}

async fn forget(engine: &MemoryEngine, id: i64, homes: &[String]) -> ForgetOutcome {
    engine
        .forget_fact(id, homes, "wrong", "unscoped:laptop", &record())
        .await
        .unwrap()
        .0
}

/// The ids recall reads in the two Homes, by text, by vector, by both, and listing.
async fn recall(engine: &MemoryEngine) -> [Vec<i64>; 4] {
    let base = || MemoryQuery::new().scope_exact_set(homes()).limit(10);
    let mut out: [Vec<i64>; 4] = Default::default();
    let queries = [
        base().text("\"kestrel\""),
        base().embedding(EMBEDDING.to_vec()),
        base().text("\"kestrel\"").embedding(EMBEDDING.to_vec()),
        base(),
    ];
    for (slot, query) in out.iter_mut().zip(queries) {
        let response = engine.execute_query(&query).await.unwrap();
        *slot = response.results.iter().map(|r| r.fact.id).collect();
    }
    out
}

#[tokio::test]
async fn forget_fact_resolves_homes_exactly() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let id = remember(&engine, "kestrel nests", "user:alice/project:x").await;
    // A parent path does not reach the child, and an unknown path names nothing.
    assert_eq!(
        forget(&engine, id, &["user:alice".into()]).await,
        ForgetOutcome::NotFound
    );
    assert_eq!(
        forget(&engine, id, &["user:nobody".into()]).await,
        ForgetOutcome::NotFound
    );
    assert_eq!(forget(&engine, id, &[]).await, ForgetOutcome::NotFound);
    assert!(engine.get_fact(id).await.unwrap().t_expired.is_none());
}

#[tokio::test]
async fn a_forgotten_fact_leaves_every_read_path_until_unforgotten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let mut engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    let id = remember(&engine, "kestrel nests", "user:alice").await;
    for ids in recall(&engine).await {
        assert_eq!(ids, vec![id]);
    }
    assert_eq!(
        forget(&engine, id, &homes()).await,
        ForgetOutcome::Forgotten
    );
    for ids in recall(&engine).await {
        assert_eq!(ids, Vec::<i64>::new());
    }
    assert_eq!(engine.list_active_facts(None).await.unwrap().len(), 0);
    engine.close().await.unwrap();

    let mut engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    for ids in recall(&engine).await {
        assert_eq!(ids, Vec::<i64>::new(), "after a reopen");
    }
    assert_eq!(
        engine.unforget_fact(id).await.unwrap(),
        UnforgetOutcome::Restored {
            facts: vec![(id, true)]
        }
    );
    for ids in recall(&engine).await {
        assert_eq!(ids, vec![id]);
    }
    engine.close().await.unwrap();
    let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    for ids in recall(&engine).await {
        assert_eq!(ids, vec![id], "after a reopen");
    }
}

#[tokio::test]
async fn explain_names_a_forget() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let id = remember(&engine, "kestrel nests", "unscoped:laptop").await;
    let (_, record) = engine
        .forget_fact(id, &homes(), "it moved", "unscoped:laptop", &record())
        .await
        .unwrap();
    let FactState::Expired {
        reason:
            ExpiredReason::Forgotten {
                reason,
                record_event_id,
                ..
            },
    } = engine.explain_fact(id).await.unwrap().state
    else {
        panic!("a forgotten fact explains as forgotten");
    };
    assert_eq!(reason.as_deref(), Some("it moved"));
    assert_eq!(record_event_id, Some(record));
    engine.unforget_fact(id).await.unwrap();
    assert_eq!(
        engine.explain_fact(id).await.unwrap().state,
        FactState::Active
    );
}

#[tokio::test]
async fn a_read_only_engine_refuses_forget_and_unforget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let mut engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    let id = remember(&engine, "kestrel nests", "user:alice").await;
    engine.close().await.unwrap();
    let engine = MemoryEngine::builder(DIM)
        .path(&path)
        .read_only(true)
        .build()
        .unwrap();
    let err = engine
        .forget_fact(id, &homes(), "wrong", "unscoped:laptop", &record())
        .await
        .unwrap_err();
    assert!(
        matches!(err, memory_engine::MemoryError::ReadOnly),
        "{err:?}"
    );
    let err = engine.unforget_fact(id).await.unwrap_err();
    assert!(
        matches!(err, memory_engine::MemoryError::ReadOnly),
        "{err:?}"
    );
}
