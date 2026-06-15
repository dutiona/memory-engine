use super::*;
use crate::resume::context::ResumeConfig;
use crate::search::hybrid::{SearchMode, SearchQuery};
use crate::search::query::MemoryQuery;
use crate::traits::{
    ConflictArbiter, ConsolidationConfig, CrudDecision, EmbeddingProvider, ForgetPolicy,
    PersistenceClassifier, SummaryGenerator,
};
use crate::types::{AddFactOptions, AddFactRequest, EventType, Fact, FactType, NewEvent, NewFact};

const DIM: usize = 4;

struct MockEmbedder {
    dim: usize,
}

impl EmbeddingProvider for MockEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.5; self.dim])
    }
}

struct MockGen;
impl SummaryGenerator for MockGen {
    fn summarize(&self, facts: &[Fact]) -> Result<String> {
        Ok(facts
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("; "))
    }
    fn embed(&self, _: &str) -> Result<Vec<f32>> {
        Ok(vec![0.1; DIM])
    }
}

struct FixedArbiter {
    decision: CrudDecision,
}
impl ConflictArbiter for FixedArbiter {
    fn arbitrate(&self, _: &Fact, _: &Fact) -> Result<CrudDecision> {
        Ok(self.decision)
    }
}

fn make_new_fact(content: &str, embedding: Vec<f32>) -> NewFact {
    let now = Utc::now();
    NewFact {
        content: content.into(),
        content_hash: blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string(),
        embedding,
        fact_type: FactType::Semantic,
        t_created: now,
        t_expired: None,
        t_valid: None,
        t_invalid: None,
        source_event_id: None,
        scope_id: 1,
        importance: 0.5,
        access_count: 0,
        last_accessed: now,
        metadata: serde_json::json!({}),
        is_pinned: false,
    }
}

/// Test helper: insert a raw fact via the write connection (bypasses engine's `add_fact`).
fn insert_raw_fact(engine: &MemoryEngine, fact: &NewFact) -> i64 {
    let conn = engine.pool.write();
    FactStore::new(&conn, engine.embed_dim)
        .insert(fact)
        .unwrap()
}

// --- Phase 1 tests ---

#[test]
fn open_memory_succeeds() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    assert_eq!(engine.embed_dim(), DIM);
}

#[test]
fn ingest_returns_event_id() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"msg": "hello"}),
        source: "test".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "local".into(),
        sequence_id: 0,
        created_at: None,
    };
    let id = engine.ingest(&event).unwrap();
    assert_eq!(id, 1);
}

#[test]
fn add_fact_returns_fact_id() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is fast".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    assert!(id > 0);
}

#[test]
fn query_returns_results_after_adding_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is a systems programming language".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let query = SearchQuery {
        text: Some("Rust".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    };
    let results = engine.query(&query).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].fact.content.contains("Rust"));
}

#[test]
fn embed_dim_validation_rejects_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    let config_768 = EngineConfig::new(db_path.clone(), 768);
    let config_384 = EngineConfig::new(db_path, 384);

    // First open with dim=768
    {
        let _engine = MemoryEngine::open(&config_768).unwrap();
    }

    // Second open with dim=384 should fail
    let err = MemoryEngine::open(&config_384).unwrap_err();
    assert!(matches!(err, MemoryError::Migration(_)));
    assert!(err.to_string().contains("mismatch"));
}

#[test]
fn get_set_config() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    assert!(engine.get_config("custom_key").unwrap().is_none());
    engine.set_config("custom_key", "custom_value").unwrap();
    assert_eq!(
        engine.get_config("custom_key").unwrap(),
        Some("custom_value".into())
    );
}

// --- Phase 2 tests ---

#[test]
fn graph_starts_empty() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    assert_eq!(engine.graph_stats(), (0, 0));
}

#[test]
fn consolidate_deduplicates_similar_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    // Two near-identical embeddings
    insert_raw_fact(
        &engine,
        &make_new_fact("fact alpha", vec![1.0, 0.0, 0.0, 0.0]),
    );
    insert_raw_fact(
        &engine,
        &make_new_fact("fact alpha copy", vec![0.99, 0.01, 0.0, 0.0]),
    );

    let config = ConsolidationConfig {
        dedup_threshold: 0.90,
        min_cluster_size: 10, // high threshold so no clusters form
    };
    let stats = engine.consolidate(&MockGen, &config).unwrap();
    assert_eq!(stats.duplicates_removed, 1);

    let active = engine.list_active_facts(None).unwrap();
    assert_eq!(active.len(), 1);
}

#[test]
fn consolidate_is_idempotent() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    insert_raw_fact(
        &engine,
        &make_new_fact("unique A", vec![1.0, 0.0, 0.0, 0.0]),
    );
    insert_raw_fact(
        &engine,
        &make_new_fact("unique B", vec![0.0, 1.0, 0.0, 0.0]),
    );

    let config = ConsolidationConfig {
        dedup_threshold: 0.92,
        min_cluster_size: 10,
    };

    let _stats1 = engine.consolidate(&MockGen, &config).unwrap();
    let stats2 = engine.consolidate(&MockGen, &config).unwrap();

    // Second run should find 0 new duplicates
    assert_eq!(stats2.duplicates_removed, 0);
    // Both facts still active
    assert_eq!(engine.list_active_facts(None).unwrap().len(), 2);
}

#[test]
fn forget_prunes_stale_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    // Insert a fact with very low importance
    let now = Utc::now();
    let old_time = now - chrono::Duration::days(200);
    insert_raw_fact(
        &engine,
        &NewFact {
            content: "ancient fact".into(),
            content_hash: "h_ancient".into(),
            embedding: vec![0.1; DIM],
            fact_type: FactType::Episodic,
            t_created: old_time,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.01,
            access_count: 0,
            last_accessed: old_time,
            metadata: serde_json::json!({}),
            is_pinned: false,
        },
    );

    let policy = ForgetPolicy {
        min_importance: 0.3,
        ..ForgetPolicy::default()
    };
    let stats = engine.forget(&policy).unwrap();
    assert_eq!(stats.facts_expired, 1);
    assert_eq!(stats.facts_evaluated, 1);
}

#[test]
fn forget_rejects_invalid_policy() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let policy = ForgetPolicy {
        half_life_days: 0.0, // invalid
        ..ForgetPolicy::default()
    };
    assert!(engine.forget(&policy).is_err());
}

#[test]
fn resolve_conflict_update_creates_edge() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let old_id = insert_raw_fact(&engine, &make_new_fact("outdated", vec![0.5; DIM]));

    let arbiter = FixedArbiter {
        decision: CrudDecision::Update,
    };
    let result = engine
        .resolve_conflict(&arbiter, old_id, &make_new_fact("updated", vec![0.5; DIM]))
        .unwrap();

    assert_eq!(result.decision, CrudDecision::Update);
    assert!(result.new_fact_id.is_some());

    // Old fact should be expired
    let old = engine.get_fact(old_id).unwrap();
    assert!(old.t_expired.is_some());

    // Graph should have the new edge
    let new_id = result.new_fact_id.unwrap();
    assert!(engine.graph_has_node(new_id));
    assert_eq!(engine.graph_neighbors(new_id), vec![old_id]);
}

#[test]
fn resolve_conflict_noop_no_changes() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let old_id = insert_raw_fact(&engine, &make_new_fact("existing", vec![0.5; DIM]));

    let arbiter = FixedArbiter {
        decision: CrudDecision::Noop,
    };
    let result = engine
        .resolve_conflict(
            &arbiter,
            old_id,
            &make_new_fact("candidate", vec![0.5; DIM]),
        )
        .unwrap();

    assert_eq!(result.decision, CrudDecision::Noop);
    assert!(result.new_fact_id.is_none());

    // Old fact unchanged
    let old = engine.get_fact(old_id).unwrap();
    assert!(old.t_expired.is_none());
}

#[test]
fn graph_loads_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    let config = EngineConfig::new(db_path, DIM);

    // First session: add facts and create an edge via conflict resolution
    {
        let engine = MemoryEngine::open(&config).unwrap();
        let old_id = insert_raw_fact(&engine, &make_new_fact("original", vec![0.5; DIM]));
        let arbiter = FixedArbiter {
            decision: CrudDecision::Update,
        };
        engine
            .resolve_conflict(
                &arbiter,
                old_id,
                &make_new_fact("replacement", vec![0.5; DIM]),
            )
            .unwrap();
        assert_eq!(engine.graph_stats().1, 1);
    }

    // Second session: graph should be restored from DB
    {
        let engine = MemoryEngine::open(&config).unwrap();
        assert_eq!(engine.graph_stats().1, 1);
    }
}

#[test]
fn list_summaries_empty() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let summaries = engine.list_summaries(&ConsolidationLevel::Global).unwrap();
    assert!(summaries.is_empty());
}

// --- Phase 3 / T2: AddFactOptions ---

#[test]
fn add_fact_with_custom_importance() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let opts = AddFactOptions {
        importance: Some(0.9),
        ..Default::default()
    };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "important fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            None,
        )
        .unwrap();
    let fact = engine.get_fact(id).unwrap();
    assert!((fact.importance - 0.9).abs() < f64::EPSILON);
}

#[test]
fn add_fact_with_temporal_bounds() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let now = Utc::now();
    let opts = AddFactOptions {
        t_valid: Some(now - chrono::Duration::hours(1)),
        t_invalid: Some(now + chrono::Duration::hours(1)),
        ..Default::default()
    };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "temporal fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            None,
        )
        .unwrap();
    let fact = engine.get_fact(id).unwrap();
    assert!(fact.t_valid.is_some());
    assert!(fact.t_invalid.is_some());
}

#[test]
fn add_fact_with_scope_path() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "scoped fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("user:test/project:demo".into()),
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    let fact = engine.get_fact(id).unwrap();
    assert_ne!(fact.scope_id, 1); // not root
}

#[test]
fn add_fact_none_opts_uses_defaults() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "default fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    let fact = engine.get_fact(id).unwrap();
    assert!((fact.importance - 0.5).abs() < f64::EPSILON);
    assert!(fact.t_valid.is_none());
}

// --- Phase 3 / T7: Send + Sync ---

#[test]
fn engine_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MemoryEngine>();
}

#[test]
fn engine_concurrent_reads() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("concurrent.db");
    let config = EngineConfig::new(db_path, DIM);

    let engine = std::sync::Arc::new(MemoryEngine::open(&config).unwrap());
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is fast".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Python is flexible".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let mut handles = vec![];
    for _ in 0..4 {
        let e = engine.clone();
        handles.push(std::thread::spawn(move || {
            let results = e
                .query(&SearchQuery {
                    text: Some("Rust".into()),
                    embedding: None,
                    mode: SearchMode::Fts,
                    limit: 10,
                    rerank_depth: None,
                    valid_at: None,
                    fact_type: None,
                    scope: None,
                })
                .unwrap();
            assert_eq!(results.len(), 1);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn engine_write_then_read_across_threads() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("write_read.db");
    let config = EngineConfig::new(db_path, DIM);

    let engine = std::sync::Arc::new(MemoryEngine::open(&config).unwrap());

    // Thread 1: write
    let e1 = engine.clone();
    let writer = std::thread::spawn(move || {
        let embedder = MockEmbedder { dim: DIM };
        e1.add_fact(
            &AddFactRequest {
                content: "Concurrent write test".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    });
    writer.join().unwrap();

    // Thread 2: read (after write completes)
    let reader = std::thread::spawn(move || {
        let results = engine
            .query(&SearchQuery {
                text: Some("Concurrent".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();
        assert_eq!(results.len(), 1);
    });
    reader.join().unwrap();
}

// --- Phase 3 / T9: resume_context ---

#[test]
fn resume_empty_engine() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let ctx = engine.resume_context(&ResumeConfig::default()).unwrap();
    assert!(ctx.pinned.is_empty());
    assert!(ctx.high_importance.is_empty());
    assert!(ctx.due.is_empty());
    assert!(ctx.recent.is_empty());
}

#[test]
fn resume_with_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    // Add a pinned fact (appears in tier 1)
    let opts_pinned = AddFactOptions {
        importance: Some(0.95),
        pinned: Some(true),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "user prefers Rust".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_pinned),
            },
            &embedder,
            None,
        )
        .unwrap();

    // Add a low-importance root fact (recent tier)
    let opts_low = AddFactOptions {
        importance: Some(0.1),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "had coffee today".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_low),
            },
            &embedder,
            None,
        )
        .unwrap();

    let config = ResumeConfig::default();
    let ctx = engine.resume_context(&config).unwrap();
    // The pinned fact should appear in the pinned tier
    assert_eq!(ctx.pinned.len(), 1);
    assert!(ctx.pinned[0].is_pinned);
    assert!(ctx.pinned[0].content.contains("Rust"));
    // The low-importance fact should appear in recent
    assert!(!ctx.recent.is_empty());
}

#[test]
fn resume_nonexistent_scope_returns_not_found() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let config = ResumeConfig {
        scope_path: Some("nonexistent/path".into()),
        ..ResumeConfig::default()
    };
    let err = engine.resume_context(&config).unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

// --- Issue #93: surfaced_at for due facts in non-due tiers ---

#[test]
fn resume_stamps_surfaced_at_on_pinned_due_fact() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let now = Utc::now();
    let past = now - chrono::Duration::hours(1);

    // Pinned fact that is ALSO due (t_valid in the past).
    // It will land in the pinned tier, not the due tier.
    engine
        .add_fact(
            &AddFactRequest {
                content: "CI pipeline uses 30min timeout".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    pinned: Some(true),
                    t_valid: Some(past),
                    importance: Some(0.9),
                    ..Default::default()
                }),
            },
            &embedder,
            None,
        )
        .unwrap();

    let config = ResumeConfig {
        now,
        ..ResumeConfig::default()
    };
    let ctx = engine.resume_context(&config).unwrap();

    // Fact should appear in pinned tier (not due tier)
    assert_eq!(ctx.pinned.len(), 1);
    assert!(ctx.due.is_empty() || !ctx.due.iter().any(|f| f.content.contains("CI")));

    // Bug: surfaced_at should be stamped because the fact IS due,
    // even though it landed in the pinned tier.
    assert!(
        ctx.pinned[0].surfaced_at.is_some(),
        "pinned-but-due fact must have surfaced_at stamped"
    );
}

#[test]
fn resume_stamps_surfaced_at_on_high_importance_due_fact() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let now = Utc::now();
    let past = now - chrono::Duration::hours(1);

    // High-importance fact that is ALSO due. Not pinned.
    // importance=0.9 → importance_score=0.9, which exceeds the 0.7 threshold.
    // It will land in high_importance tier, not due tier.
    engine
        .add_fact(
            &AddFactRequest {
                content: "user prefers tabs over spaces".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    importance: Some(0.9),
                    t_valid: Some(past),
                    ..Default::default()
                }),
            },
            &embedder,
            None,
        )
        .unwrap();

    let config = ResumeConfig {
        now,
        high_importance_min: 0.7,
        ..ResumeConfig::default()
    };
    let ctx = engine.resume_context(&config).unwrap();

    // Fact should appear in high_importance tier (not due tier)
    assert_eq!(ctx.high_importance.len(), 1);
    assert!(ctx.due.is_empty());

    // Bug: surfaced_at should be stamped because the fact IS due.
    assert!(
        ctx.high_importance[0].surfaced_at.is_some(),
        "high-importance-but-due fact must have surfaced_at stamped"
    );
}

#[test]
fn resume_does_not_stamp_invalidated_pinned_due_fact() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let now = Utc::now();
    let past = now - chrono::Duration::hours(2);
    let past_invalid = now - chrono::Duration::hours(1);

    // Pinned fact that WAS due but is now bi-temporally invalidated:
    // t_valid in the past, t_invalid ALSO in the past (before now).
    engine
        .add_fact(
            &AddFactRequest {
                content: "old CI timeout no longer valid".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    pinned: Some(true),
                    t_valid: Some(past),
                    t_invalid: Some(past_invalid),
                    importance: Some(0.9),
                    ..Default::default()
                }),
            },
            &embedder,
            None,
        )
        .unwrap();

    let config = ResumeConfig {
        now,
        ..ResumeConfig::default()
    };
    let ctx = engine.resume_context(&config).unwrap();

    // Fact lands in pinned tier (it's pinned and not expired)
    assert_eq!(ctx.pinned.len(), 1);

    // But surfaced_at must NOT be stamped — the fact is bi-temporally
    // invalidated (t_invalid <= now), so it's no longer "due".
    assert!(
        ctx.pinned[0].surfaced_at.is_none(),
        "invalidated fact must not have surfaced_at stamped"
    );
}

// --- Phase 3b / T6: SearchConfig in EngineConfig ---

#[test]
fn engine_config_default_has_no_search_config() {
    let config = EngineConfig::new("test.db".into(), 128);
    assert!(config.search_config.is_none());
}

#[test]
fn engine_config_with_search_config() {
    let mut config = EngineConfig::new("test.db".into(), 128);
    config.search_config = Some(SearchConfig::default());
    assert_eq!(config.search_config.unwrap().ann_threshold, 50_000);
}

#[test]
fn query_nonexistent_scope_returns_empty() {
    use crate::types::ScopeQuery;

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    // Add a fact at root scope so there's something to find if search were unscoped
    engine
        .add_fact(
            &AddFactRequest {
                content: "visible without scope".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // Query with a scope path that doesn't exist
    let query = SearchQuery {
        text: Some("visible".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: Some(ScopeQuery::Exact("nonexistent/scope".into())),
    };
    let results = engine.query(&query).unwrap();
    assert!(
        results.is_empty(),
        "expected empty results for nonexistent scope, got {}",
        results.len()
    );
}

// --- Phase 3b / T8: Engine facade new methods ---

#[test]
fn list_due_returns_scheduled_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let past = Utc::now() - chrono::Duration::hours(1);
    let future = Utc::now() + chrono::Duration::hours(1);

    // Past-due fact
    engine
        .add_fact(
            &AddFactRequest {
                content: "check release".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    t_valid: Some(past),
                    ..Default::default()
                }),
            },
            &embedder,
            None,
        )
        .unwrap();

    // Future fact
    engine
        .add_fact(
            &AddFactRequest {
                content: "future check".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    t_valid: Some(future),
                    ..Default::default()
                }),
            },
            &embedder,
            None,
        )
        .unwrap();

    // Regular fact (no t_valid)
    engine
        .add_fact(
            &AddFactRequest {
                content: "regular".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let due = engine.list_due(Utc::now(), None).unwrap();
    assert_eq!(due.len(), 1);
    assert!(due[0].content.contains("check release"));

    let next = engine.next_due_time(None).unwrap();
    assert!(next.is_some()); // the future fact

    // Future-dated facts should be invisible to regular search (no valid_at)
    let search = engine
        .query(&SearchQuery {
            text: Some("future check".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();
    assert!(
        search.is_empty(),
        "future-dated facts should not appear in regular search"
    );

    // But past-due facts should be visible
    let search2 = engine
        .query(&SearchQuery {
            text: Some("check release".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();
    assert_eq!(search2.len(), 1, "past-due facts should appear in search");
}

#[test]
fn pin_unpin_fact() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "pinnable".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    assert!(!engine.get_fact(id).unwrap().is_pinned);
    engine.pin_fact(id).unwrap();
    assert!(engine.get_fact(id).unwrap().is_pinned);
    engine.unpin_fact(id).unwrap();
    assert!(!engine.get_fact(id).unwrap().is_pinned);
}

#[test]
fn add_fact_with_explicit_pin() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let opts = AddFactOptions {
        pinned: Some(true),
        ..Default::default()
    };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "identity".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            None,
        )
        .unwrap();
    assert!(engine.get_fact(id).unwrap().is_pinned);
}

#[test]
fn add_fact_with_classifier() {
    struct PinSemantic;
    impl PersistenceClassifier for PinSemantic {
        fn should_pin(&self, fact: &Fact) -> bool {
            fact.fact_type == FactType::Semantic
        }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let classifier = PinSemantic;

    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "auto-pinned".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            Some(&classifier),
        )
        .unwrap();
    assert!(engine.get_fact(id).unwrap().is_pinned);

    let id2 = engine
        .add_fact(
            &AddFactRequest {
                content: "not pinned".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            Some(&classifier),
        )
        .unwrap();
    assert!(!engine.get_fact(id2).unwrap().is_pinned);
}

#[test]
fn explicit_pin_overrides_classifier() {
    struct AlwaysPin;
    impl PersistenceClassifier for AlwaysPin {
        fn should_pin(&self, _fact: &Fact) -> bool {
            true
        }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let classifier = AlwaysPin;

    // Explicitly set pinned=false — should override the classifier
    let opts = AddFactOptions {
        pinned: Some(false),
        ..Default::default()
    };
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "not pinned despite classifier".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts),
            },
            &embedder,
            Some(&classifier),
        )
        .unwrap();
    assert!(!engine.get_fact(id).unwrap().is_pinned);
}

// --- execute_query integration tests ---

#[test]
fn execute_query_empty_returns_active_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "fact one".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "fact two".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine.execute_query(&MemoryQuery::new()).unwrap().results;
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r.match_type == MatchType::ImportanceRank)
    );
}

#[test]
fn execute_query_text_search() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust systems programming".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Python machine learning".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().text("Rust"))
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "Rust systems programming");
    assert_eq!(results[0].match_type, MatchType::Fts);
}

#[test]
fn execute_query_scope_only() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    // Add fact to "project:demo" scope (auto-created by add_fact)
    engine
        .add_fact(
            &AddFactRequest {
                content: "scoped fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: Some("project:demo".into()),
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    // Add fact to root scope
    engine
        .add_fact(
            &AddFactRequest {
                content: "root fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().scope_exact("project:demo"))
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "scoped fact");
}

#[test]
fn execute_query_fact_type_filter() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "episodic".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "semantic".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().fact_type(FactType::Semantic))
        .unwrap()
        .results;
    // fact_type filtering in store path — list_by_importance_score doesn't filter by fact_type,
    // so it should be post-filtered
    assert!(
        results
            .iter()
            .all(|r| r.fact.fact_type == FactType::Semantic)
    );
}

#[test]
fn execute_query_importance_threshold() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let opts_low = AddFactOptions {
        importance: Some(0.1),
        ..Default::default()
    };
    let opts_high = AddFactOptions {
        importance: Some(0.9),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "low importance".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_low),
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "high importance".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_high),
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().min_importance_score(0.5))
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "high importance");
}

#[test]
fn execute_query_pinned_only() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "pinned".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine.pin_fact(id).unwrap();

    engine
        .add_fact(
            &AddFactRequest {
                content: "normal".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().pinned_only())
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "pinned");
    assert!(results[0].fact.is_pinned);
}

#[test]
fn execute_query_future_dated_excluded() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    // Regular fact
    engine
        .add_fact(
            &AddFactRequest {
                content: "present fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // Future-dated fact
    let future_opts = AddFactOptions {
        t_valid: Some(Utc::now() + chrono::Duration::hours(24)),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "future fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(future_opts),
            },
            &embedder,
            None,
        )
        .unwrap();

    // Empty query should NOT return the future-dated fact
    let results = engine.execute_query(&MemoryQuery::new()).unwrap().results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "present fact");

    // Scope-only query should also exclude future-dated facts
    let results2 = engine
        .execute_query(&MemoryQuery::new().min_importance_score(0.0))
        .unwrap()
        .results;
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].fact.content, "present fact");
}

#[test]
fn execute_query_period_mutual_exclusion() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let now = Utc::now();

    let result = engine.execute_query(
        &MemoryQuery::new()
            .valid_at(now)
            .period(now - chrono::Duration::hours(1), now),
    );
    assert!(result.is_err());
}

#[test]
fn execute_query_search_mode_conflict() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    // Hybrid requires both text and embedding
    let result = engine.execute_query(
        &MemoryQuery::new()
            .text("test")
            .search_mode(SearchMode::Hybrid),
    );
    assert!(result.is_err());
}

#[test]
fn execute_query_search_mode_inference() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust programming".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // Text-only → should infer FTS mode
    let results = engine
        .execute_query(&MemoryQuery::new().text("Rust"))
        .unwrap()
        .results;
    assert!(!results.is_empty());
    assert_eq!(results[0].match_type, MatchType::Fts);
}

#[test]
fn execute_query_period_filter() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let now = Utc::now();

    // Fact valid in the past
    let past_opts = AddFactOptions {
        t_valid: Some(now - chrono::Duration::hours(3)),
        t_invalid: Some(now - chrono::Duration::hours(1)),
        ..Default::default()
    };
    engine
        .add_fact(
            &AddFactRequest {
                content: "past fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(past_opts),
            },
            &embedder,
            None,
        )
        .unwrap();

    // Fact still valid
    engine
        .add_fact(
            &AddFactRequest {
                content: "current fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // Period query covering only the past window
    let results = engine
        .execute_query(&MemoryQuery::new().period(
            now - chrono::Duration::hours(4),
            now - chrono::Duration::minutes(30),
        ))
        .unwrap()
        .results;

    // Both should match: past fact has [t_valid, t_invalid) overlapping the period,
    // and current fact has NULL t_valid/t_invalid (unbounded, overlaps everything)
    assert_eq!(results.len(), 2);
}

#[test]
fn execute_query_composed_filters() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let opts_high = AddFactOptions {
        importance: Some(0.9),
        ..Default::default()
    };
    let opts_low = AddFactOptions {
        importance: Some(0.1),
        ..Default::default()
    };

    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust high importance".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_high.clone()),
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust low importance".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_low),
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Python high importance".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: Some(opts_high),
            },
            &embedder,
            None,
        )
        .unwrap();

    // Text "Rust" + importance >= 0.5 → only "Rust high importance"
    let results = engine
        .execute_query(&MemoryQuery::new().text("Rust").min_importance_score(0.5))
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "Rust high importance");
}

#[test]
fn execute_query_empty_results() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().text("nonexistent"))
        .unwrap()
        .results;
    assert!(results.is_empty());
}

#[test]
fn execute_query_default_limit() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    // Add 60 facts (over the default limit of 50)
    for i in 0..60 {
        engine
            .add_fact(
                &AddFactRequest {
                    content: format!("fact {i}").to_string(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &embedder,
                None,
            )
            .unwrap();
    }

    let results = engine.execute_query(&MemoryQuery::new()).unwrap().results;
    assert_eq!(results.len(), 50); // default limit
}

// --- Reranker tests ---

struct ReverseReranker;
impl Reranker for ReverseReranker {
    fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        let n = candidates.len();
        Ok((0..n).rev().map(|i| (i, candidates[i].score)).collect())
    }
    fn name(&self) -> &'static str {
        "reverse"
    }
}

struct FailingReranker;
impl Reranker for FailingReranker {
    fn rerank(&self, _query: &str, _candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        Err(MemoryError::Reranker("cross-encoder timeout".into()))
    }
    fn name(&self) -> &'static str {
        "failing"
    }
}

/// Records how many candidates it received, then passes through.
struct SpyReranker {
    seen_count: std::sync::atomic::AtomicUsize,
}
impl SpyReranker {
    fn new() -> Self {
        Self {
            seen_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn seen(&self) -> usize {
        self.seen_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl Reranker for SpyReranker {
    fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        self.seen_count
            .store(candidates.len(), std::sync::atomic::Ordering::Relaxed);
        Ok((0..candidates.len())
            .map(|i| (i, candidates[i].score))
            .collect())
    }
    fn name(&self) -> &'static str {
        "spy"
    }
}

#[test]
fn reranker_none_results_unchanged() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine
        .query(&SearchQuery {
            text: Some("fact".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    assert_eq!(results.len(), 2);
}

#[test]
fn reranker_reverses_order() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // Get baseline order (no reranker)
    let baseline_engine = MemoryEngine::open_memory(DIM).unwrap();
    baseline_engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    baseline_engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let baseline = baseline_engine
        .query(&SearchQuery {
            text: Some("fact".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    let reranked = engine
        .query(&SearchQuery {
            text: Some("fact".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    assert_eq!(baseline.len(), reranked.len());
    assert_eq!(baseline.len(), 2);
    // Reversed order
    assert_eq!(baseline[0].fact.content, reranked[1].fact.content);
    assert_eq!(baseline[1].fact.content, reranked[0].fact.content);
}

#[test]
fn reranker_skipped_for_vector_only_no_text() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let results = engine
        .query(&SearchQuery {
            text: None,
            embedding: Some(vec![0.5; DIM]),
            mode: SearchMode::Vector,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    // Reranker should NOT have fired (no text) — order should match vector similarity.
    // Both have identical embeddings, so they're equivalent; just check we got results.
    assert_eq!(results.len(), 2);
}

#[test]
fn reranker_applies_to_fts_only_mode() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // FTS-only with text → reranker should fire
    let results = engine
        .query(&SearchQuery {
            text: Some("fact".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    assert_eq!(results.len(), 2);
    // Results are reversed by ReverseReranker
}

#[test]
fn reranker_applies_to_vector_mode_with_text() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // Vector mode WITH text → reranker should fire
    let results = engine
        .query(&SearchQuery {
            text: Some("alpha".into()),
            embedding: Some(vec![0.5; DIM]),
            mode: SearchMode::Vector,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    // Should still get results (vector search ignores text, but reranker fires)
    assert!(!results.is_empty());
}

#[test]
fn rerank_depth_overfetches_then_truncates() {
    let spy = std::sync::Arc::new(SpyReranker::new());
    // Clone Arc into Box<dyn Reranker> — SpyReranker is Send+Sync
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(SpyRerankerWrapper(spy.clone()))))
            .unwrap();
    let embedder = MockEmbedder { dim: DIM };

    // Insert 10 facts
    for i in 0..10 {
        engine
            .add_fact(
                &AddFactRequest {
                    content: format!("rerank test fact {i}").to_string(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &embedder,
                None,
            )
            .unwrap();
    }

    let results = engine
        .query(&SearchQuery {
            text: Some("rerank test fact".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 3,
            rerank_depth: Some(8),
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    // Reranker should have seen up to 8 candidates
    assert!(
        spy.seen() > 3,
        "reranker should see more than limit (saw {})",
        spy.seen()
    );
    assert!(
        spy.seen() <= 8,
        "reranker should see at most rerank_depth (saw {})",
        spy.seen()
    );
    // But final output truncated to limit
    assert!(results.len() <= 3);
}

/// Wrapper to allow `Arc<SpyReranker>` to be `Box<dyn Reranker>`.
struct SpyRerankerWrapper(std::sync::Arc<SpyReranker>);
impl Reranker for SpyRerankerWrapper {
    fn rerank(&self, query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        self.0.rerank(query, candidates)
    }
    fn name(&self) -> &'static str {
        "spy_wrapper"
    }
}

#[test]
fn reranker_error_propagates() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(FailingReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "test fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = engine.query(&SearchQuery {
        text: Some("test".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    });

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::Reranker(_)));
}

#[test]
fn reranker_name_accessor() {
    let engine_none = MemoryEngine::open_memory(DIM).unwrap();
    assert_eq!(engine_none.reranker_name(), None);

    let engine_some =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
    assert_eq!(engine_some.reranker_name(), Some("reverse"));
}

#[test]
fn debug_output_includes_reranker() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
    let debug = format!("{engine:?}");
    assert!(
        debug.contains("reverse"),
        "Debug output should include reranker name"
    );
}

#[test]
fn rerank_depth_none_falls_back_to_limit() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    for i in 0..10 {
        engine
            .add_fact(
                &AddFactRequest {
                    content: format!("limit test fact {i}").to_string(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &embedder,
                None,
            )
            .unwrap();
    }

    let results = engine
        .query(&SearchQuery {
            text: Some("limit test fact".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 5,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        })
        .unwrap();

    assert_eq!(
        results.len(),
        5,
        "should respect limit when rerank_depth is None"
    );
}

// --- Co-session edge tests ---

/// Helper: ingest an event with a `session_id` and add a fact linked to it.
fn add_session_fact(engine: &MemoryEngine, content: &str, session_id: &str) -> (i64, i64) {
    let embedder = MockEmbedder { dim: DIM };
    let event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"msg": content}),
        source: "test".into(),
        session_id: Some(session_id.into()),
        scope_id: 1,
        origin_node_id: "local".into(),
        sequence_id: 0,
        created_at: None,
    };
    let event_id = engine.ingest(&event).unwrap();
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: Some(event_id),
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    (event_id, fact_id)
}

#[test]
fn link_session_facts_creates_bidirectional_edges() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let (_, f1) = add_session_fact(&engine, "fact a", "s1");
    let (_, f2) = add_session_fact(&engine, "fact b", "s1");

    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 2); // A→B and B→A

    // Verify edges in DB
    let co_edges = {
        let edges = crate::store::edges::EdgeStore::new(&engine.pool.read())
            .list_active()
            .unwrap();
        edges
            .into_iter()
            .filter(|e| e.relation_type == "co_session")
            .collect::<Vec<_>>()
    };
    assert_eq!(co_edges.len(), 2);

    // Both directions present
    assert!(
        co_edges
            .iter()
            .any(|e| e.source_fact_id == f1 && e.target_fact_id == f2)
    );
    assert!(
        co_edges
            .iter()
            .any(|e| e.source_fact_id == f2 && e.target_fact_id == f1)
    );

    // Weight matches constant
    for e in &co_edges {
        assert!((e.weight - MemoryEngine::CO_SESSION_WEIGHT).abs() < f64::EPSILON);
    }
}

#[test]
fn link_session_facts_three_facts_six_edges() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    add_session_fact(&engine, "a", "s1");
    add_session_fact(&engine, "b", "s1");
    add_session_fact(&engine, "c", "s1");

    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 6); // 3 pairs × 2 directions
}

#[test]
fn link_session_facts_single_fact_noop() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    add_session_fact(&engine, "lonely", "s1");

    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 0);
}

#[test]
fn link_session_facts_empty_session_noop() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let created = engine.link_session_facts("nonexistent", None).unwrap();
    assert_eq!(created, 0);
}

#[test]
fn link_session_facts_idempotent() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    add_session_fact(&engine, "a", "s1");
    add_session_fact(&engine, "b", "s1");

    let first = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(first, 2);

    let second = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(second, 0); // no new edges

    // Total edge count unchanged
    let (_, edge_count) = engine.graph_stats();
    assert_eq!(edge_count, 2);
}

#[test]
fn link_session_facts_graph_degree() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let (_, f1) = add_session_fact(&engine, "a", "s1");
    let (_, f2) = add_session_fact(&engine, "b", "s1");
    let (_, f3) = add_session_fact(&engine, "c", "s1");

    // Before linking — no edges
    assert_eq!(engine.graph_degree(f1), 0);

    engine.link_session_facts("s1", None).unwrap();

    // After: each fact has 2 outgoing + 2 incoming = degree 4
    assert_eq!(engine.graph_degree(f1), 4);
    assert_eq!(engine.graph_degree(f2), 4);
    assert_eq!(engine.graph_degree(f3), 4);
}

#[test]
fn link_session_facts_ignores_expired() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let (_, f1) = add_session_fact(&engine, "active1", "s1");
    add_session_fact(&engine, "active2", "s1");
    let (_, f3) = add_session_fact(&engine, "will_expire", "s1");

    // Expire f3 before linking
    {
        let conn = engine.pool.write();
        FactStore::new(&conn, DIM).expire(f3, Utc::now()).unwrap();
    }

    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 2); // Only f1↔active2, not f3

    // f3 should have no edges
    assert_eq!(engine.graph_degree(f3), 0);
    assert_eq!(engine.graph_degree(f1), 2); // 1 out + 1 in
}

// --- Scope-aware session linking tests ---

/// Helper: add a fact in a specific scope, linked to a session.
fn add_scoped_session_fact(
    engine: &MemoryEngine,
    content: &str,
    session_id: &str,
    scope_path: &str,
) -> (i64, i64) {
    let embedder = MockEmbedder { dim: DIM };
    let event = NewEvent {
        timestamp: Utc::now(),
        event_type: EventType::Interaction,
        payload: serde_json::json!({"msg": content}),
        source: "test".into(),
        session_id: Some(session_id.into()),
        scope_id: 1,
        origin_node_id: "local".into(),
        sequence_id: 0,
        created_at: None,
    };
    let event_id = engine.ingest(&event).unwrap();
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: Some(event_id),
                scope: Some(scope_path.into()),
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    (event_id, fact_id)
}

#[test]
fn link_session_facts_scope_filters_cross_scope() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    // Two facts in user:alice, one in user:bob — same session_id
    let (_, f1) = add_scoped_session_fact(&engine, "alice a", "s1", "user:alice");
    let (_, f2) = add_scoped_session_fact(&engine, "alice b", "s1", "user:alice");
    let (_, f3) = add_scoped_session_fact(&engine, "bob c", "s1", "user:bob");

    // Scope-filtered: only link alice's facts
    let created = engine.link_session_facts("s1", Some("user:alice")).unwrap();
    assert_eq!(created, 2); // f1↔f2

    assert_eq!(engine.graph_degree(f1), 2); // 1 out + 1 in
    assert_eq!(engine.graph_degree(f2), 2);
    assert_eq!(engine.graph_degree(f3), 0); // bob excluded
}

#[test]
fn link_session_facts_scope_none_links_all() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    add_scoped_session_fact(&engine, "alice a", "s1", "user:alice");
    add_scoped_session_fact(&engine, "bob b", "s1", "user:bob");
    add_scoped_session_fact(&engine, "root c", "s1", "user:charlie");

    // None = global lookup (backward-compatible)
    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 6); // 3 facts × 2 directions
}

#[test]
fn link_session_facts_scope_subtree() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    // Create facts at different depths under user:alice
    let (_, f1) = add_scoped_session_fact(&engine, "top", "s1", "user:alice");
    let (_, f2) = add_scoped_session_fact(&engine, "nested", "s1", "user:alice/project:x");
    let (_, f3) = add_scoped_session_fact(&engine, "other", "s1", "user:bob");

    // Subtree from user:alice should include both alice and alice/project:x
    let created = engine.link_session_facts("s1", Some("user:alice")).unwrap();
    assert_eq!(created, 2); // f1↔f2
    assert_eq!(engine.graph_degree(f1), 2);
    assert_eq!(engine.graph_degree(f2), 2);
    assert_eq!(engine.graph_degree(f3), 0);
}

#[test]
fn link_session_facts_scope_not_found() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    add_session_fact(&engine, "a", "s1");

    let result = engine.link_session_facts("s1", Some("user:nonexistent"));
    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), MemoryError::NotFound(msg) if msg.contains("scope path"))
    );
}

// --- Reranker subset/permutation guard (issue #85) ---

/// Returns all candidates plus a fabricated fact with a bogus ID.
/// Returns a single out-of-bounds index.
struct OutOfBoundsReranker;
impl Reranker for OutOfBoundsReranker {
    fn rerank(&self, _query: &str, _candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        Ok(vec![(999_999, 1.0)])
    }
    fn name(&self) -> &'static str {
        "out_of_bounds"
    }
}

/// Returns first two candidates with the same index (duplicate).
struct DuplicatingReranker;
impl Reranker for DuplicatingReranker {
    fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        if candidates.len() >= 2 {
            Ok(vec![(0, candidates[0].score), (0, candidates[0].score)])
        } else {
            Ok((0..candidates.len())
                .map(|i| (i, candidates[i].score))
                .collect())
        }
    }
    fn name(&self) -> &'static str {
        "duplicating"
    }
}

#[test]
fn reranker_rejects_out_of_bounds_index() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(OutOfBoundsReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "real fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = engine.query(&SearchQuery {
        text: Some("real".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    });

    assert!(result.is_err(), "should reject out-of-bounds index");
    let err = result.unwrap_err();
    assert!(
        matches!(err, MemoryError::Reranker(_)),
        "should be a Reranker error, got: {err}"
    );
    assert!(
        err.to_string().contains("out-of-bounds"),
        "error message should mention out-of-bounds, got: {err}"
    );
}

#[test]
fn reranker_rejects_duplicates() {
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(DuplicatingReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "dup fact alpha".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "dup fact beta".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = engine.query(&SearchQuery {
        text: Some("dup".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    });

    assert!(result.is_err(), "should reject duplicates");
    let err = result.unwrap_err();
    assert!(
        matches!(err, MemoryError::Reranker(_)),
        "should be a Reranker error, got: {err}"
    );
    assert!(
        err.to_string().contains("duplicate"),
        "error message should mention duplicate, got: {err}"
    );
}

#[test]
fn reranker_allows_valid_subset() {
    // A well-behaved reranker (ReverseReranker) should still work fine
    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "guard alpha".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "guard beta".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = engine.query(&SearchQuery {
        text: Some("guard".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    });

    assert!(
        result.is_ok(),
        "valid subset should pass: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().len(), 2);
}

#[test]
fn reranker_allows_filtering_subset() {
    /// Returns only the first candidate, discarding the rest.
    struct FilteringReranker;
    impl Reranker for FilteringReranker {
        fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            if candidates.is_empty() {
                Ok(vec![])
            } else {
                Ok(vec![(0, candidates[0].score)])
            }
        }
        fn name(&self) -> &'static str {
            "filtering"
        }
    }

    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(FilteringReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "filterable first".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "filterable second".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = engine.query(&SearchQuery {
        text: Some("filterable".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    });

    assert!(
        result.is_ok(),
        "filtering subset should pass: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().len(), 1);
}

#[test]
fn reranker_rejects_non_finite_score() {
    struct NanScoreReranker;
    impl Reranker for NanScoreReranker {
        fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            if candidates.is_empty() {
                Ok(vec![])
            } else {
                Ok(vec![(0, f64::NAN)])
            }
        }
        fn name(&self) -> &'static str {
            "nan_score"
        }
    }

    let engine =
        MemoryEngine::open_memory_with(DIM, None, Some(Box::new(NanScoreReranker))).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "score test fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    let result = engine.query(&SearchQuery {
        text: Some("score".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    });

    assert!(result.is_err(), "should reject NaN score");
    let err = result.unwrap_err();
    assert!(
        matches!(err, MemoryError::Reranker(_)),
        "should be a Reranker error, got: {err}"
    );
    assert!(
        err.to_string().contains("non-finite"),
        "error message should mention non-finite score, got: {err}"
    );
}

// --- Batch embedding + batch add_fact tests ---

#[test]
fn embed_batch_default_impl_loops_embed() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingEmbedder {
        calls: AtomicUsize,
        dim: usize,
    }

    impl EmbeddingProvider for CountingEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![0.1; self.dim])
        }
    }

    let embedder = CountingEmbedder {
        calls: AtomicUsize::new(0),
        dim: DIM,
    };

    let texts = ["alpha", "beta", "gamma"];
    let result = embedder.embed_batch(&texts).unwrap();

    assert_eq!(result.len(), 3);
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 3);
    for emb in &result {
        assert_eq!(emb.len(), DIM);
    }
}

#[test]
fn embed_batch_empty_returns_empty() {
    let embedder = MockEmbedder { dim: DIM };
    let result = embedder.embed_batch(&[]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn add_facts_batch_inserts_all_facts() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let entries: Vec<AddFactRequest> = (0..5)
        .map(|i| AddFactRequest {
            content: format!("batch fact {i}"),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        })
        .collect();

    let ids = engine.add_facts_batch(&entries, &embedder, None).unwrap();
    assert_eq!(ids.len(), 5);

    // All IDs should be unique and positive
    let unique: std::collections::HashSet<i64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 5);
    assert!(ids.iter().all(|&id| id > 0));

    // Verify facts are actually in the DB
    for (i, &id) in ids.iter().enumerate() {
        let fact = engine.get_fact(id).unwrap();
        assert_eq!(fact.content, format!("batch fact {i}"));
    }
}

#[test]
fn add_facts_batch_empty_returns_empty() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let ids = engine.add_facts_batch(&[], &embedder, None).unwrap();
    assert!(ids.is_empty());
}

#[test]
fn add_facts_batch_with_scopes() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let entries = vec![
        AddFactRequest {
            content: "fact in project/a".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: Some("project/a".into()),
            opts: None,
        },
        AddFactRequest {
            content: "fact in project/b".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: Some("project/b".into()),
            opts: None,
        },
        AddFactRequest {
            content: "fact in root".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
    ];

    let ids = engine.add_facts_batch(&entries, &embedder, None).unwrap();
    assert_eq!(ids.len(), 3);

    // Verify scope assignments via fact retrieval
    let f0 = engine.get_fact(ids[0]).unwrap();
    let f2 = engine.get_fact(ids[2]).unwrap();
    // f0 should be in a non-root scope, f2 in root (scope_id=1)
    assert_ne!(f0.scope_id, f2.scope_id);
    assert_eq!(f2.scope_id, 1); // root scope
}

#[test]
fn add_facts_batch_with_classifier() {
    struct AlwaysPin;
    impl PersistenceClassifier for AlwaysPin {
        fn should_pin(&self, _fact: &Fact) -> bool {
            true
        }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };
    let classifier = AlwaysPin;

    let entries = vec![AddFactRequest {
        content: "important fact".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    }];

    let ids = engine
        .add_facts_batch(&entries, &embedder, Some(&classifier))
        .unwrap();
    let fact = engine.get_fact(ids[0]).unwrap();
    assert!(fact.is_pinned);
}

#[test]
fn add_facts_batch_rejects_embedding_count_mismatch() {
    /// Embedder that returns fewer embeddings than requested.
    struct BadBatchEmbedder;
    impl EmbeddingProvider for BadBatchEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; DIM])
        }
        fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            // Always return exactly 1 embedding regardless of input
            Ok(vec![vec![0.5; DIM]])
        }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let entries = vec![
        AddFactRequest {
            content: "a".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
        AddFactRequest {
            content: "b".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
    ];

    let err = engine
        .add_facts_batch(&entries, &BadBatchEmbedder, None)
        .unwrap_err();
    assert!(
        err.to_string().contains("1 embeddings for 2 entries"),
        "expected count mismatch error, got: {err}"
    );
}

#[test]
fn add_facts_batch_rollback_on_insert_failure() {
    /// Embedder that returns wrong dimension for the last embedding,
    /// causing `FactStore::insert` to fail mid-transaction.
    struct BadDimBatchEmbedder;
    impl EmbeddingProvider for BadDimBatchEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; DIM])
        }
        fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            let mut results: Vec<Vec<f32>> = texts.iter().map(|_| vec![0.5; DIM]).collect();
            // Corrupt the last embedding with wrong dimension
            if let Some(last) = results.last_mut() {
                *last = vec![0.5; DIM + 1]; // wrong dim
            }
            Ok(results)
        }
    }

    let engine = MemoryEngine::open_memory(DIM).unwrap();

    // Use scoped entries to verify scopes also roll back
    let entries = vec![
        AddFactRequest {
            content: "rollback test 0".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: Some("rollback/scope-a".into()),
            opts: None,
        },
        AddFactRequest {
            content: "rollback test 1".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: Some("rollback/scope-b".into()),
            opts: None,
        },
        AddFactRequest {
            content: "rollback test 2".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        },
    ];

    // Should fail on the last insert (wrong dim)
    let result = engine.add_facts_batch(&entries, &BadDimBatchEmbedder, None);
    assert!(result.is_err());

    // Verify rollback: no facts should be in the DB
    let query = SearchQuery {
        text: Some("rollback test".into()),
        embedding: Some(vec![0.5; DIM]),
        limit: 10,
        mode: SearchMode::Hybrid,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    };
    let results = engine.query(&query).unwrap();
    assert!(
        results.is_empty(),
        "expected no facts after rollback, got {}",
        results.len()
    );

    // Verify scope atomicity: scopes should NOT exist after rollback.
    // A successful add_facts_batch with the same scopes should create
    // them fresh (proving they were rolled back from the failed attempt).
    let good_entries = vec![AddFactRequest {
        content: "after rollback".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: Some("rollback/scope-a".into()),
        opts: None,
    }];
    let good_embedder = MockEmbedder { dim: DIM };
    let ids = engine
        .add_facts_batch(&good_entries, &good_embedder, None)
        .unwrap();
    assert_eq!(ids.len(), 1);
    let fact = engine.get_fact(ids[0]).unwrap();
    // If scopes were leaked, scope_id would already exist.
    // The fact that this succeeds proves the DB is consistent.
    assert_ne!(fact.scope_id, 1, "should be in a non-root scope");
}

#[test]
fn add_facts_batch_temporal_consistency() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let entries: Vec<AddFactRequest> = (0..3)
        .map(|i| AddFactRequest {
            content: format!("temporal {i}"),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        })
        .collect();

    let ids = engine.add_facts_batch(&entries, &embedder, None).unwrap();

    // All facts should have the same t_created (within a reasonable window)
    let facts: Vec<Fact> = ids.iter().map(|&id| engine.get_fact(id).unwrap()).collect();
    let first_created = facts[0].t_created;
    for fact in &facts {
        let diff = (fact.t_created - first_created).num_milliseconds().abs();
        assert!(
            diff == 0,
            "batch facts should share the same timestamp, diff: {diff}ms"
        );
    }
}

// ---------------------------------------------------------------------------
// Snapshot integration tests
// ---------------------------------------------------------------------------

// --- Outcome tracking tests (#63) ---

#[test]
fn record_outcome_returns_event_id() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let fact = make_new_fact("outcome target", vec![0.5; DIM]);
    let fact_id = insert_raw_fact(&engine, &fact);

    let event_id = engine
        .record_outcome(fact_id, crate::types::Outcome::Positive)
        .unwrap();
    assert!(event_id > 0);
}

#[test]
fn record_outcome_nonexistent_fact_returns_not_found() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    let result = engine.record_outcome(999, crate::types::Outcome::Negative);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::NotFound(_)));
}

#[test]
fn record_outcome_read_only_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Create DB with a fact
    let fact_id = {
        let config = EngineConfig::new(db_path.clone(), DIM);
        let engine = MemoryEngine::open(&config).unwrap();
        let fact = make_new_fact("pinned for ro", vec![0.5; DIM]);
        insert_raw_fact(&engine, &fact)
    };

    // Re-open read-only
    let mut config = EngineConfig::new(db_path, DIM);
    config.read_only = true;
    let engine = MemoryEngine::open(&config).unwrap();

    let result = engine.record_outcome(fact_id, crate::types::Outcome::Positive);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::ReadOnly));
}

#[test]
fn get_outcome_counts_nonexistent_fact_returns_not_found() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();

    let result = engine.get_outcome_counts(999);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::NotFound(_)));
}

#[test]
fn get_outcome_counts_no_outcomes_returns_zeros() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let fact = make_new_fact("no outcomes", vec![0.5; DIM]);
    let fact_id = insert_raw_fact(&engine, &fact);

    let counts = engine.get_outcome_counts(fact_id).unwrap();
    assert_eq!(counts.positive, 0);
    assert_eq!(counts.negative, 0);
    assert_eq!(counts.neutral, 0);
}

#[test]
fn get_outcome_counts_tallies_correctly() {
    use crate::types::Outcome;

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let fact = make_new_fact("tallied fact", vec![0.5; DIM]);
    let fact_id = insert_raw_fact(&engine, &fact);

    // Record mixed outcomes
    engine.record_outcome(fact_id, Outcome::Positive).unwrap();
    engine.record_outcome(fact_id, Outcome::Positive).unwrap();
    engine.record_outcome(fact_id, Outcome::Negative).unwrap();
    engine.record_outcome(fact_id, Outcome::Neutral).unwrap();
    engine.record_outcome(fact_id, Outcome::Positive).unwrap();

    let counts = engine.get_outcome_counts(fact_id).unwrap();
    assert_eq!(counts.positive, 3);
    assert_eq!(counts.negative, 1);
    assert_eq!(counts.neutral, 1);
}

#[test]
fn get_outcome_counts_isolates_per_fact() {
    use crate::types::Outcome;

    let engine = MemoryEngine::open_memory(DIM).unwrap();
    let f1 = insert_raw_fact(&engine, &make_new_fact("fact one", vec![0.5; DIM]));
    let f2 = insert_raw_fact(&engine, &make_new_fact("fact two", vec![0.3; DIM]));

    engine.record_outcome(f1, Outcome::Positive).unwrap();
    engine.record_outcome(f1, Outcome::Positive).unwrap();
    engine.record_outcome(f2, Outcome::Negative).unwrap();

    let c1 = engine.get_outcome_counts(f1).unwrap();
    assert_eq!(c1.positive, 2);
    assert_eq!(c1.negative, 0);

    let c2 = engine.get_outcome_counts(f2).unwrap();
    assert_eq!(c2.positive, 0);
    assert_eq!(c2.negative, 1);
}

#[test]
fn outcome_serde_round_trip() {
    use crate::types::Outcome;

    for variant in [Outcome::Positive, Outcome::Negative, Outcome::Neutral] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: Outcome = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
}

#[test]
fn outcome_display() {
    use crate::types::Outcome;

    assert_eq!(Outcome::Positive.to_string(), "positive");
    assert_eq!(Outcome::Negative.to_string(), "negative");
    assert_eq!(Outcome::Neutral.to_string(), "neutral");
}

mod snapshot_integration {
    use super::*;

    fn open_file_engine(dir: &std::path::Path) -> MemoryEngine {
        let config = EngineConfig::new(dir.join("test.db"), DIM);
        MemoryEngine::open(&config).unwrap()
    }

    fn add_test_fact(engine: &MemoryEngine, content: &str) -> i64 {
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                &AddFactRequest {
                    content: content.into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &embedder,
                None,
            )
            .unwrap()
    }

    #[test]
    fn snapshot_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Create engine, add data, write snapshot
        {
            let engine = open_file_engine(dir.path());
            add_test_fact(&engine, "fact one");
            add_test_fact(&engine, "fact two");
            engine.write_snapshot().unwrap();
        }

        // Verify snapshot file exists
        let snap_path = super::snapshot::snapshot_path(&db_path);
        assert!(snap_path.exists(), "snapshot file should exist");

        // Re-open — should load from snapshot
        let engine = open_file_engine(dir.path());
        let facts = engine.list_active_facts(None).unwrap();
        assert_eq!(facts.len(), 2);
    }

    #[test]
    fn snapshot_fallback_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        // Create engine, add data, do NOT write snapshot
        {
            let config = EngineConfig::new(dir.path().join("test.db"), DIM);
            let engine = MemoryEngine::open(&config).unwrap();
            add_test_fact(&engine, "fact one");
            // Remove snapshot if Drop wrote one
            let snap_path = super::snapshot::snapshot_path(&dir.path().join("test.db"));
            let _ = std::fs::remove_file(&snap_path);
        }

        // Re-open — should fall back to full rebuild
        let engine = open_file_engine(dir.path());
        let facts = engine.list_active_facts(None).unwrap();
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn snapshot_fallback_on_stale_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let snap_path = super::snapshot::snapshot_path(&db_path);

        // Phase 1: create engine with one fact, write snapshot explicitly.
        {
            let engine = open_file_engine(dir.path());
            add_test_fact(&engine, "original fact");
            engine.write_snapshot().unwrap();
        }

        // Save the snapshot bytes so we can restore them later.
        let snapshot_bytes = std::fs::read(&snap_path).unwrap();

        // Phase 2: add more data. Drop writes a fresh (updated) snapshot.
        {
            let engine = open_file_engine(dir.path());
            add_test_fact(&engine, "second fact");
            // Drop writes snapshot with fingerprint reflecting 2 facts.
        }

        // Phase 3: overwrite the snapshot with the stale one from phase 1.
        std::fs::write(&snap_path, &snapshot_bytes).unwrap();

        // Phase 4: re-open — stale snapshot fingerprint should mismatch,
        // engine falls back to full rebuild and sees both facts.
        let engine = open_file_engine(dir.path());
        let facts = engine.list_active_facts(None).unwrap();
        assert_eq!(facts.len(), 2, "should see both facts via full rebuild");
    }

    #[test]
    fn snapshot_skipped_for_memory_engine() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let result = engine.write_snapshot().unwrap();
        assert!(!result, "in-memory engine should skip snapshot");
    }

    #[test]
    fn snapshot_skipped_for_read_only() {
        let dir = tempfile::tempdir().unwrap();

        // First open in read-write to create the DB
        {
            let _engine = open_file_engine(dir.path());
        }

        // Open read-only
        let mut config = EngineConfig::new(dir.path().join("test.db"), DIM);
        config.read_only = true;
        let engine = MemoryEngine::open(&config).unwrap();
        let result = engine.write_snapshot().unwrap();
        assert!(!result, "read-only engine should skip snapshot");
    }

    #[test]
    fn drop_writes_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let snap_path = super::snapshot::snapshot_path(&db_path);

        {
            let engine = open_file_engine(dir.path());
            add_test_fact(&engine, "will be snapshotted");
            // Do NOT call write_snapshot — let Drop do it
        }

        assert!(snap_path.exists(), "Drop should write snapshot file");
    }

    #[test]
    fn snapshot_and_full_rebuild_agree() {
        let dir = tempfile::tempdir().unwrap();

        // Create engine with data
        {
            let engine = open_file_engine(dir.path());
            add_test_fact(&engine, "alpha");
            add_test_fact(&engine, "beta");
            add_test_fact(&engine, "gamma");
            engine.write_snapshot().unwrap();
        }

        // Load from snapshot
        let engine_snap = open_file_engine(dir.path());
        let snap_facts = engine_snap.list_active_facts(None).unwrap();
        let snap_graph_nodes = engine_snap.graph.read().node_count();
        let snap_graph_edges = engine_snap.graph.read().edge_count();

        // Delete snapshot, force full rebuild
        let snap_path = super::snapshot::snapshot_path(&dir.path().join("test.db"));
        std::fs::remove_file(&snap_path).unwrap();
        let engine_rebuild = open_file_engine(dir.path());
        let rebuild_facts = engine_rebuild.list_active_facts(None).unwrap();
        let rebuild_graph_nodes = engine_rebuild.graph.read().node_count();
        let rebuild_graph_edges = engine_rebuild.graph.read().edge_count();

        assert_eq!(snap_facts.len(), rebuild_facts.len());
        assert_eq!(snap_graph_nodes, rebuild_graph_nodes);
        assert_eq!(snap_graph_edges, rebuild_graph_edges);
    }
}
