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

/// Constant-vector embedder (dim 4) — the import paths embed every candidate.
struct FixedEmbedder;
impl memory_engine::EmbeddingProvider for FixedEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, memory_engine::MemoryError> {
        Ok(EMBEDDING.to_vec())
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("test", "test", DIM)
    }
}

fn embedder() -> std::sync::Arc<dyn memory_engine::EmbeddingProvider> {
    std::sync::Arc::new(FixedEmbedder)
}

fn config(scope: &str) -> memory_engine::BootstrapConfig {
    memory_engine::BootstrapConfig {
        scope: Some(scope.into()),
        ..Default::default()
    }
}

/// The one active fact holding `content`, if any.
async fn active_with(engine: &MemoryEngine, content: &str) -> Vec<i64> {
    let all = engine.list_active_facts(None).await.unwrap();
    all.iter()
        .filter(|f| f.content.contains(content))
        .map(|f| f.id)
        .collect()
}

#[tokio::test]
async fn re_importing_a_memory_dir_leaves_a_forgotten_fact_out() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("kestrel.md"),
        "---\nname: kestrel\nmetadata:\n  type: project\n---\nKestrels nest on the north ledge.\n",
    )
    .unwrap();
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let run = |scope: &'static str| {
        let engine = &engine;
        let dir = dir.path().to_owned();
        async move {
            engine
                .bootstrap_memory_directory(&dir, embedder(), &config(scope), None)
                .await
                .unwrap()
        }
    };
    assert_eq!(run("user:alice").await.facts_created, 1);
    let id = active_with(&engine, "Kestrels").await[0];
    assert_eq!(
        forget(&engine, id, &homes()).await,
        ForgetOutcome::Forgotten
    );
    for scope in ["user:alice", "unscoped:laptop"] {
        let report = run(scope).await;
        assert_eq!(
            (report.facts_created, report.facts_blocked),
            (0, 1),
            "{scope}"
        );
    }
    assert_eq!(active_with(&engine, "Kestrels").await, Vec::<i64>::new());
    engine.unforget_fact(id).await.unwrap();
    assert_eq!(run("user:alice").await.facts_reinforced, 1);
}

#[tokio::test]
async fn re_bootstrapping_a_session_leaves_a_forgotten_fact_out() {
    let session = include_str!("fixtures/success_session.jsonl");
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let mut cfg = config("user:alice");
    cfg.skip_existing = false;
    let run = || {
        engine.bootstrap_session(
            std::io::Cursor::new(session),
            embedder(),
            std::sync::Arc::new(memory_engine::KeywordExtractor),
            &cfg,
            None,
        )
    };
    let first = run().await.unwrap();
    assert!(first.facts_created > 0);
    let ids: Vec<i64> = engine
        .list_active_facts(None)
        .await
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    for &id in &ids {
        forget(&engine, id, &homes()).await;
    }
    let again = run().await.unwrap();
    assert_eq!(again.facts_created, 0);
    assert_eq!(again.facts_blocked, first.facts_created);
    assert_eq!(engine.list_active_facts(None).await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_partial_batch_fails_only_its_forgotten_entry() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let id = remember(&engine, "forgotten line", "user:alice").await;
    forget(&engine, id, &homes()).await;
    let entry = |content: &str| AddFactRequest {
        content: content.into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: Some("user:alice".into()),
        opts: None,
    };
    let results = engine
        .add_facts_batch_partial(
            &[entry("fresh line"), entry("forgotten line")],
            embedder(),
            None,
        )
        .await
        .unwrap();
    assert!(results[0].is_ok());
    assert!(
        matches!(
            results[1],
            Err(memory_engine::MemoryError::Conflict(
                memory_engine::ConflictError::Forgotten
            ))
        ),
        "{:?}",
        results[1]
    );
    assert_eq!(active_with(&engine, "line").await.len(), 1);
}

#[tokio::test]
async fn erase_mirrors_the_graph_and_the_vector_index() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let session = engine
        .ingest(&NewEvent {
            session_id: Some("s1".into()),
            ..record()
        })
        .await
        .unwrap();
    let fingerprint = EmbeddingFingerprint::new("test", "test", DIM);
    let mut ids = Vec::new();
    for content in ["kestrel nests", "kestrel eggs"] {
        let request = AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: Some(session),
            scope: Some("user:alice".into()),
            opts: None,
        };
        ids.push(
            engine
                .add_fact_precomputed(&request, EMBEDDING.to_vec(), &fingerprint, None)
                .await
                .unwrap(),
        );
    }
    engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(engine.graph_neighbors(ids[1]), vec![ids[0]]);
    let report = engine
        .erase(&memory_engine::types::EraseTarget::Fact(ids[0]))
        .await
        .unwrap();
    assert_eq!(report.erased_facts, vec![ids[0]]);
    assert_eq!(engine.graph_neighbors(ids[1]), Vec::<i64>::new());
    assert!(!engine.graph_has_node(ids[0]));
    for found in recall(&engine).await {
        assert!(!found.contains(&ids[0]), "{found:?}");
    }
}

#[tokio::test]
async fn erase_removes_the_sidecar_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let mut engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    let id = remember(&engine, "kestrel nests", "unscoped:laptop").await;
    engine.close().await.unwrap();
    assert_eq!(sidecars(dir.path()), 1, "a clean close writes the sidecar");
    let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    let report = engine
        .erase(&memory_engine::types::EraseTarget::Machine("laptop".into()))
        .await
        .unwrap();
    assert_eq!(report.erased_facts, vec![id]);
    assert!(report.purged, "{:?}", report.purge_error);
    assert_eq!(sidecars(dir.path()), 0, "erasure removes the sidecar");
}

fn sidecars(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".snapshot"))
        .count()
}

#[tokio::test]
async fn a_read_only_engine_refuses_erase_and_keeps_the_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let mut engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    remember(&engine, "kestrel nests", "user:alice").await;
    engine.close().await.unwrap();
    assert_eq!(sidecars(dir.path()), 1);
    let engine = MemoryEngine::builder(DIM)
        .path(&path)
        .read_only(true)
        .build()
        .unwrap();
    let err = engine
        .erase(&memory_engine::types::EraseTarget::Fact(1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, memory_engine::MemoryError::ReadOnly),
        "{err:?}"
    );
    assert_eq!(
        sidecars(dir.path()),
        1,
        "a refused erasure keeps the sidecar"
    );
}

#[tokio::test]
async fn an_erasure_that_takes_nothing_keeps_the_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let mut engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    remember(&engine, "kestrel nests", "unscoped:laptop").await;
    engine.close().await.unwrap();
    let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    let report = engine
        .erase(&memory_engine::types::EraseTarget::Machine(
            "desktop".into(),
        ))
        .await
        .unwrap();
    assert!(
        report.erased_facts.is_empty() && report.purged,
        "{report:?}"
    );
    assert_eq!(sidecars(dir.path()), 1, "nothing erased, nothing to drop");
}

#[tokio::test]
async fn an_invalid_machine_is_refused_before_the_sidecar_is_touched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let mut engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    remember(&engine, "kestrel nests", "unscoped:laptop").await;
    engine.close().await.unwrap();
    let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    let err = engine
        .erase(&memory_engine::types::EraseTarget::Machine("a/b".into()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            memory_engine::MemoryError::Conflict(memory_engine::ConflictError::PolicyParameter(_))
        ),
        "{err:?}"
    );
    assert_eq!(
        sidecars(dir.path()),
        1,
        "a refused erasure keeps the sidecar"
    );
}
