use super::*;
use crate::resume::context::ResumeConfig;
use crate::search::hybrid::{SearchMode, SearchQuery};
use crate::search::query::MemoryQuery;
use crate::traits::{
    ConflictArbiter, ConsolidationConfig, CrudDecision, EmbeddingProvider, ForgetPolicy,
    PersistenceClassifier, SummaryGenerator,
};
use crate::types::{
    AddFactOptions, AddFactRequest, EmbeddingFingerprint, EventType, Fact, FactType, NewEvent,
    NewFact,
};

const DIM: usize = 4;

struct MockEmbedder {
    dim: usize,
}

impl EmbeddingProvider for MockEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.5; self.dim])
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", self.dim)
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
    NewFact::builder(content, embedding, FactType::Semantic)
        .content_hash(blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string())
        .build()
}

/// Test helper: insert a raw fact via the write connection (bypasses engine's `add_fact`).
fn insert_raw_fact(engine: &MemoryEngine, fact: &NewFact) -> i64 {
    let conn = engine.pool.write();
    FactStore::new(&conn, engine.embed_dim)
        .insert(fact)
        .unwrap()
}

// --- Phase 1 tests ---

/// The L10 size bound also guards `resolve_conflict`, which persists the
/// candidate `NewFact` verbatim on an Add/Update decision. The check runs
/// before the arbiter and the old-fact lookup, so a non-existent `old_id`
/// still surfaces `PayloadTooLarge` rather than `NotFound`.
#[test]
fn resolve_conflict_rejects_oversized_fact() {
    use crate::error::{ConflictError, MemoryError};

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let arbiter = FixedArbiter {
        decision: CrudDecision::Add,
    };
    let mut oversized = make_new_fact("seed", vec![0.5; DIM]);
    oversized.content = "x".repeat(crate::limits::MAX_PAYLOAD_BYTES + 1);

    let err = engine
        .resolve_conflict(&arbiter, 9999, &oversized)
        .unwrap_err();
    assert!(matches!(
        err,
        MemoryError::Conflict(ConflictError::PayloadTooLarge {
            kind: "fact content",
            ..
        })
    ));
}

#[test]
fn open_memory_succeeds() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine.embed_dim(), DIM);
}

#[test]
fn ingest_returns_event_id() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    // First open with dim=768 and write a fact: the embedding identity (incl. dim)
    // is recorded on the FIRST embedding write (#613, ADR 0015 §2), not at open.
    {
        let engine = MemoryEngine::builder(768)
            .path(db_path.clone())
            .build()
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "seed".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &MockEmbedder { dim: 768 },
                None,
            )
            .unwrap();
    }

    // Second open with dim=384 should fail (recorded identity dim=768 != 384).
    let err = MemoryEngine::builder(384)
        .path(db_path)
        .build()
        .unwrap_err();
    assert!(matches!(err, MemoryError::Migration(_)));
    assert!(err.to_string().contains("mismatch"));
}

#[test]
fn first_add_fact_records_embedding_meta() {
    // A second embedder with a DIFFERENT fingerprint, to prove mismatch rejection below.
    struct OtherEmbedder {
        dim: usize,
    }
    impl EmbeddingProvider for OtherEmbedder {
        fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![0.7; self.dim])
        }
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("other-model", "other-provider", self.dim)
        }
    }

    // The embedding identity is established on the FIRST embedding write (#613,
    // ADR 0015 §2); a later differing fingerprint is rejected, not overwritten (#614).
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap()
            .is_none(),
        "no identity before any write"
    );

    let expected = MockEmbedder { dim: DIM }.fingerprint();
    let req = |c: &str| AddFactRequest {
        content: c.into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    };
    engine
        .add_fact(&req("a"), &MockEmbedder { dim: DIM }, None)
        .unwrap();
    assert_eq!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap(),
        Some(expected.clone()),
        "first write records the embedder's fingerprint"
    );

    // #614 enforcement: a second add with a DIFFERENT fingerprint is hard-rejected
    // (not silently ignored), and the stored identity is left untouched.
    let err = engine
        .add_fact(&req("b"), &OtherEmbedder { dim: DIM }, None)
        .unwrap_err();
    assert!(
        matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
        "a differing later fingerprint must be rejected, got {err:?}"
    );
    assert_eq!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap(),
        Some(expected),
        "stored identity is unchanged after a rejected mismatched write"
    );
}

#[test]
fn verify_embedding_identity_enforces_match() {
    // The eager fail-fast check (#614, §Design.2) consumed by MCP startup.
    struct OtherEmbedder {
        dim: usize,
    }
    impl EmbeddingProvider for OtherEmbedder {
        fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![0.7; self.dim])
        }
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("other-model", "other-provider", self.dim)
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    // Fresh store has no identity yet -> any provider is compatible.
    engine
        .verify_embedding_identity(&MockEmbedder { dim: DIM })
        .expect("fresh store compatible with any provider");

    // Stamp the identity via a real embedding write.
    let req = AddFactRequest {
        content: "a".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    };
    engine
        .add_fact(&req, &MockEmbedder { dim: DIM }, None)
        .unwrap();

    // Matching provider -> Ok; differing provider -> EmbeddingModelMismatch.
    engine
        .verify_embedding_identity(&MockEmbedder { dim: DIM })
        .expect("matching provider passes the eager check");
    let err = engine
        .verify_embedding_identity(&OtherEmbedder { dim: DIM })
        .expect_err("differing provider must fail the eager check");
    assert!(
        matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
        "expected EmbeddingModelMismatch, got {err:?}"
    );
}

#[test]
fn add_fact_precomputed_requires_present_identity() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let req = AddFactRequest {
        content: "p".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    };

    // Fresh store: a pre-computed write cannot establish identity -> rejected
    // (consistent with promote / cycle AddFact). The error is the require_present guard.
    let err = engine
        .add_fact_precomputed(&req, vec![0.5; DIM], None)
        .expect_err("precomputed add on a fresh store must require an identity");
    assert!(
        matches!(err, MemoryError::Internal(_)),
        "expected require_present Internal error, got {err:?}"
    );

    // Stamp the identity via a real embedder, then a pre-computed add succeeds — with NO
    // model comparison, so the passthrough-style sentinel can't trigger a false mismatch
    // (the #614 regression). This is the documented memory_add_fact precomputed workflow.
    engine
        .add_fact(&req, &MockEmbedder { dim: DIM }, None)
        .unwrap();
    let id = engine
        .add_fact_precomputed(&req, vec![0.6; DIM], None)
        .expect("precomputed add into a stamped store succeeds");
    assert!(id > 0);

    // Dimension is still enforced on the pre-computed vector.
    let dim_err = engine
        .add_fact_precomputed(&req, vec![0.6; DIM + 3], None)
        .expect_err("wrong-dimension precomputed vector must be rejected");
    assert!(
        matches!(dim_err, MemoryError::EmbeddingDimension { .. }),
        "expected EmbeddingDimension, got {dim_err:?}"
    );
}

#[test]
fn noop_bootstrap_does_not_stamp_identity() {
    // #643: bootstrapping a session that creates zero facts (here an empty reader)
    // must NOT record the embedding identity. Previously the engine stamped before
    // the inner import ran, so a no-op bootstrap permanently fixed the identity even
    // though no vector was written.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let report = engine
        .bootstrap_session(
            std::io::Cursor::new(""),
            &MockEmbedder { dim: DIM },
            &crate::KeywordExtractor,
            &crate::BootstrapConfig::default(),
            None,
        )
        .unwrap();
    assert_eq!(report.facts_created, 0, "empty session creates no facts");
    assert!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap()
            .is_none(),
        "a fact-less bootstrap must not stamp the embedding identity"
    );
}

#[test]
fn noop_bootstrap_then_real_write_records_real_embedder() {
    // #643 (the #614-era harm): a no-op bootstrap with embedder A must not shadow a
    // later real first write with a different embedder B — B is the true identity.
    struct EmbedderB {
        dim: usize,
    }
    impl EmbeddingProvider for EmbedderB {
        fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![0.7; self.dim])
        }
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("model-b", "provider-b", self.dim)
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    // No-op bootstrap with embedder A (MockEmbedder): zero facts ⇒ no stamp.
    engine
        .bootstrap_session(
            std::io::Cursor::new(""),
            &MockEmbedder { dim: DIM },
            &crate::KeywordExtractor,
            &crate::BootstrapConfig::default(),
            None,
        )
        .unwrap();
    // The store must be left UNSTAMPED by the no-op run (the crux of #643): this is
    // what lets the first real writer below establish the identity.
    assert!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap()
            .is_none(),
        "no-op bootstrap must leave the store unstamped"
    );
    // First real write with embedder B establishes the identity.
    let req = AddFactRequest {
        content: "real".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    };
    engine
        .add_fact(&req, &EmbedderB { dim: DIM }, None)
        .unwrap();
    assert_eq!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap(),
        Some(EmbedderB { dim: DIM }.fingerprint()),
        "the real first writer's identity must win, not the no-op bootstrap's"
    );
}

#[test]
fn bootstrap_creating_facts_stamps_identity() {
    // Positive guard: a bootstrap that DOES create facts records the embedder
    // identity (atomically, inside the session savepoint).
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let fixture = include_str!("../../tests/fixtures/success_session.jsonl");
    let report = engine
        .bootstrap_session(
            std::io::Cursor::new(fixture),
            &MockEmbedder { dim: DIM },
            &crate::KeywordExtractor,
            &crate::BootstrapConfig::default(),
            None,
        )
        .unwrap();
    assert!(report.facts_created > 0, "fixture should create facts");
    assert_eq!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap(),
        Some(MockEmbedder { dim: DIM }.fingerprint()),
        "a fact-creating bootstrap records the embedder's fingerprint"
    );
}

#[test]
fn noop_bootstrap_directory_does_not_stamp_identity() {
    // #643 names all three bootstrap wrappers. `bootstrap_directory` stamps each
    // session inside its own savepoint; an empty directory processes no session, so
    // the store must be left unstamped.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let report = engine
        .bootstrap_directory(
            dir.path(),
            &MockEmbedder { dim: DIM },
            &crate::KeywordExtractor,
            &crate::BootstrapConfig::default(),
            None,
        )
        .unwrap();
    assert_eq!(report.facts_created, 0, "empty directory creates no facts");
    assert!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap()
            .is_none(),
        "a fact-less directory bootstrap must not stamp the embedding identity"
    );
}

#[test]
fn bootstrap_directory_creating_facts_stamps_identity() {
    // Positive guard for the multi-file path: a directory with a fact-producing
    // session records the embedder identity (inside that session's savepoint).
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.jsonl"),
        include_str!("../../tests/fixtures/success_session.jsonl"),
    )
    .unwrap();
    let report = engine
        .bootstrap_directory(
            dir.path(),
            &MockEmbedder { dim: DIM },
            &crate::KeywordExtractor,
            &crate::BootstrapConfig::default(),
            None,
        )
        .unwrap();
    assert!(report.facts_created > 0, "fixture should create facts");
    assert_eq!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap(),
        Some(MockEmbedder { dim: DIM }.fingerprint()),
        "a fact-creating directory bootstrap records the embedder's fingerprint"
    );
}

#[test]
fn memory_directory_stamps_identity_even_when_empty() {
    // The deliberately RETAINED meta-first path (#643): `bootstrap_memory_directory`
    // is autocommit-per-file, so it stamps BEFORE the first file for crash safety.
    // An empty directory therefore still stamps — the harmless no-op stamp the other
    // paths shed, kept here as the crash-safe choice. Pinning it guards against a
    // future refactor that "consistently" defers this path and reopens the
    // orphan-vector window.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    engine
        .bootstrap_memory_directory(
            dir.path(),
            &MockEmbedder { dim: DIM },
            &crate::BootstrapConfig::default(),
            None,
        )
        .unwrap();
    assert_eq!(
        engine
            .with_read(crate::store::embedding_meta::load)
            .unwrap(),
        Some(MockEmbedder { dim: DIM }.fingerprint()),
        "memory-directory import is meta-first: it stamps even with no files"
    );
}

#[test]
fn read_only_open_of_unstamped_db_is_ok() {
    // D6 behavior change: previously a read-only open of a DB with no persisted
    // dim errored ("open read-write first"). Now identity is written on first
    // embed, so read-only-opening an un-embedded store is Ok — there is nothing to
    // validate, and the runtime dim always comes from EngineConfig.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ro_unstamped.db");
    {
        // Open read-write, never write a fact ⇒ no embedding_meta recorded.
        let _e = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    }
    let engine = MemoryEngine::builder(DIM)
        .path(&path)
        .read_only(true)
        .build()
        .unwrap();
    assert!(engine.pool.is_read_only());
}

#[test]
fn get_set_config() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine.graph_stats(), (0, 0));
}

#[test]
fn consolidate_deduplicates_similar_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let stats = engine
        .consolidate(&MockGen, &MockEmbedder { dim: DIM }, &config)
        .unwrap();
    assert_eq!(stats.duplicates_removed, 1);

    let active = engine.list_active_facts(None).unwrap();
    assert_eq!(active.len(), 1);
}

#[test]
fn consolidate_is_idempotent() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let _stats1 = engine
        .consolidate(&MockGen, &MockEmbedder { dim: DIM }, &config)
        .unwrap();
    let stats2 = engine
        .consolidate(&MockGen, &MockEmbedder { dim: DIM }, &config)
        .unwrap();

    // Second run should find 0 new duplicates
    assert_eq!(stats2.duplicates_removed, 0);
    // Both facts still active
    assert_eq!(engine.list_active_facts(None).unwrap().len(), 2);
}

#[test]
fn forget_prunes_stale_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    // Insert a fact with very low importance
    let now = Utc::now();
    let old_time = now - chrono::Duration::days(200);
    insert_raw_fact(
        &engine,
        &NewFact::builder("ancient fact", vec![0.1; DIM], FactType::Episodic)
            .content_hash("h_ancient")
            .t_created(old_time)
            .last_accessed(old_time)
            .importance(0.01)
            .build(),
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let policy = ForgetPolicy {
        half_life_days: 0.0, // invalid
        ..ForgetPolicy::default()
    };
    assert!(engine.forget(&policy).is_err());
}

#[test]
fn resolve_conflict_update_creates_edge() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
        let engine = MemoryEngine::open_from_config(&config, None).unwrap();
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
        let engine = MemoryEngine::open_from_config(&config, None).unwrap();
        assert_eq!(engine.graph_stats().1, 1);
    }
}

#[test]
fn list_summaries_empty() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let summaries = engine.list_summaries(&ConsolidationLevel::Global).unwrap();
    assert!(summaries.is_empty());
}

// --- Phase 3 / T2: AddFactOptions ---

#[test]
fn add_fact_with_custom_importance() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let engine = std::sync::Arc::new(MemoryEngine::builder(DIM).path(db_path).build().unwrap());
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

    let engine = std::sync::Arc::new(MemoryEngine::builder(DIM).path(db_path).build().unwrap());

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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let ctx = engine.resume_context(&ResumeConfig::default()).unwrap();
    assert!(ctx.pinned.is_empty());
    assert!(ctx.high_importance.is_empty());
    assert!(ctx.due.is_empty());
    assert!(ctx.recent.is_empty());
}

#[test]
fn resume_with_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

/// Regression for the multi-segment scope-cache bug: `add_fact` on a path with
/// depth > 1 (`user:michael/project:demo`) must make the fact retrievable via a
/// `scope_subtree` query on that same path *in the same session*.
///
/// The defect was that `ensure_scope_with_conn` inserted only the leaf node into
/// the in-memory `scope_tree`, leaving the intermediate `user:michael` link
/// absent. `resolve_path` walks children from root, so it failed at the first
/// missing segment and `scope_subtree("user:michael/project:demo")` resolved to
/// `None` → 0 results, even though the DB held the full chain.
#[test]
fn execute_query_subtree_multi_segment_scope() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder = MockEmbedder { dim: DIM };

    engine
        .add_fact(
            &AddFactRequest {
                content: "multi-segment scoped fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: Some("user:michael/project:demo".into()),
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap();

    // Subtree of the deep leaf must find the fact.
    let leaf = engine
        .execute_query(&MemoryQuery::new().scope_subtree("user:michael/project:demo"))
        .unwrap()
        .results;
    assert_eq!(leaf.len(), 1, "leaf subtree must retrieve the fact");
    assert_eq!(leaf[0].fact.content, "multi-segment scoped fact");

    // Subtree of an intermediate ancestor must also find the descendant fact.
    let ancestor = engine
        .execute_query(&MemoryQuery::new().scope_subtree("user:michael"))
        .unwrap()
        .results;
    assert_eq!(
        ancestor.len(),
        1,
        "ancestor subtree must retrieve the descendant fact"
    );

    // Exact match on the deep leaf must also resolve.
    let exact = engine
        .execute_query(&MemoryQuery::new().scope_exact("user:michael/project:demo"))
        .unwrap()
        .results;
    assert_eq!(exact.len(), 1, "exact deep-path query must resolve");
}

#[test]
fn execute_query_fact_type_filter() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();

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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().text("nonexistent"))
        .unwrap()
        .results;
    assert!(results.is_empty());
}

#[test]
fn execute_query_default_limit() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
        Err(MemoryError::Reranker(
            crate::error::RerankerError::Provider("cross-encoder timeout".into()),
        ))
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
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
    let baseline_engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
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
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
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
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
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
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(SpyRerankerWrapper(spy.clone())))
        .build()
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
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(FailingReranker))
        .build()
        .unwrap();
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
    assert!(matches!(
        result.unwrap_err(),
        MemoryError::Reranker(crate::error::RerankerError::Provider(_))
    ));
}

#[test]
fn reranker_name_accessor() {
    let engine_none = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine_none.reranker_name(), None);

    let engine_some = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    assert_eq!(engine_some.reranker_name(), Some("reverse"));
}

#[test]
fn debug_output_includes_reranker() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    let debug = format!("{engine:?}");
    assert!(
        debug.contains("reverse"),
        "Debug output should include reranker name"
    );
}

#[test]
fn rerank_depth_none_falls_back_to_limit() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let (_, f1) = add_session_fact(&engine, "fact a", "s1");
    let (_, f2) = add_session_fact(&engine, "fact b", "s1");

    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 2); // A→B and B→A

    // Verify edges in DB
    let co_edges = {
        let edges = crate::store::edges::EdgeStore::new(&engine.pool.read().unwrap())
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    add_session_fact(&engine, "a", "s1");
    add_session_fact(&engine, "b", "s1");
    add_session_fact(&engine, "c", "s1");

    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 6); // 3 pairs × 2 directions
}

#[test]
fn link_session_facts_single_fact_noop() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    add_session_fact(&engine, "lonely", "s1");

    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 0);
}

#[test]
fn link_session_facts_empty_session_noop() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let created = engine.link_session_facts("nonexistent", None).unwrap();
    assert_eq!(created, 0);
}

#[test]
fn link_session_facts_idempotent() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();

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
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    add_scoped_session_fact(&engine, "alice a", "s1", "user:alice");
    add_scoped_session_fact(&engine, "bob b", "s1", "user:bob");
    add_scoped_session_fact(&engine, "root c", "s1", "user:charlie");

    // None = global lookup (backward-compatible)
    let created = engine.link_session_facts("s1", None).unwrap();
    assert_eq!(created, 6); // 3 facts × 2 directions
}

#[test]
fn link_session_facts_scope_subtree() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(OutOfBoundsReranker))
        .build()
        .unwrap();
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
        matches!(
            err,
            MemoryError::Reranker(crate::error::RerankerError::OutOfBoundsIndex { .. })
        ),
        "should be a Reranker(OutOfBoundsIndex) error, got: {err}"
    );
    assert!(
        err.to_string().contains("out-of-bounds"),
        "error message should mention out-of-bounds, got: {err}"
    );
}

#[test]
fn reranker_rejects_duplicates() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(DuplicatingReranker))
        .build()
        .unwrap();
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
        matches!(
            err,
            MemoryError::Reranker(crate::error::RerankerError::DuplicateIndex { .. })
        ),
        "should be a Reranker(DuplicateIndex) error, got: {err}"
    );
    assert!(
        err.to_string().contains("duplicate"),
        "error message should mention duplicate, got: {err}"
    );
}

#[test]
fn reranker_allows_valid_subset() {
    // A well-behaved reranker (ReverseReranker) should still work fine
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
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

    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(FilteringReranker))
        .build()
        .unwrap();
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

    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(NanScoreReranker))
        .build()
        .unwrap();
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
        matches!(
            err,
            MemoryError::Reranker(crate::error::RerankerError::NonFiniteScore { .. })
        ),
        "should be a Reranker(NonFiniteScore) error, got: {err}"
    );
    assert!(
        err.to_string().contains("non-finite"),
        "error message should mention non-finite score, got: {err}"
    );
}

#[test]
fn reranker_rejects_output_too_long() {
    // Returns more (index, score) pairs than it was given candidates, tripping
    // the subset-contract length check — which `validate_reranker_output` runs
    // *before* the bounds/duplicate/finite checks. Pins the `OutputTooLong`
    // variant end-to-end (its three sibling variants already have integration
    // coverage; this closes the remaining gap).
    struct TooManyResultsReranker;
    impl Reranker for TooManyResultsReranker {
        fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            // One more pair than there are candidates. Index 0 is reused with an
            // in-bounds value so the *only* invariant violated is output length
            // (keeps the earlier bounds check from firing first).
            let mut out: Vec<(usize, f64)> = candidates
                .iter()
                .enumerate()
                .map(|(i, c)| (i, c.score))
                .collect();
            out.push((0, 0.0));
            Ok(out)
        }
        fn name(&self) -> &'static str {
            "too_many"
        }
    }

    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(TooManyResultsReranker))
        .build()
        .unwrap();
    let embedder = MockEmbedder { dim: DIM };
    engine
        .add_fact(
            &AddFactRequest {
                content: "length contract fact".into(),
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
        text: Some("length".into()),
        embedding: None,
        mode: SearchMode::Fts,
        limit: 10,
        rerank_depth: None,
        valid_at: None,
        fact_type: None,
        scope: None,
    });

    assert!(result.is_err(), "should reject output longer than input");
    let err = result.unwrap_err();
    assert!(
        matches!(
            err,
            MemoryError::Reranker(crate::error::RerankerError::OutputTooLong { .. })
        ),
        "should be a Reranker(OutputTooLong) error, got: {err}"
    );
    assert!(
        err.to_string().contains("exceeds input length"),
        "error message should mention exceeding input length, got: {err}"
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

        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("mock", "test", self.dim)
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let ids = engine.add_facts_batch(&[], &embedder, None).unwrap();
    assert!(ids.is_empty());
}

#[test]
fn add_facts_batch_with_scopes() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("mock", "test", DIM)
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
        fn fingerprint(&self) -> EmbeddingFingerprint {
            EmbeddingFingerprint::new("mock", "test", DIM)
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();

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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let fact = make_new_fact("outcome target", vec![0.5; DIM]);
    let fact_id = insert_raw_fact(&engine, &fact);

    let event_id = engine
        .record_outcome(fact_id, crate::types::Outcome::Positive)
        .unwrap();
    assert!(event_id > 0);
}

#[test]
fn record_outcome_nonexistent_fact_returns_not_found() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

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
        let engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
        let fact = make_new_fact("pinned for ro", vec![0.5; DIM]);
        insert_raw_fact(&engine, &fact)
    };

    // Re-open read-only
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();

    let result = engine.record_outcome(fact_id, crate::types::Outcome::Positive);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::ReadOnly));
}

#[test]
fn get_outcome_counts_nonexistent_fact_returns_not_found() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    let result = engine.get_outcome_counts(999);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::NotFound(_)));
}

#[test]
fn get_outcome_counts_no_outcomes_returns_zeros() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let engine = MemoryEngine::builder(DIM).build().unwrap();
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

    let engine = MemoryEngine::builder(DIM).build().unwrap();
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
fn get_outcome_counts_batch_matches_per_fact_loop() {
    use crate::types::Outcome;

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let f1 = insert_raw_fact(&engine, &make_new_fact("batch one", vec![0.5; DIM]));
    let f2 = insert_raw_fact(&engine, &make_new_fact("batch two", vec![0.3; DIM]));
    let f3 = insert_raw_fact(
        &engine,
        &make_new_fact("batch three (no outcomes)", vec![0.1; DIM]),
    );

    engine.record_outcome(f1, Outcome::Positive).unwrap();
    engine.record_outcome(f1, Outcome::Positive).unwrap();
    engine.record_outcome(f1, Outcome::Negative).unwrap();
    engine.record_outcome(f2, Outcome::Neutral).unwrap();
    // f3 deliberately has no outcomes.

    let nonexistent = 999_999;
    let ids = [f1, f2, f3, nonexistent];
    let batch = engine.get_outcome_counts_batch(&ids).unwrap();

    // Batch must equal the per-fact loop for every existing fact.
    for &id in &[f1, f2, f3] {
        let loop_counts = engine.get_outcome_counts(id).unwrap();
        let batch_counts = batch.get(&id).copied().unwrap_or_default();
        assert_eq!(
            batch_counts, loop_counts,
            "batch and per-fact counts must match for fact {id}"
        );
    }
    // Facts with no outcomes (f3) and nonexistent ids are absent from the map.
    assert!(
        !batch.contains_key(&f3),
        "zero-outcome fact should be absent"
    );
    assert!(
        !batch.contains_key(&nonexistent),
        "nonexistent id should be absent"
    );
    // Spot-check the actual tallies.
    assert_eq!(
        batch[&f1],
        crate::types::OutcomeCounts {
            positive: 2,
            negative: 1,
            neutral: 0
        }
    );
    assert_eq!(
        batch[&f2],
        crate::types::OutcomeCounts {
            positive: 0,
            negative: 0,
            neutral: 1
        }
    );
}

#[test]
fn get_outcome_counts_batch_empty_input_no_query() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let batch = engine.get_outcome_counts_batch(&[]).unwrap();
    assert!(batch.is_empty());
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

// --- MemoryEngineBuilder (#113) ---

#[test]
fn builder_in_memory_matches_open_memory() {
    // No `.path()` => in-memory engine, identical to `open_memory`.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine.embed_dim, DIM);
    assert!(engine.reranker_name().is_none());
    // In-memory pool has no backing file.
    assert!(engine.pool.path().is_none());
}

// NOTE (#541): #543's `builder_rejects_in_memory_read_only` (a RUNTIME check
// that `read_only` on an in-memory builder returns `Err`) was removed. The
// typestate builder makes that combination unrepresentable at COMPILE time —
// `read_only` exists only on `MemoryEngineBuilder<File>`. The guarantee is
// enforced by a `compile_fail` doctest in `engine::builder`.

#[test]
fn builder_file_backed_matches_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("builder.db");
    let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    assert_eq!(engine.embed_dim, DIM);
    assert!(engine.pool.path().is_some());
    // The file was created on disk.
    assert!(path.exists());
}

#[test]
fn builder_wires_reranker() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    assert_eq!(engine.reranker_name(), Some("reverse"));
}

#[test]
fn builder_wires_search_config() {
    let engine = MemoryEngine::builder(DIM)
        .search_config(SearchConfig { ann_threshold: 0 })
        .build()
        .unwrap();
    // Search config flows through to the engine.
    assert_eq!(
        engine.search_config.as_ref().map(|c| c.ann_threshold),
        Some(0),
        "search_config should be threaded through build()"
    );
}

#[test]
#[allow(clippy::significant_drop_tightening)]
fn builder_threads_upcaster_registry_in_memory() {
    // Regression for #543: `.upcaster_registry(custom).build()` with NO `.path()`
    // must HONOR the custom registry. Before the fix, `build()`'s in-memory branch
    // routed through `open_memory_with`, which hardcodes an empty registry, so a
    // custom registry set via the builder was silently dropped.
    //
    // Observable: an event inserted at revision 1 (via a raw store with an empty
    // registry) is upcast on replay *only if* the engine's threaded registry has
    // the matching 1->2 upcaster. With an empty registry, `list_upcasted` is a
    // no-op and the payload field is absent.
    let mut registry = crate::store::upcaster::UpcasterRegistry::new();
    registry.register("Interaction", 1, |mut v| {
        v["upcasted"] = serde_json::json!(true);
        Ok(v)
    });

    let engine = MemoryEngine::builder(DIM)
        .upcaster_registry(registry)
        .build()
        .unwrap();

    // Insert an event stamped at revision 1 using an *empty* registry, bypassing
    // the engine's ingest (which would stamp at the engine registry's latest).
    {
        let conn = engine.pool.write();
        let empty = crate::store::upcaster::UpcasterRegistry::new();
        let store = crate::store::events::EventStore::new(&conn, &empty);
        let event = NewEvent {
            timestamp: chrono::Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"msg": "hello"}),
            source: "test".into(),
            session_id: Some("s1".into()),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };
        store.insert(&event).unwrap();
    }

    let filter = crate::inspect::ReplayFilter {
        upcast: true,
        ..Default::default()
    };
    let events = engine.replay_events(&filter).unwrap();
    assert_eq!(events.len(), 1);
    // The threaded registry's 1->2 upcaster ran on replay. Without the fix the
    // engine holds an empty registry and `upcasted` would be absent.
    assert_eq!(
        events[0].payload.get("upcasted"),
        Some(&serde_json::json!(true)),
        "custom upcaster_registry must be honored in-memory and applied on replay"
    );
    assert_eq!(events[0].event_revision, 2, "payload upcast to revision 2");
}

#[test]
fn builder_read_only_rejects_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.db");
    // read_only on a non-existent file fails, exactly like `open` with a
    // read-only `EngineConfig`.
    let result = MemoryEngine::builder(DIM)
        .path(&path)
        .read_only(true)
        .build();
    assert!(result.is_err());
}

#[test]
fn builder_read_only_opens_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ro.db");
    // First create it read-write.
    {
        let _engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    }
    // Then re-open read-only.
    let engine = MemoryEngine::builder(DIM)
        .path(&path)
        .read_only(true)
        .build()
        .unwrap();
    assert_eq!(engine.embed_dim, DIM);
    assert!(engine.pool.is_read_only());
}

#[test]
fn builder_embed_dim_mismatch_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dim.db");
    {
        let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
        // Identity (incl. dim) is recorded on the first embedding write (#613).
        engine
            .add_fact(
                &AddFactRequest {
                    content: "seed".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &MockEmbedder { dim: DIM },
                None,
            )
            .unwrap();
    }
    // Re-opening with a different embed_dim must fail (parity with `open`).
    let err = MemoryEngine::builder(DIM + 1)
        .path(&path)
        .build()
        .unwrap_err();
    assert!(matches!(err, MemoryError::Migration(_)));
}

// --- Issue #130: engine-level error-path coverage ---
//
// These pin error paths reached *through the engine facade* (not the lower
// stores), for the gaps not already covered by the ~13 `execute_query_*`
// store-only tests:
//   1. `add_fact` — invalid scope path; out-of-range `importance`.
//   2. `restore_json` — corrupt JSON; `restore_sqlite` — non-SQLite file.

#[test]
fn add_fact_invalid_scope_path_returns_scope_label_conflict() {
    // An empty path segment ("a//b" → segments ["a", "", "b"]) is rejected by
    // `ScopeStore::validate_label` while resolving the scope inside `add_fact`'s
    // write lock. The error must surface verbatim at the engine boundary as a
    // typed `Conflict(ScopeLabel)` — not a generic Database/Internal error.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let err = engine
        .add_fact(
            &AddFactRequest {
                content: "fact with a malformed scope".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("a//b".into()),
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap_err();

    assert!(
        matches!(
            err,
            MemoryError::Conflict(crate::error::ConflictError::ScopeLabel(_))
        ),
        "expected Conflict(ScopeLabel) for an empty scope segment, got {err:?}"
    );

    // Discriminating: the failed insert must NOT have leaked a fact into the
    // store (scope resolution happens before the row insert, under one lock).
    assert!(
        engine.list_active_facts(None).unwrap().is_empty(),
        "a rejected scope path must not persist any fact"
    );

    // A second malformed shape — a leading-whitespace label — must also be
    // rejected at the same boundary (guards against the check being narrowed
    // to only the empty-segment case).
    let err_ws = engine
        .add_fact(
            &AddFactRequest {
                content: "fact with whitespace scope".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some(" leading".into()),
                opts: None,
            },
            &embedder,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(
            err_ws,
            MemoryError::Conflict(crate::error::ConflictError::ScopeLabel(_))
        ),
        "expected Conflict(ScopeLabel) for a leading-whitespace label, got {err_ws:?}"
    );
}

#[test]
fn add_fact_rejects_out_of_range_importance() {
    // CONTRACT (issue #571, follow-up to #130): `AddFactOptions::importance` is
    // documented as "Must be in [0, 1]". `add_fact` now ENFORCES this loudly —
    // an out-of-range (or non-finite) value is rejected with
    // `Conflict(PolicyParameter)` BEFORE anything is persisted (no event, no
    // fact), rather than being silently stored verbatim. A valid [0, 1] value
    // still succeeds.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder = MockEmbedder { dim: DIM };

    let request = |importance: f64| AddFactRequest {
        content: format!("importance = {importance}"),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: Some(AddFactOptions {
            importance: Some(importance),
            ..Default::default()
        }),
    };

    // Above the range: rejected as Conflict(PolicyParameter), nothing persisted.
    let err_high = engine
        .add_fact(&request(5.0), &embedder, None)
        .expect_err("importance > 1.0 must be rejected");
    assert!(
        matches!(
            err_high,
            MemoryError::Conflict(crate::error::ConflictError::PolicyParameter(_))
        ),
        "expected Conflict(PolicyParameter) for importance 5.0, got {err_high:?}"
    );

    // Below the range: also rejected.
    let err_low = engine
        .add_fact(&request(-0.1), &embedder, None)
        .expect_err("importance < 0.0 must be rejected");
    assert!(
        matches!(
            err_low,
            MemoryError::Conflict(crate::error::ConflictError::PolicyParameter(_))
        ),
        "expected Conflict(PolicyParameter) for importance -0.1, got {err_low:?}"
    );

    // Neither rejected insert left a fact behind.
    assert_eq!(
        engine.list_active_facts(None).unwrap().len(),
        0,
        "rejected out-of-range inserts must not persist any fact"
    );

    // In-range still works and is stored verbatim (base + materialized score).
    let id = engine
        .add_fact(&request(0.8), &embedder, None)
        .expect("in-range importance 0.8 must succeed");
    let fact = engine.get_fact(id).unwrap();
    assert!(
        (fact.importance - 0.8).abs() < f64::EPSILON,
        "in-range importance stored verbatim: got {}",
        fact.importance
    );
    assert!(
        (fact.importance_score - 0.8).abs() < f64::EPSILON,
        "importance_score seeded from the base importance: got {}",
        fact.importance_score
    );
}

#[test]
fn restore_json_corrupt_json_returns_serialization_error() {
    // `restore_json` parses the snapshot via `serde_json::from_reader`; invalid
    // JSON must surface as `MemoryError::Serialization` at the engine boundary,
    // and the (not-yet-created) target DB must not be left behind.
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("corrupt.json");
    std::fs::write(&snapshot_path, b"{ this is : not valid json ]]").unwrap();

    let target = dir.path().join("restored.db");
    let config = EngineConfig::new(target.clone(), DIM);

    let err = MemoryEngine::restore_json(&snapshot_path, &config).unwrap_err();
    assert!(
        matches!(err, MemoryError::Serialization(_)),
        "expected Serialization error for corrupt JSON snapshot, got {err:?}"
    );
    // The snapshot is parsed BEFORE the DB is opened, so no orphan file.
    assert!(
        !target.exists(),
        "a corrupt-JSON restore must not create the target database"
    );
}

#[test]
fn restore_sqlite_non_sqlite_file_returns_database_error() {
    // `restore_sqlite` accepts only a real SQLite backup. A regular file that is
    // NOT a SQLite database passes the `is_file()` precondition, gets copied to
    // the target, and then fails when the probe connection runs its first
    // statement against the bogus header — surfacing `MemoryError::Database`.
    // The orphaned copy must be cleaned up.
    let dir = tempfile::tempdir().unwrap();
    let bogus = dir.path().join("not_a_db.bin");
    // Garbage bytes that do not begin with the "SQLite format 3\0" magic.
    std::fs::write(&bogus, b"this is plainly not an sqlite database file\n").unwrap();

    let target = dir.path().join("restored.db");
    let config = EngineConfig::new(target.clone(), DIM);

    let err = MemoryEngine::restore_sqlite(&bogus, &config).unwrap_err();
    assert!(
        matches!(err, MemoryError::Database(_)),
        "expected Database error when restoring from a non-SQLite file, got {err:?}"
    );
    // The copied orphan must be removed on the failure path.
    assert!(
        !target.exists(),
        "a failed restore_sqlite must clean up the copied target file"
    );
}

#[test]
fn restore_sqlite_missing_file_returns_not_found() {
    // The `is_file()` precondition rejects a non-existent backup up front with a
    // clear `NotFound`, before any copy is attempted.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does_not_exist.db");
    let target = dir.path().join("restored.db");
    let config = EngineConfig::new(target.clone(), DIM);

    let err = MemoryEngine::restore_sqlite(&missing, &config).unwrap_err();
    assert!(
        matches!(err, MemoryError::NotFound(_)),
        "expected NotFound for a missing backup file, got {err:?}"
    );
    assert!(!target.exists());
}

mod snapshot_integration {
    use super::*;

    fn open_file_engine(dir: &std::path::Path) -> MemoryEngine {
        MemoryEngine::builder(DIM)
            .path(dir.join("test.db"))
            .build()
            .unwrap()
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
            let engine = MemoryEngine::builder(DIM)
                .path(dir.path().join("test.db"))
                .build()
                .unwrap();
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
        let engine = MemoryEngine::builder(DIM).build().unwrap();
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
        let engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("test.db"))
            .read_only(true)
            .build()
            .unwrap();
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
