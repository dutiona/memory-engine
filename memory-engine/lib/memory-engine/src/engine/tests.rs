use super::*;
use crate::graph::MemoryGraph;
use crate::resume::context::ResumeConfig;
use crate::search::hybrid::{SearchMode, SearchQuery};
use crate::search::query::MemoryQuery;
use crate::traits::{
    ConflictArbiter, ConsolidationConfig, CrudDecision, EmbeddingProvider, ForgetPolicy,
    PersistenceClassifier, SummarizableContent, SummaryGenerator,
};
use crate::types::{
    AddFactOptions, AddFactRequest, ClassifierInput, EmbeddingFingerprint, EventType, Fact,
    FactType, NewEdge, NewEvent, NewFact,
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

/// Keyword-driven embedder that maps a small fixed vocabulary to orthogonal
/// basis vectors in a 4-dimensional space (issue #453).
///
/// `MockEmbedder` returns the same constant vector for every input, so every
/// fact ties on cosine similarity and the vector-ranking path is never
/// discriminated. This embedder instead projects each known keyword onto a
/// distinct unit axis:
///
/// | keyword | vector        |
/// | ------- | ------------- |
/// | `cats`  | `[1, 0, 0, 0]`|
/// | `dogs`  | `[0, 1, 0, 0]`|
/// | `birds` | `[0, 0, 1, 0]`|
/// | `fish`  | `[0, 0, 0, 1]`|
///
/// The first keyword found in the (lowercased) text wins; text with no known
/// keyword embeds to the zero vector (cosine 0 against every axis). Because the
/// axes are orthonormal, a query embedded as one axis scores cosine `1.0`
/// against the matching fact and `0.0` against the others — an unambiguous
/// ranking oracle for the brute-force cosine scan and RRF blending.
struct DeterministicEmbedder {
    dim: usize,
}

impl DeterministicEmbedder {
    /// The orthogonal vocabulary: keyword → basis axis index.
    const VOCAB: [(&'static str, usize); 4] = [("cats", 0), ("dogs", 1), ("birds", 2), ("fish", 3)];

    /// Build the basis vector for a single keyword (panics in tests if the
    /// keyword is outside the vocabulary or the axis exceeds `self.dim`).
    fn axis(&self, keyword: &str) -> Vec<f32> {
        let (_, idx) = Self::VOCAB
            .iter()
            .find(|(kw, _)| *kw == keyword)
            .copied()
            .unwrap_or_else(|| panic!("keyword '{keyword}' not in DeterministicEmbedder vocab"));
        let mut v = vec![0.0_f32; self.dim];
        v[idx] = 1.0;
        v
    }
}

impl EmbeddingProvider for DeterministicEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let lower = text.to_ascii_lowercase();
        let mut v = vec![0.0_f32; self.dim];
        // First vocabulary keyword present in the text wins.
        if let Some((_, idx)) = Self::VOCAB
            .iter()
            .find(|(kw, _)| lower.contains(kw))
            .copied()
        {
            v[idx] = 1.0;
        }
        Ok(v)
    }

    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("deterministic", "test", self.dim)
    }
}

struct MockGen;
impl SummaryGenerator for MockGen {
    fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String> {
        Ok(items.iter().map(|i| i.text).collect::<Vec<_>>().join("; "))
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

/// Captures the `new_fact` argument passed to `arbitrate` so tests can inspect
/// which synthetic `Fact` the engine hands to the arbiter.  Uses a `Mutex` so
/// that `CapturingArbiter` is `Send + Sync` (required by the trait bound) while
/// still being mutated via `&self`.
struct CapturingArbiter {
    captured: std::sync::Mutex<Option<Fact>>,
}
impl CapturingArbiter {
    fn new() -> Self {
        Self {
            captured: std::sync::Mutex::new(None),
        }
    }
    fn take(&self) -> Option<Fact> {
        self.captured.lock().unwrap().take()
    }
}
impl ConflictArbiter for CapturingArbiter {
    fn arbitrate(&self, _old: &Fact, new_fact: &Fact) -> Result<CrudDecision> {
        *self.captured.lock().unwrap() = Some(new_fact.clone());
        Ok(CrudDecision::Noop)
    }
}

fn make_new_fact(content: &str, embedding: Vec<f32>) -> NewFact {
    NewFact::builder(content, embedding, FactType::Semantic)
        .content_hash(blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string())
        .build()
}

/// Test helper: insert a raw fact via the storage backend (bypasses engine's `add_fact`).
async fn insert_raw_fact(engine: &MemoryEngine, fact: &NewFact) -> i64 {
    engine.storage().insert_fact(fact).await.unwrap()
}

// --- Phase 1 tests ---

/// The L10 size bound also guards `resolve_conflict`, which persists the
/// candidate `NewFact` verbatim on an Add/Update decision. The check runs
/// before the arbiter and the old-fact lookup, so a non-existent `old_id`
/// still surfaces `PayloadTooLarge` rather than `NotFound`.
#[tokio::test]
async fn resolve_conflict_rejects_oversized_fact() {
    use crate::error::{ConflictError, MemoryError};

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let arbiter = FixedArbiter {
        decision: CrudDecision::Add,
    };
    let mut oversized = make_new_fact("seed", vec![0.5; DIM]);
    oversized.content = "x".repeat(crate::limits::MAX_PAYLOAD_BYTES + 1);

    let err = engine
        .resolve_conflict(&arbiter, 9999, &oversized)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        MemoryError::Conflict(ConflictError::PayloadTooLarge {
            kind: "fact content",
            ..
        })
    ));
}

#[tokio::test]
async fn open_memory_succeeds() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine.embed_dim(), DIM);
}

#[tokio::test]
async fn ingest_returns_event_id() {
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
    let id = engine.ingest(&event).await.unwrap();
    assert_eq!(id, 1);
}

#[tokio::test]
async fn add_fact_returns_fact_id() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is fast".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    assert!(id > 0);
}

#[tokio::test]
async fn query_returns_results_after_adding_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is a systems programming language".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let query = SearchQuery::new(SearchMode::Fts, 10).text("Rust");
    let results = engine.query(&query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].fact.content.contains("Rust"));
}

// --- Issue #453: vector ranking discriminated by distinct embeddings ---

/// Vector-only search must rank the semantically-closest fact first when the
/// embeddings are actually distinct.
///
/// `MockEmbedder` returns a constant vector, so every fact ties on cosine and
/// this ordering is invisible. With [`DeterministicEmbedder`] each keyword maps
/// to an orthogonal basis axis, so a query embedded as the `cats` axis scores
/// cosine `1.0` against the cats fact and `0.0` against every other fact — a
/// strict ordering the brute-force cosine scan must honor.
///
/// **Discrimination**: if the vector path stopped ordering by cosine (e.g. an
/// off-by-one in `select_nth_unstable_by` / `sort_by`, or a sign flip in
/// `cosine_similarity`), the top result would no longer be the cats fact and
/// this assertion would fail. With a constant embedder it could never fail.
#[tokio::test]
async fn vector_query_ranks_closest_embedding_first() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder = DeterministicEmbedder { dim: DIM };

    for content in ["cats are independent", "dogs are loyal", "birds can fly"] {
        engine
            .add_fact(
                &AddFactRequest {
                    content: content.into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(DeterministicEmbedder { dim: DIM })
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
    }

    // Query embedded as the "cats" axis — orthogonal to dogs/birds.
    let results = engine
        .query(&SearchQuery::new(SearchMode::Vector, 10).embedding(embedder.axis("cats")))
        .await
        .unwrap();

    assert_eq!(results.len(), 3, "all three facts are vector candidates");
    assert!(
        results[0].fact.content.contains("cats"),
        "closest embedding (cats) must rank first, got: {}",
        results[0].fact.content
    );
    // Cosine 1.0 for the matching axis strictly dominates the 0.0 ties below it.
    assert!(
        results[0].score > results[1].score,
        "cats score ({}) must strictly exceed the next ({})",
        results[0].score,
        results[1].score
    );
}

/// A second axis confirms the ranking tracks the query, not insertion order:
/// querying the "dogs" axis surfaces the dogs fact first even though it was
/// inserted second. A constant embedder would tie all facts and fall back to a
/// stable/insertion order, masking this.
#[tokio::test]
async fn vector_query_ranking_follows_query_axis() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder = DeterministicEmbedder { dim: DIM };

    for content in ["cats are independent", "dogs are loyal", "fish swim"] {
        engine
            .add_fact(
                &AddFactRequest {
                    content: content.into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(DeterministicEmbedder { dim: DIM })
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
    }

    let results = engine
        .query(&SearchQuery::new(SearchMode::Vector, 10).embedding(embedder.axis("dogs")))
        .await
        .unwrap();

    assert_eq!(results.len(), 3);
    assert!(
        results[0].fact.content.contains("dogs"),
        "query axis (dogs) must rank the dogs fact first, got: {}",
        results[0].fact.content
    );
    assert!(results[0].score > results[1].score);
}

#[tokio::test]
async fn embed_dim_validation_rejects_mismatch() {
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
                std::sync::Arc::new(MockEmbedder { dim: 768 })
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
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

#[tokio::test]
async fn first_add_fact_records_embedding_meta() {
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
            .storage()
            .load_embedding_fingerprint()
            .await
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
        .add_fact(
            &req("a"),
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        engine.storage().load_embedding_fingerprint().await.unwrap(),
        Some(expected.clone()),
        "first write records the embedder's fingerprint"
    );

    // #614 enforcement: a second add with a DIFFERENT fingerprint is hard-rejected
    // (not silently ignored), and the stored identity is left untouched.
    let err = engine
        .add_fact(
            &req("b"),
            std::sync::Arc::new(OtherEmbedder { dim: DIM })
                as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
        "a differing later fingerprint must be rejected, got {err:?}"
    );
    assert_eq!(
        engine.storage().load_embedding_fingerprint().await.unwrap(),
        Some(expected),
        "stored identity is unchanged after a rejected mismatched write"
    );
}

#[tokio::test]
async fn verify_embedding_identity_enforces_match() {
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
    // Fresh store has no identity yet -> any same-dim provider is compatible.
    engine
        .verify_embedding_identity(&MockEmbedder { dim: DIM })
        .await
        .expect("fresh store compatible with any same-dim provider");
    // ...but a wrong-dim provider still fails fast on a fresh store (would otherwise
    // fail on every later write/query).
    let dim_err = engine
        .verify_embedding_identity(&MockEmbedder { dim: DIM + 1 })
        .await
        .expect_err("wrong-dim provider must fail the eager check on a fresh store");
    assert!(
        matches!(dim_err, MemoryError::EmbeddingDimension { .. }),
        "expected EmbeddingDimension, got {dim_err:?}"
    );

    // Stamp the identity via a real embedding write.
    let req = AddFactRequest {
        content: "a".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    };
    engine
        .add_fact(
            &req,
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();

    // Matching provider -> Ok; differing provider -> EmbeddingModelMismatch.
    engine
        .verify_embedding_identity(&MockEmbedder { dim: DIM })
        .await
        .expect("matching provider passes the eager check");
    let err = engine
        .verify_embedding_identity(&OtherEmbedder { dim: DIM })
        .await
        .expect_err("differing provider must fail the eager check");
    assert!(
        matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
        "expected EmbeddingModelMismatch, got {err:?}"
    );
}

#[tokio::test]
async fn add_fact_precomputed_records_or_compares_declared_identity() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let req = AddFactRequest {
        content: "p".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    };
    let declared = EmbeddingFingerprint::new("declared-model", "tei", DIM);

    // Fresh store: the declared identity is RECORDED (a precomputed-only workflow can
    // bootstrap, #615) — no live embedder needed.
    let id = engine
        .add_fact_precomputed(&req, vec![0.5; DIM], &declared, None)
        .await
        .expect("precomputed add with a declared model records identity on a fresh store");
    assert!(id > 0);
    assert_eq!(
        engine.storage().load_embedding_fingerprint().await.unwrap(),
        Some(declared.clone()),
        "the declared fingerprint becomes the store identity"
    );

    // Matching declared identity -> accepted.
    engine
        .add_fact_precomputed(&req, vec![0.6; DIM], &declared, None)
        .await
        .expect("matching declared identity is accepted");

    // Differing declared identity (same dim) -> hard mismatch, closing the foreign-vector hole.
    let foreign = EmbeddingFingerprint::new("other-model", "ollama", DIM);
    let mismatch = engine
        .add_fact_precomputed(&req, vec![0.7; DIM], &foreign, None)
        .await
        .expect_err("a differing declared model must be rejected");
    assert!(
        matches!(mismatch, MemoryError::EmbeddingModelMismatch { .. }),
        "expected EmbeddingModelMismatch, got {mismatch:?}"
    );

    // Wrong dimension on a FRESH store -> the absent-branch dim guard fires
    // (declared.dim must equal the engine dim before it can be recorded). On a stamped
    // store a wrong dim would instead surface as a full-tuple mismatch (dim is one field).
    let fresh = MemoryEngine::builder(DIM).build().unwrap();
    let wrong_dim = EmbeddingFingerprint::new("declared-model", "tei", DIM + 3);
    let dim_err = fresh
        .add_fact_precomputed(&req, vec![0.6; DIM + 3], &wrong_dim, None)
        .await
        .expect_err("wrong-dimension precomputed vector must be rejected");
    assert!(
        matches!(dim_err, MemoryError::EmbeddingDimension { .. }),
        "expected EmbeddingDimension, got {dim_err:?}"
    );
}

#[tokio::test]
async fn noop_bootstrap_does_not_stamp_identity() {
    // #643: bootstrapping a session that creates zero facts (here an empty reader)
    // must NOT record the embedding identity. Previously the engine stamped before
    // the inner import ran, so a no-op bootstrap permanently fixed the identity even
    // though no vector was written.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let report = engine
        .bootstrap_session(
            std::io::Cursor::new(""),
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            std::sync::Arc::new(crate::KeywordExtractor)
                as std::sync::Arc<dyn crate::bootstrap::SessionExtractor>,
            &crate::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(report.facts_created, 0, "empty session creates no facts");
    assert!(
        engine
            .storage()
            .load_embedding_fingerprint()
            .await
            .unwrap()
            .is_none(),
        "a fact-less bootstrap must not stamp the embedding identity"
    );
}

#[tokio::test]
async fn noop_bootstrap_then_real_write_records_real_embedder() {
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
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            std::sync::Arc::new(crate::KeywordExtractor)
                as std::sync::Arc<dyn crate::bootstrap::SessionExtractor>,
            &crate::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap();
    // The store must be left UNSTAMPED by the no-op run (the crux of #643): this is
    // what lets the first real writer below establish the identity.
    assert!(
        engine
            .storage()
            .load_embedding_fingerprint()
            .await
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
        .add_fact(
            &req,
            std::sync::Arc::new(EmbedderB { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        engine.storage().load_embedding_fingerprint().await.unwrap(),
        Some(EmbedderB { dim: DIM }.fingerprint()),
        "the real first writer's identity must win, not the no-op bootstrap's"
    );
}

#[tokio::test]
async fn bootstrap_creating_facts_stamps_identity() {
    // Positive guard: a bootstrap that DOES create facts records the embedder
    // identity (atomically, inside the session savepoint).
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let fixture = include_str!("../../../../../tests/fixtures/success_session.jsonl");
    let report = engine
        .bootstrap_session(
            std::io::Cursor::new(fixture),
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            std::sync::Arc::new(crate::KeywordExtractor)
                as std::sync::Arc<dyn crate::bootstrap::SessionExtractor>,
            &crate::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert!(report.facts_created > 0, "fixture should create facts");
    assert_eq!(
        engine.storage().load_embedding_fingerprint().await.unwrap(),
        Some(MockEmbedder { dim: DIM }.fingerprint()),
        "a fact-creating bootstrap records the embedder's fingerprint"
    );
}

#[tokio::test]
async fn noop_bootstrap_directory_does_not_stamp_identity() {
    // #643 names all three bootstrap wrappers. `bootstrap_directory` stamps each
    // session inside its own savepoint; an empty directory processes no session, so
    // the store must be left unstamped.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let report = engine
        .bootstrap_directory(
            dir.path(),
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            std::sync::Arc::new(crate::KeywordExtractor)
                as std::sync::Arc<dyn crate::bootstrap::SessionExtractor>,
            &crate::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(report.facts_created, 0, "empty directory creates no facts");
    assert!(
        engine
            .storage()
            .load_embedding_fingerprint()
            .await
            .unwrap()
            .is_none(),
        "a fact-less directory bootstrap must not stamp the embedding identity"
    );
}

#[tokio::test]
async fn bootstrap_directory_creating_facts_stamps_identity() {
    // Positive guard for the multi-file path: a directory with a fact-producing
    // session records the embedder identity (inside that session's savepoint).
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.jsonl"),
        include_str!("../../../../../tests/fixtures/success_session.jsonl"),
    )
    .unwrap();
    let report = engine
        .bootstrap_directory(
            dir.path(),
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            std::sync::Arc::new(crate::KeywordExtractor)
                as std::sync::Arc<dyn crate::bootstrap::SessionExtractor>,
            &crate::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert!(report.facts_created > 0, "fixture should create facts");
    assert_eq!(
        engine.storage().load_embedding_fingerprint().await.unwrap(),
        Some(MockEmbedder { dim: DIM }.fingerprint()),
        "a fact-creating directory bootstrap records the embedder's fingerprint"
    );
}

#[tokio::test]
async fn memory_directory_stamps_identity_even_when_empty() {
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
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            &crate::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        engine.storage().load_embedding_fingerprint().await.unwrap(),
        Some(MockEmbedder { dim: DIM }.fingerprint()),
        "memory-directory import is meta-first: it stamps even with no files"
    );
}

#[tokio::test]
async fn read_only_open_of_unstamped_db_is_ok() {
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
    assert!(engine.is_read_only());
}

#[tokio::test]
async fn get_set_config() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert!(engine.get_config("custom_key").await.unwrap().is_none());
    engine
        .set_config("custom_key", "custom_value")
        .await
        .unwrap();
    assert_eq!(
        engine.get_config("custom_key").await.unwrap(),
        Some("custom_value".into())
    );
}

// --- Phase 2 tests ---

#[tokio::test]
async fn graph_starts_empty() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine.graph_stats(), (0, 0));
}

#[tokio::test]
async fn consolidate_deduplicates_similar_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    // Two near-identical embeddings
    insert_raw_fact(
        &engine,
        &make_new_fact("fact alpha", vec![1.0, 0.0, 0.0, 0.0]),
    )
    .await;
    insert_raw_fact(
        &engine,
        &make_new_fact("fact alpha copy", vec![0.99, 0.01, 0.0, 0.0]),
    )
    .await;

    let config = ConsolidationConfig::builder()
        .dedup_threshold(0.90)
        .min_cluster_size(10) // high threshold so no clusters form
        .build();
    let stats = engine
        .consolidate(
            std::sync::Arc::new(MockGen) as std::sync::Arc<dyn SummaryGenerator>,
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            &config,
        )
        .await
        .unwrap();
    assert_eq!(stats.duplicates_removed, 1);

    let active = engine.list_active_facts(None).await.unwrap();
    assert_eq!(active.len(), 1);
}

#[tokio::test]
async fn consolidate_is_idempotent() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    insert_raw_fact(
        &engine,
        &make_new_fact("unique A", vec![1.0, 0.0, 0.0, 0.0]),
    )
    .await;
    insert_raw_fact(
        &engine,
        &make_new_fact("unique B", vec![0.0, 1.0, 0.0, 0.0]),
    )
    .await;

    let config = ConsolidationConfig::builder()
        .dedup_threshold(0.92)
        .min_cluster_size(10)
        .build();

    let _stats1 = engine
        .consolidate(
            std::sync::Arc::new(MockGen) as std::sync::Arc<dyn SummaryGenerator>,
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            &config,
        )
        .await
        .unwrap();
    let stats2 = engine
        .consolidate(
            std::sync::Arc::new(MockGen) as std::sync::Arc<dyn SummaryGenerator>,
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            &config,
        )
        .await
        .unwrap();

    // Second run should find 0 new duplicates
    assert_eq!(stats2.duplicates_removed, 0);
    // Both facts still active
    assert_eq!(engine.list_active_facts(None).await.unwrap().len(), 2);
}

/// A `SummaryGenerator` that performs an engine WRITE from inside `summarize`.
/// Used to prove the consumer callbacks run without the engine holding its write
/// lock (#409): the write only succeeds if `consolidate` is *not* holding
/// `write_conn` across the summarize/embed phase.
struct LockProbingGenerator {
    engine: std::sync::Arc<MemoryEngine>,
}
impl SummaryGenerator for LockProbingGenerator {
    fn summarize(&self, items: &[SummarizableContent<'_>]) -> Result<String> {
        // A real engine write reached from within the consumer callback. The
        // compute phase runs the consumer `summarize`/`embed` off the async
        // executor (in `spawn_blocking`, #409), so `block_on` here is valid — the
        // blocking-pool thread is not a runtime worker. If the write phase still
        // held the storage write lock across the compute phase, this re-entrant
        // write would deadlock/park rather than land — the deterministic signal.
        let engine = std::sync::Arc::clone(&self.engine);
        tokio::runtime::Handle::current().block_on(async move {
            engine
                .set_config("consolidate_compute_was_lock_free", "yes")
                .await
        })?;
        Ok(items.iter().map(|i| i.text).collect::<Vec<_>>().join("; "))
    }
}

/// #409 (denial-of-service / lock starvation): the consumer `SummaryGenerator` /
/// `EmbeddingProvider` callbacks must run **without** the engine holding its `write_conn` lock.
/// Before the read→compute→write split, `MemoryEngine::consolidate` held that guard
/// across the entire pipeline — including the unbounded `summarize`/`embed` network
/// IO — starving every other engine writer (and, on an in-memory pool, every reader,
/// since reads serialize through the same `Mutex`) for the full duration.
///
/// The probe is deterministic and thread-free: a generator that writes through the
/// engine from inside `summarize` succeeds only if the compute phase is lock-free.
/// With the lock still held this same-thread re-lock would panic ("reentrant")
/// rather than complete (see [`LockProbingGenerator`]); after the fix the marker
/// write lands and we assert it persisted.
#[tokio::test]
async fn consolidate_runs_consumer_callbacks_without_holding_write_lock() {
    use std::sync::Arc;
    let engine = Arc::new(MemoryEngine::builder(DIM).build().unwrap());

    // A single-linkage chain that forms exactly one cluster (so `summarize` is
    // actually invoked) with no near-duplicate expiry: adjacent cosines ~0.883
    // sit between the 0.85 cluster and 0.90 dedup thresholds.
    insert_raw_fact(&engine, &make_new_fact("a", vec![1.0, 0.0, 0.0, 0.0])).await;
    insert_raw_fact(&engine, &make_new_fact("b", vec![0.8829, 0.4695, 0.0, 0.0])).await;
    insert_raw_fact(&engine, &make_new_fact("c", vec![0.5592, 0.829, 0.0, 0.0])).await;

    let generator = LockProbingGenerator {
        engine: Arc::clone(&engine),
    };
    let config = ConsolidationConfig::builder()
        .dedup_threshold(0.90)
        .cluster_threshold(0.85)
        .min_cluster_size(2)
        .build();

    let stats = engine
        .consolidate(
            std::sync::Arc::new(generator) as std::sync::Arc<dyn SummaryGenerator>,
            std::sync::Arc::new(MockEmbedder { dim: DIM }) as std::sync::Arc<dyn EmbeddingProvider>,
            &config,
        )
        .await
        .unwrap();
    assert_eq!(
        stats.clusters_created, 1,
        "fixture must form exactly one cluster so `summarize` runs"
    );

    // The callback's write landed → the compute phase did not hold the write lock.
    assert_eq!(
        engine
            .get_config("consolidate_compute_was_lock_free")
            .await
            .unwrap(),
        Some("yes".to_string()),
        "a consumer callback must be able to write during consolidation (#409)"
    );
}

#[tokio::test]
async fn forget_prunes_stale_facts() {
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
            .base_importance(0.01)
            .build(),
    )
    .await;

    let policy = ForgetPolicy {
        min_importance: 0.3,
        ..ForgetPolicy::default()
    };
    let stats = engine.forget(&policy).await.unwrap();
    assert_eq!(stats.facts_expired, 1);
    assert_eq!(stats.facts_evaluated, 1);
}

#[tokio::test]
async fn forget_rejects_invalid_policy() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let policy = ForgetPolicy {
        half_life_days: 0.0, // invalid
        ..ForgetPolicy::default()
    };
    assert!(engine.forget(&policy).await.is_err());
}

#[tokio::test]
async fn resolve_conflict_update_creates_edge() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let old_id = insert_raw_fact(&engine, &make_new_fact("outdated", vec![0.5; DIM])).await;

    let arbiter = FixedArbiter {
        decision: CrudDecision::Update,
    };
    let result = engine
        .resolve_conflict(&arbiter, old_id, &make_new_fact("updated", vec![0.5; DIM]))
        .await
        .unwrap();

    assert_eq!(result.decision, CrudDecision::Update);
    assert!(result.new_fact_id.is_some());

    // Old fact should be expired
    let old = engine.get_fact(old_id).await.unwrap();
    assert!(old.t_expired.is_some());

    // Graph should have the new edge
    let new_id = result.new_fact_id.unwrap();
    assert!(engine.graph_has_node(new_id));
    assert_eq!(engine.graph_neighbors(new_id), vec![old_id]);
}

#[tokio::test]
async fn resolve_conflict_noop_no_changes() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let old_id = insert_raw_fact(&engine, &make_new_fact("existing", vec![0.5; DIM])).await;

    let arbiter = FixedArbiter {
        decision: CrudDecision::Noop,
    };
    let result = engine
        .resolve_conflict(
            &arbiter,
            old_id,
            &make_new_fact("candidate", vec![0.5; DIM]),
        )
        .await
        .unwrap();

    assert_eq!(result.decision, CrudDecision::Noop);
    assert!(result.new_fact_id.is_none());

    // Old fact unchanged
    let old = engine.get_fact(old_id).await.unwrap();
    assert!(old.t_expired.is_none());
}

// --- #434: NotFound for nonexistent old_id ---

#[tokio::test]
async fn resolve_conflict_nonexistent_fact_returns_not_found() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let arbiter = FixedArbiter {
        decision: CrudDecision::Update,
    };
    let result = engine
        .resolve_conflict(
            &arbiter,
            999_999,
            &make_new_fact("irrelevant", vec![0.5; DIM]),
        )
        .await;
    assert!(
        matches!(result, Err(MemoryError::NotFound(_))),
        "expected NotFound, got {result:?}"
    );
}

// --- #435: Delete cascade removes DB edges and in-memory graph edge ---

#[tokio::test]
async fn resolve_conflict_delete_cascades_edges() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    let fact_a = insert_raw_fact(&engine, &make_new_fact("fact a", vec![0.5; DIM])).await;
    let fact_b = insert_raw_fact(&engine, &make_new_fact("fact b", vec![0.5; DIM])).await;

    // Insert an active edge between A and B, then sync the in-memory graph.
    engine
        .storage()
        .insert_edge(&NewEdge {
            source_fact_id: fact_a,
            target_fact_id: fact_b,
            relation_type: "related".into(),
            weight: 1.0,
            t_created: chrono::Utc::now(),
            t_expired: None,
            scope_id: 1,
        })
        .await
        .unwrap();
    {
        let active_edges = engine.storage().list_active_edges().await.unwrap();
        *engine.graph.write() = MemoryGraph::from_active_edges(&active_edges);
    }
    assert_eq!(
        engine.graph_stats().1,
        1,
        "pre-condition: one edge before delete"
    );

    // Delete fact A via conflict resolution.
    let arbiter = FixedArbiter {
        decision: CrudDecision::Delete,
    };
    let result = engine
        .resolve_conflict(&arbiter, fact_a, &make_new_fact("gone", vec![0.5; DIM]))
        .await
        .unwrap();
    assert_eq!(result.decision, CrudDecision::Delete);

    // (a) Fact A is expired.
    let fact = engine.get_fact(fact_a).await.unwrap();
    assert!(
        fact.t_expired.is_some(),
        "fact A must be expired after Delete"
    );

    // (b) The in-memory graph edge involving A is removed.
    assert_eq!(
        engine.graph_stats().1,
        0,
        "in-memory graph edge count must drop to 0 after Delete cascade"
    );
    assert!(
        engine.graph_neighbors(fact_a).is_empty(),
        "fact A must have no in-memory graph neighbors after Delete"
    );

    // (c) DB side: the cascade expired the A→B edge (independently of the
    // in-memory mirror above). Asserting this proves `expire_by_fact` actually
    // committed — a bug that cleared only the in-memory graph would pass (b) but
    // fail here, which is exactly the DB/graph divergence #435 guards against.
    // The fresh DB held exactly one edge, so the post-cascade active set must be
    // empty — a stronger check than "no edge incident to A" (it also catches a
    // cascade that wrongly *inserted* an edge).
    let active = engine.storage().list_active_edges().await.unwrap();
    assert!(
        active.is_empty(),
        "DB must have no active edges after Delete cascade, got {active:?}"
    );
}

// --- #436: Update cascade removes stale edges and adds the new contradicts edge ---

#[tokio::test]
async fn resolve_conflict_update_cascade_rebuilds_graph() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    let fact_a = insert_raw_fact(&engine, &make_new_fact("original a", vec![0.5; DIM])).await;
    let fact_b = insert_raw_fact(&engine, &make_new_fact("fact b", vec![0.5; DIM])).await;

    // Pre-existing edge: A → B (some prior relationship).
    engine
        .storage()
        .insert_edge(&NewEdge {
            source_fact_id: fact_a,
            target_fact_id: fact_b,
            relation_type: "related".into(),
            weight: 1.0,
            t_created: chrono::Utc::now(),
            t_expired: None,
            scope_id: 1,
        })
        .await
        .unwrap();
    {
        let active_edges = engine.storage().list_active_edges().await.unwrap();
        *engine.graph.write() = MemoryGraph::from_active_edges(&active_edges);
    }
    assert_eq!(
        engine.graph_stats().1,
        1,
        "pre-condition: one edge before Update"
    );

    // Update fact A via conflict resolution.
    let arbiter = FixedArbiter {
        decision: CrudDecision::Update,
    };
    let result = engine
        .resolve_conflict(
            &arbiter,
            fact_a,
            &make_new_fact("replacement a", vec![0.5; DIM]),
        )
        .await
        .unwrap();
    assert_eq!(result.decision, CrudDecision::Update);
    let new_id = result.new_fact_id.unwrap();

    // The stale A↔B edge is gone; the new "contradicts" edge new_id→A is present.
    assert_eq!(
        engine.graph_stats().1,
        1,
        "total active edge count must remain exactly 1 (contradicts edge replaces stale edge)"
    );
    assert_eq!(
        engine.graph_neighbors(new_id),
        vec![fact_a],
        "new fact must have a contradicts edge pointing to the old fact A"
    );
    assert!(
        engine.graph_neighbors(fact_a).is_empty(),
        "old fact A must have no outgoing edges after Update cascade"
    );

    // DB side: the cascade must have expired the stale A→B edge and committed the
    // new contradicts edge new_id→A. Verifying the DB (not just the in-memory
    // mirror) is the point of #436 — the two can diverge, so a divergence-free
    // result requires asserting both. Exactly one active edge must remain.
    let active = engine.storage().list_active_edges().await.unwrap();
    assert_eq!(
        active.len(),
        1,
        "exactly one active DB edge (the contradicts edge) must remain after Update cascade, got {active:?}"
    );
    assert_eq!(
        active[0].source_fact_id, new_id,
        "active edge source must be the new fact"
    );
    assert_eq!(
        active[0].target_fact_id, fact_a,
        "active edge target must be the old fact A"
    );
    assert_eq!(
        active[0].relation_type, "contradicts",
        "the surviving edge must be the contradicts edge"
    );
}

// --- #438: Capturing arbiter verifies the synthetic Fact the engine passes in ---

#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "both comparisons are against exact sentinel/literal constants; \
              UNSCORED_IMPORTANCE is 0.5 and distinctive_base_importance is 0.9"
)]
async fn resolve_conflict_arbiter_sees_synthetic_importance() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let old_id = insert_raw_fact(&engine, &make_new_fact("existing", vec![0.5; DIM])).await;

    // Build a NewFact with a distinctive base_importance to confirm the arbiter
    // sees it, and confirm that importance_score is the UNSCORED_IMPORTANCE sentinel.
    let distinctive_base_importance = 0.9_f64;
    let new_fact = NewFact::builder("candidate", vec![0.5; DIM], FactType::Semantic)
        .base_importance(distinctive_base_importance)
        .build();

    let arbiter = CapturingArbiter::new();
    let result = engine
        .resolve_conflict(&arbiter, old_id, &new_fact)
        .await
        .unwrap();
    assert_eq!(result.decision, CrudDecision::Noop);

    let captured = arbiter
        .take()
        .expect("arbiter must have captured the new_fact");

    // The synthetic Fact is pre-insert: id is the placeholder 0.
    assert_eq!(
        captured.id, 0,
        "pre-insert synthetic Fact must have id == 0"
    );

    // importance_score is the UNSCORED_IMPORTANCE sentinel — NOT a real computed score.
    assert_eq!(
        captured.importance_score,
        Fact::UNSCORED_IMPORTANCE,
        "arbiter must see UNSCORED_IMPORTANCE sentinel, not a derived score"
    );

    // base_importance is the caller-supplied raw prior — the real value.
    assert_eq!(
        captured.base_importance, distinctive_base_importance,
        "arbiter must see the caller-supplied base_importance, not a default"
    );
}

#[tokio::test]
async fn graph_loads_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    let config = EngineConfig::new(db_path, DIM);

    // First session: add facts and create an edge via conflict resolution
    {
        let engine = MemoryEngine::open_from_config(&config, None).unwrap();
        let old_id = insert_raw_fact(&engine, &make_new_fact("original", vec![0.5; DIM])).await;
        let arbiter = FixedArbiter {
            decision: CrudDecision::Update,
        };
        engine
            .resolve_conflict(
                &arbiter,
                old_id,
                &make_new_fact("replacement", vec![0.5; DIM]),
            )
            .await
            .unwrap();
        assert_eq!(engine.graph_stats().1, 1);
    }

    // Second session: graph should be restored from DB
    {
        let engine = MemoryEngine::open_from_config(&config, None).unwrap();
        assert_eq!(engine.graph_stats().1, 1);
    }
}

#[tokio::test]
async fn list_summaries_empty() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let summaries = engine
        .list_summaries(&ConsolidationLevel::Global)
        .await
        .unwrap();
    assert!(summaries.is_empty());
}

// --- Phase 3 / T2: AddFactOptions ---

#[tokio::test]
async fn add_fact_with_custom_importance() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let opts = AddFactOptions {
        base_importance: Some(0.9),
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    let fact = engine.get_fact(id).await.unwrap();
    assert!((fact.base_importance - 0.9).abs() < f64::EPSILON);
}

#[tokio::test]
async fn add_fact_with_temporal_bounds() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    let fact = engine.get_fact(id).await.unwrap();
    assert!(fact.t_valid.is_some());
    assert!(fact.t_invalid.is_some());
}

#[tokio::test]
async fn add_fact_with_scope_path() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "scoped fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("user:test/project:demo".into()),
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    let fact = engine.get_fact(id).await.unwrap();
    assert_ne!(fact.scope_id, 1); // not root
}

#[tokio::test]
async fn add_fact_none_opts_uses_defaults() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "default fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    let fact = engine.get_fact(id).await.unwrap();
    assert!((fact.base_importance - 0.5).abs() < f64::EPSILON);
    assert!(fact.t_valid.is_none());
}

// --- Phase 3 / T7: Send + Sync ---

#[tokio::test]
async fn engine_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MemoryEngine>();
}

#[tokio::test]
async fn engine_concurrent_reads() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("concurrent.db");

    let engine = std::sync::Arc::new(MemoryEngine::builder(DIM).path(db_path).build().unwrap());
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust is fast".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let mut handles = vec![];
    for _ in 0..4 {
        let e = engine.clone();
        handles.push(tokio::spawn(async move {
            let results = e
                .query(&SearchQuery::new(SearchMode::Fts, 10).text("Rust"))
                .await
                .unwrap();
            assert_eq!(results.len(), 1);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

// A FTS query by the shared "Concurrent" content — factored out so the concurrent
// reader and the terminal check issue exactly the same query.
#[cfg(test)]
fn concurrent_probe_query() -> SearchQuery {
    SearchQuery::new(SearchMode::Fts, 10).text("Concurrent")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_write_then_read_across_threads() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("write_read.db");

    let engine = std::sync::Arc::new(MemoryEngine::builder(DIM).path(db_path).build().unwrap());

    // Writer and reader run CONCURRENTLY on a multi-threaded runtime, sharing the engine
    // via `Arc` — exercising overlapping write+read through the async storage port and the
    // RwLock-guarded in-memory projections (the cutover's whole point: high-fan-out
    // concurrency, with no lock held across an `.await`). Both are spawned before either
    // is awaited, so they genuinely overlap.
    let e_w = engine.clone();
    let writer = tokio::spawn(async move {
        let embedder: std::sync::Arc<dyn EmbeddingProvider> =
            std::sync::Arc::new(MockEmbedder { dim: DIM });
        e_w.add_fact(
            &AddFactRequest {
                content: "Concurrent write test".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder,
            None,
        )
        .await
        .unwrap();
    });

    let e_r = engine.clone();
    let reader = tokio::spawn(async move {
        // Concurrent with the writer: the count is inherently a race (0 or 1). Assert only
        // that the query neither errors nor deadlocks and never over-counts.
        let results = e_r.query(&concurrent_probe_query()).await.unwrap();
        assert!(
            results.len() <= 1,
            "concurrent read must never see more than the one written fact, got {}",
            results.len()
        );
    });

    // Join both, THEN assert the deterministic terminal state.
    writer.await.unwrap();
    reader.await.unwrap();

    let final_results = engine.query(&concurrent_probe_query()).await.unwrap();
    assert_eq!(
        final_results.len(),
        1,
        "the written fact must be visible after both tasks complete"
    );
}

// --- Phase 3 / T9: resume_context ---

#[tokio::test]
async fn resume_empty_engine() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let ctx = engine
        .resume_context(&ResumeConfig::default())
        .await
        .unwrap();
    assert!(ctx.pinned.is_empty());
    assert!(ctx.high_importance.is_empty());
    assert!(ctx.due.is_empty());
    assert!(ctx.recent.is_empty());
}

#[tokio::test]
async fn resume_with_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    // Add a pinned fact (appears in tier 1)
    let opts_pinned = AddFactOptions {
        base_importance: Some(0.95),
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Add a low-importance root fact (recent tier)
    let opts_low = AddFactOptions {
        base_importance: Some(0.1),
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let config = ResumeConfig::default();
    let ctx = engine.resume_context(&config).await.unwrap();
    // The pinned fact should appear in the pinned tier
    assert_eq!(ctx.pinned.len(), 1);
    assert!(ctx.pinned[0].is_pinned);
    assert!(ctx.pinned[0].content.contains("Rust"));
    // The low-importance fact should appear in recent
    assert!(!ctx.recent.is_empty());
}

#[tokio::test]
async fn resume_nonexistent_scope_returns_not_found() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let config = ResumeConfig {
        scope_path: Some("nonexistent/path".into()),
        ..ResumeConfig::default()
    };
    let err = engine.resume_context(&config).await.unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[tokio::test]
async fn resume_rejects_invalid_config() {
    // #359: ResumeConfig::validate() is enforced at the public boundary
    // (fail-fast, before the scope lock / DB), so an out-of-range config is a
    // Conflict — not a silently-empty tier.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let config = ResumeConfig {
        high_importance_min: 5.0,
        ..ResumeConfig::default()
    };
    let err = engine.resume_context(&config).await.unwrap_err();
    assert!(matches!(err, MemoryError::Conflict(_)), "got {err:?}");
}

// --- Issue #279: ResumeConfig::default() is pure (no wall-clock capture) ---

/// `ResumeConfig::default()` must not capture `Utc::now()` at construction — the
/// `now` field defaults to `None`, and the engine resolves the wall-clock instant
/// once at call time. This makes `Default` deterministic and stops stored configs
/// from operating on a stale timestamp.
#[test]
fn resume_config_default_now_is_none() {
    assert_eq!(ResumeConfig::default().now, None);
}

// --- Issue #93: surfaced_at for due facts in non-due tiers ---

#[tokio::test]
async fn resume_stamps_surfaced_at_on_pinned_due_fact() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
                    base_importance: Some(0.9),
                    ..Default::default()
                }),
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

#[tokio::test]
async fn resume_stamps_surfaced_at_on_high_importance_due_fact() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
                    base_importance: Some(0.9),
                    t_valid: Some(past),
                    ..Default::default()
                }),
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let config = ResumeConfig {
        now: Some(now),
        high_importance_min: 0.7,
        ..ResumeConfig::default()
    };
    let ctx = engine.resume_context(&config).await.unwrap();

    // Fact should appear in high_importance tier (not due tier)
    assert_eq!(ctx.high_importance.len(), 1);
    assert!(ctx.due.is_empty());

    // Bug: surfaced_at should be stamped because the fact IS due.
    assert!(
        ctx.high_importance[0].surfaced_at.is_some(),
        "high-importance-but-due fact must have surfaced_at stamped"
    );
}

#[tokio::test]
async fn resume_does_not_stamp_invalidated_pinned_due_fact() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
                    base_importance: Some(0.9),
                    ..Default::default()
                }),
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

    // Fact lands in pinned tier (it's pinned and not expired)
    assert_eq!(ctx.pinned.len(), 1);

    // But surfaced_at must NOT be stamped — the fact is bi-temporally
    // invalidated (t_invalid <= now), so it's no longer "due".
    assert!(
        ctx.pinned[0].surfaced_at.is_none(),
        "invalidated fact must not have surfaced_at stamped"
    );
}

// --- Issue #476: surfaced_at stamping is idempotent across calls ---

/// A second `resume_context` call on the same due fact must NOT re-stamp
/// `surfaced_at`: the timestamp from the first surfacing is the authoritative
/// record and must stay stable. Both the closure guard in `engine/resume.rs`
/// (`f.surfaced_at.is_none()`) and the SQL guard in `FactStore::stamp_surfaced`
/// (`AND surfaced_at IS NULL`) protect against re-stamping; this test fails if
/// either drifts.
///
/// The two calls pass *different* `now` values (`now` then `later`). If
/// re-stamping leaked, the second call would overwrite the timestamp with
/// `later`, so the equality assertion discriminates the regression.
#[tokio::test]
async fn resume_surfaced_at_is_idempotent() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let now = Utc::now();
    let past = now - chrono::Duration::hours(1);
    let later = now + chrono::Duration::hours(2);

    // A plain due fact (t_valid in the past) — lands in the `due` tier.
    engine
        .add_fact(
            &AddFactRequest {
                content: "deploy the staging build".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    t_valid: Some(past),
                    ..Default::default()
                }),
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let ctx1 = engine
        .resume_context(&ResumeConfig {
            now: Some(now),
            ..ResumeConfig::default()
        })
        .await
        .unwrap();
    assert_eq!(ctx1.due.len(), 1);
    let first_stamp = ctx1.due[0]
        .surfaced_at
        .expect("due fact must be surfaced on first resume_context");

    // Second call at a strictly later `now`. surfaced_at must be unchanged.
    let ctx2 = engine
        .resume_context(&ResumeConfig {
            now: Some(later),
            ..ResumeConfig::default()
        })
        .await
        .unwrap();
    assert_eq!(ctx2.due.len(), 1);
    let second_stamp = ctx2.due[0]
        .surfaced_at
        .expect("due fact must still carry its surfaced_at on the second call");

    assert_eq!(
        second_stamp, first_stamp,
        "surfaced_at must not be re-stamped on a second resume_context call"
    );
}

/// The same idempotency guarantee on the public `list_due()` path, which shares
/// the `surfaced_at.is_none()` guard (`engine/scheduling.rs`). A second
/// `list_due()` call must return the original `surfaced_at`, not a fresh stamp.
#[tokio::test]
async fn list_due_surfaced_at_is_idempotent() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let now = Utc::now();
    let past = now - chrono::Duration::hours(1);
    let later = now + chrono::Duration::hours(2);

    engine
        .add_fact(
            &AddFactRequest {
                content: "rotate the API key".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: Some(AddFactOptions {
                    t_valid: Some(past),
                    ..Default::default()
                }),
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let due1 = engine.list_due(now, None).await.unwrap();
    assert_eq!(due1.len(), 1);
    let first_stamp = due1[0]
        .surfaced_at
        .expect("due fact must be surfaced on first list_due");

    let due2 = engine.list_due(later, None).await.unwrap();
    assert_eq!(due2.len(), 1);
    let second_stamp = due2[0]
        .surfaced_at
        .expect("due fact must still carry its surfaced_at on the second call");

    assert_eq!(
        second_stamp, first_stamp,
        "surfaced_at must not be re-stamped on a second list_due call"
    );
}

// --- Issue #474: resume_context scope ancestor-chain filtering ---

/// `resume_context` with `scope_path = Some(<existing child>)` must scope the
/// `recent` and `high_importance` tiers to the resolved scope's ancestor chain
/// (`tree.ancestors(id)` = `[child, …parents, root]`), excluding facts from a
/// *sibling* scope that is not on that chain.
///
/// Every prior resume test passes `scope_path = None` (root only) or a
/// nonexistent path (`NotFound` short-circuit), so the success branch at
/// `engine/resume.rs` — `resolve_path` → `ancestors(id)` → a non-root
/// `scope_ids` slice into `list_by_importance_score` / `list_by_scopes_recent`
/// — was never exercised. This test creates two sibling scopes under a common
/// parent, populates both, resumes from one, and asserts the sibling's facts
/// are absent from both scope-filtered tiers.
///
/// **Discrimination**: if `ancestors(id)` were replaced by `subtree(id)`,
/// `[root]`, or an over-broad set that leaked the sibling scope, the
/// `does_not_contain` assertions on the `beta`/sibling facts would fail. With
/// `scope_path = None` (the only previously-tested path) the slice is always
/// `[root]` and a child/sibling distinction can never arise.
#[tokio::test]
async fn resume_with_existing_scope_path_excludes_sibling_scope() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    // Two sibling scopes under a common parent: project/alpha and project/beta.
    let alpha_id = engine.ensure_scope_path("project/alpha").await.unwrap();
    let beta_id = engine.ensure_scope_path("project/beta").await.unwrap();
    assert_ne!(alpha_id, beta_id, "siblings must be distinct scopes");

    // Helper: add a non-pinned fact at a given scope path with a chosen
    // importance (which materializes importance_score 1:1 at insert time).
    let engine_ref = &engine;
    let add = |scope: &str, content: &str, importance: f64| {
        let embedder = embedder.clone();
        let req = AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: Some(scope.into()),
            opts: Some(AddFactOptions {
                base_importance: Some(importance),
                ..Default::default()
            }),
        };
        async move { engine_ref.add_fact(&req, embedder, None).await.unwrap() }
    };

    // recent-tier facts (low importance, below the 0.7 high-importance floor).
    add("project/alpha", "alpha recent note", 0.1).await;
    add("project/beta", "beta recent note", 0.1).await;
    // high-importance-tier facts (importance_score >= 0.7).
    add("project/alpha", "alpha critical decision", 0.9).await;
    add("project/beta", "beta critical decision", 0.9).await;

    let config = ResumeConfig {
        scope_path: Some("project/alpha".into()),
        ..ResumeConfig::default()
    };
    let ctx = engine.resume_context(&config).await.unwrap();

    let recent_contents: Vec<&str> = ctx.recent.iter().map(|f| f.content.as_str()).collect();
    let high_contents: Vec<&str> = ctx
        .high_importance
        .iter()
        .map(|f| f.content.as_str())
        .collect();

    // alpha (resolved scope, on its own ancestor chain) is present.
    assert!(
        recent_contents.contains(&"alpha recent note"),
        "alpha-scope recent fact must surface, got recent={recent_contents:?}"
    );
    assert!(
        high_contents.contains(&"alpha critical decision"),
        "alpha-scope high-importance fact must surface, got high={high_contents:?}"
    );

    // beta (sibling, NOT on alpha's ancestor chain) is excluded from both
    // scope-filtered tiers.
    assert!(
        !recent_contents.contains(&"beta recent note"),
        "sibling-scope recent fact must be excluded, got recent={recent_contents:?}"
    );
    assert!(
        !high_contents.contains(&"beta critical decision"),
        "sibling-scope high-importance fact must be excluded, got high={high_contents:?}"
    );

    // Cross-tier safety net: no fact from the sibling scope appears anywhere in
    // the non-pinned tiers (the sibling's scope_id must never enter the chain).
    let sibling_leak = ctx
        .high_importance
        .iter()
        .chain(ctx.due.iter())
        .chain(ctx.recent.iter())
        .any(|f| f.scope_id == beta_id);
    assert!(
        !sibling_leak,
        "no sibling-scope fact may leak into any tier"
    );
}

// --- Phase 3b / T6: SearchConfig in EngineConfig ---

#[tokio::test]
async fn engine_config_default_has_no_search_config() {
    let config = EngineConfig::new("test.db".into(), 128);
    assert!(config.search_config.is_none());
}

#[tokio::test]
async fn engine_config_with_search_config() {
    let mut config = EngineConfig::new("test.db".into(), 128);
    config.search_config = Some(SearchConfig::default());
    assert_eq!(config.search_config.unwrap().ann_threshold, 50_000);
}

#[tokio::test]
async fn query_nonexistent_scope_returns_empty() {
    use crate::types::ScopeQuery;

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Query with a scope path that doesn't exist
    let query = SearchQuery::new(SearchMode::Fts, 10)
        .text("visible")
        .scope(ScopeQuery::Exact("nonexistent/scope".into()));
    let results = engine.query(&query).await.unwrap();
    assert!(
        results.is_empty(),
        "expected empty results for nonexistent scope, got {}",
        results.len()
    );
}

// --- Phase 3b / T8: Engine facade new methods ---

#[tokio::test]
async fn list_due_returns_scheduled_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let due = engine.list_due(Utc::now(), None).await.unwrap();
    assert_eq!(due.len(), 1);
    assert!(due[0].content.contains("check release"));

    let next = engine.next_due_time(None).await.unwrap();
    assert!(next.is_some()); // the future fact

    // Future-dated facts should be invisible to regular search (no valid_at)
    let search = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("future check"))
        .await
        .unwrap();
    assert!(
        search.is_empty(),
        "future-dated facts should not appear in regular search"
    );

    // But past-due facts should be visible
    let search2 = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("check release"))
        .await
        .unwrap();
    assert_eq!(search2.len(), 1, "past-due facts should appear in search");
}

#[tokio::test]
async fn pin_unpin_fact() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "pinnable".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    assert!(!engine.get_fact(id).await.unwrap().is_pinned);
    engine.pin_fact(id).await.unwrap();
    assert!(engine.get_fact(id).await.unwrap().is_pinned);
    engine.unpin_fact(id).await.unwrap();
    assert!(!engine.get_fact(id).await.unwrap().is_pinned);
}

#[tokio::test]
async fn add_fact_with_explicit_pin() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    assert!(engine.get_fact(id).await.unwrap().is_pinned);
}

#[tokio::test]
async fn add_fact_with_classifier() {
    struct PinSemantic;
    impl PersistenceClassifier for PinSemantic {
        fn should_pin(&self, input: &ClassifierInput) -> bool {
            input.fact_type == FactType::Semantic
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let classifier: std::sync::Arc<dyn PersistenceClassifier> = std::sync::Arc::new(PinSemantic);

    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "auto-pinned".into(),
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
    assert!(engine.get_fact(id).await.unwrap().is_pinned);

    let id2 = engine
        .add_fact(
            &AddFactRequest {
                content: "not pinned".into(),
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
}

#[tokio::test]
async fn explicit_pin_overrides_classifier() {
    struct AlwaysPin;
    impl PersistenceClassifier for AlwaysPin {
        fn should_pin(&self, _input: &ClassifierInput) -> bool {
            true
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
            embedder.clone(),
            Some(std::sync::Arc::new(classifier) as std::sync::Arc<dyn PersistenceClassifier>),
        )
        .await
        .unwrap();
    assert!(!engine.get_fact(id).await.unwrap().is_pinned);
}

// --- execute_query integration tests ---

#[tokio::test]
async fn execute_query_empty_returns_active_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "fact one".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "fact two".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new())
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r.match_type == MatchType::ImportanceRank)
    );
}

#[tokio::test]
async fn execute_query_text_search() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust systems programming".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "Python machine learning".into(),
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

    let results = engine
        .execute_query(&MemoryQuery::new().text("Rust"))
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "Rust systems programming");
    assert_eq!(results[0].match_type, MatchType::Fts);
}

#[tokio::test]
async fn execute_query_scope_only() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().scope_exact("project:demo"))
        .await
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
#[tokio::test]
async fn execute_query_subtree_multi_segment_scope() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    engine
        .add_fact(
            &AddFactRequest {
                content: "multi-segment scoped fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: Some("user:michael/project:demo".into()),
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Subtree of the deep leaf must find the fact.
    let leaf = engine
        .execute_query(&MemoryQuery::new().scope_subtree("user:michael/project:demo"))
        .await
        .unwrap()
        .results;
    assert_eq!(leaf.len(), 1, "leaf subtree must retrieve the fact");
    assert_eq!(leaf[0].fact.content, "multi-segment scoped fact");

    // Subtree of an intermediate ancestor must also find the descendant fact.
    let ancestor = engine
        .execute_query(&MemoryQuery::new().scope_subtree("user:michael"))
        .await
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
        .await
        .unwrap()
        .results;
    assert_eq!(exact.len(), 1, "exact deep-path query must resolve");
}

#[tokio::test]
async fn execute_query_fact_type_filter() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "episodic".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "semantic".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().fact_type(FactType::Semantic))
        .await
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

#[tokio::test]
async fn execute_query_importance_threshold() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let opts_low = AddFactOptions {
        base_importance: Some(0.1),
        ..Default::default()
    };
    let opts_high = AddFactOptions {
        base_importance: Some(0.9),
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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().min_importance_score(0.5))
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "high importance");
}

#[tokio::test]
async fn execute_query_pinned_only() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let id = engine
        .add_fact(
            &AddFactRequest {
                content: "pinned".into(),
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
    engine.pin_fact(id).await.unwrap();

    engine
        .add_fact(
            &AddFactRequest {
                content: "normal".into(),
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

    let results = engine
        .execute_query(&MemoryQuery::new().pinned_only())
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "pinned");
    assert!(results[0].fact.is_pinned);
}

#[tokio::test]
async fn execute_query_future_dated_excluded() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Empty query should NOT return the future-dated fact
    let results = engine
        .execute_query(&MemoryQuery::new())
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "present fact");

    // Scope-only query should also exclude future-dated facts
    let results2 = engine
        .execute_query(&MemoryQuery::new().min_importance_score(0.0))
        .await
        .unwrap()
        .results;
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].fact.content, "present fact");
}

#[tokio::test]
async fn execute_query_period_mutual_exclusion() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let now = Utc::now();

    let result = engine
        .execute_query(
            &MemoryQuery::new()
                .valid_at(now)
                .period(now - chrono::Duration::hours(1), now),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn execute_query_search_mode_conflict() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    // Hybrid requires both text and embedding
    let result = engine
        .execute_query(
            &MemoryQuery::new()
                .text("test")
                .search_mode(SearchMode::Hybrid),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn execute_query_search_mode_inference() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "Rust programming".into(),
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

    // Text-only → should infer FTS mode
    let results = engine
        .execute_query(&MemoryQuery::new().text("Rust"))
        .await
        .unwrap()
        .results;
    assert!(!results.is_empty());
    assert_eq!(results[0].match_type, MatchType::Fts);
}

#[tokio::test]
async fn execute_query_period_filter() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Period query covering only the past window
    let results = engine
        .execute_query(&MemoryQuery::new().period(
            now - chrono::Duration::hours(4),
            now - chrono::Duration::minutes(30),
        ))
        .await
        .unwrap()
        .results;

    // Both should match: past fact has [t_valid, t_invalid) overlapping the period,
    // and current fact has NULL t_valid/t_invalid (unbounded, overlaps everything)
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn execute_query_composed_filters() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let opts_high = AddFactOptions {
        base_importance: Some(0.9),
        ..Default::default()
    };
    let opts_low = AddFactOptions {
        base_importance: Some(0.1),
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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    // Text "Rust" + importance >= 0.5 → only "Rust high importance"
    let results = engine
        .execute_query(&MemoryQuery::new().text("Rust").min_importance_score(0.5))
        .await
        .unwrap()
        .results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.content, "Rust high importance");
}

#[tokio::test]
async fn execute_query_empty_results() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    let results = engine
        .execute_query(&MemoryQuery::new().text("nonexistent"))
        .await
        .unwrap()
        .results;
    assert!(results.is_empty());
}

#[tokio::test]
async fn execute_query_default_limit() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

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
                embedder.clone(),
                None,
            )
            .await
            .unwrap();
    }

    let results = engine
        .execute_query(&MemoryQuery::new())
        .await
        .unwrap()
        .results;
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

#[tokio::test]
async fn reranker_none_results_unchanged() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
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

    let results = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("fact"))
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn reranker_reverses_order() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
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
            embedder.clone(),
            None,
        )
        .await
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap();

    let baseline = baseline_engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("fact"))
        .await
        .unwrap();

    let reranked = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("fact"))
        .await
        .unwrap();

    assert_eq!(baseline.len(), reranked.len());
    assert_eq!(baseline.len(), 2);
    // Reversed order
    assert_eq!(baseline[0].fact.content, reranked[1].fact.content);
    assert_eq!(baseline[1].fact.content, reranked[0].fact.content);
}

#[tokio::test]
async fn reranker_skipped_for_vector_only_no_text() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta".into(),
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

    let results = engine
        .query(&SearchQuery::new(SearchMode::Vector, 10).embedding(vec![0.5; DIM]))
        .await
        .unwrap();

    // Reranker should NOT have fired (no text) — order should match vector similarity.
    // Both have identical embeddings, so they're equivalent; just check we got results.
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn reranker_applies_to_fts_only_mode() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
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

    // FTS-only with text → reranker should fire
    let results = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("fact"))
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    // Results are reversed by ReverseReranker
}

#[tokio::test]
async fn reranker_applies_to_vector_mode_with_text() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta".into(),
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

    // Vector mode WITH text → reranker should fire
    let results = engine
        .query(
            &SearchQuery::new(SearchMode::Vector, 10)
                .text("alpha")
                .embedding(vec![0.5; DIM]),
        )
        .await
        .unwrap();

    // Should still get results (vector search ignores text, but reranker fires)
    assert!(!results.is_empty());
}

#[tokio::test]
async fn rerank_depth_overfetches_then_truncates() {
    let spy = std::sync::Arc::new(SpyReranker::new());
    // Clone Arc into Box<dyn Reranker> — SpyReranker is Send+Sync
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(SpyRerankerWrapper(spy.clone())))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

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
                embedder.clone(),
                None,
            )
            .await
            .unwrap();
    }

    let results = engine
        .query(
            &SearchQuery::new(SearchMode::Fts, 3)
                .text("rerank test fact")
                .rerank_depth(8),
        )
        .await
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

#[tokio::test]
async fn reranker_error_propagates() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(FailingReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "test fact".into(),
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

    let result = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("test"))
        .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MemoryError::Reranker(crate::error::RerankerError::Provider(_))
    ));
}

#[tokio::test]
async fn reranker_name_accessor() {
    let engine_none = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine_none.reranker_name(), None);

    let engine_some = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    assert_eq!(engine_some.reranker_name(), Some("reverse"));
}

#[tokio::test]
async fn debug_output_includes_reranker() {
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

#[tokio::test]
async fn rerank_depth_none_falls_back_to_limit() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

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
                embedder.clone(),
                None,
            )
            .await
            .unwrap();
    }

    let results = engine
        .query(&SearchQuery::new(SearchMode::Fts, 5).text("limit test fact"))
        .await
        .unwrap();

    assert_eq!(
        results.len(),
        5,
        "should respect limit when rerank_depth is None"
    );
}

// --- Co-session edge tests ---

/// Helper: ingest an event with a `session_id` and add a fact linked to it.
async fn add_session_fact(engine: &MemoryEngine, content: &str, session_id: &str) -> (i64, i64) {
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
    let event_id = engine.ingest(&event).await.unwrap();
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: Some(event_id),
                scope: None,
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    (event_id, fact_id)
}

#[tokio::test]
async fn link_session_facts_creates_bidirectional_edges() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let (_, f1) = add_session_fact(&engine, "fact a", "s1").await;
    let (_, f2) = add_session_fact(&engine, "fact b", "s1").await;

    let created = engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(created, 2); // A→B and B→A

    // Verify edges in DB
    let co_edges = {
        let edges = engine.storage().list_active_edges().await.unwrap();
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

#[tokio::test]
async fn link_session_facts_three_facts_six_edges() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    add_session_fact(&engine, "a", "s1").await;
    add_session_fact(&engine, "b", "s1").await;
    add_session_fact(&engine, "c", "s1").await;

    let created = engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(created, 6); // 3 pairs × 2 directions
}

#[tokio::test]
async fn link_session_facts_single_fact_noop() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    add_session_fact(&engine, "lonely", "s1").await;

    let created = engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(created, 0);
}

#[tokio::test]
async fn link_session_facts_empty_session_noop() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let created = engine
        .link_session_facts("nonexistent", None)
        .await
        .unwrap();
    assert_eq!(created, 0);
}

#[tokio::test]
async fn link_session_facts_idempotent() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    add_session_fact(&engine, "a", "s1").await;
    add_session_fact(&engine, "b", "s1").await;

    let first = engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(first, 2);

    let second = engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(second, 0); // no new edges

    // Total edge count unchanged
    let (_, edge_count) = engine.graph_stats();
    assert_eq!(edge_count, 2);
}

#[tokio::test]
async fn link_session_facts_graph_degree() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let (_, f1) = add_session_fact(&engine, "a", "s1").await;
    let (_, f2) = add_session_fact(&engine, "b", "s1").await;
    let (_, f3) = add_session_fact(&engine, "c", "s1").await;

    // Before linking — no edges
    assert_eq!(engine.graph_degree(f1), 0);

    engine.link_session_facts("s1", None).await.unwrap();

    // After: each fact has 2 outgoing + 2 incoming = degree 4
    assert_eq!(engine.graph_degree(f1), 4);
    assert_eq!(engine.graph_degree(f2), 4);
    assert_eq!(engine.graph_degree(f3), 4);
}

#[tokio::test]
async fn link_session_facts_ignores_expired() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let (_, f1) = add_session_fact(&engine, "active1", "s1").await;
    add_session_fact(&engine, "active2", "s1").await;
    let (_, f3) = add_session_fact(&engine, "will_expire", "s1").await;

    // Expire f3 before linking
    engine.storage().expire_fact(f3, Utc::now()).await.unwrap();

    let created = engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(created, 2); // Only f1↔active2, not f3

    // f3 should have no edges
    assert_eq!(engine.graph_degree(f3), 0);
    assert_eq!(engine.graph_degree(f1), 2); // 1 out + 1 in
}

// --- Scope-aware session linking tests ---

/// Helper: add a fact in a specific scope, linked to a session.
async fn add_scoped_session_fact(
    engine: &MemoryEngine,
    content: &str,
    session_id: &str,
    scope_path: &str,
) -> (i64, i64) {
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
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
    let event_id = engine.ingest(&event).await.unwrap();
    let fact_id = engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: Some(event_id),
                scope: Some(scope_path.into()),
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
        .unwrap();
    (event_id, fact_id)
}

#[tokio::test]
async fn link_session_facts_scope_filters_cross_scope() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    // Two facts in user:alice, one in user:bob — same session_id
    let (_, f1) = add_scoped_session_fact(&engine, "alice a", "s1", "user:alice").await;
    let (_, f2) = add_scoped_session_fact(&engine, "alice b", "s1", "user:alice").await;
    let (_, f3) = add_scoped_session_fact(&engine, "bob c", "s1", "user:bob").await;

    // Scope-filtered: only link alice's facts
    let created = engine
        .link_session_facts("s1", Some("user:alice"))
        .await
        .unwrap();
    assert_eq!(created, 2); // f1↔f2

    assert_eq!(engine.graph_degree(f1), 2); // 1 out + 1 in
    assert_eq!(engine.graph_degree(f2), 2);
    assert_eq!(engine.graph_degree(f3), 0); // bob excluded
}

#[tokio::test]
async fn link_session_facts_scope_none_links_all() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    add_scoped_session_fact(&engine, "alice a", "s1", "user:alice").await;
    add_scoped_session_fact(&engine, "bob b", "s1", "user:bob").await;
    add_scoped_session_fact(&engine, "root c", "s1", "user:charlie").await;

    // None = global lookup (backward-compatible)
    let created = engine.link_session_facts("s1", None).await.unwrap();
    assert_eq!(created, 6); // 3 facts × 2 directions
}

#[tokio::test]
async fn link_session_facts_scope_subtree() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    // Create facts at different depths under user:alice
    let (_, f1) = add_scoped_session_fact(&engine, "top", "s1", "user:alice").await;
    let (_, f2) = add_scoped_session_fact(&engine, "nested", "s1", "user:alice/project:x").await;
    let (_, f3) = add_scoped_session_fact(&engine, "other", "s1", "user:bob").await;

    // Subtree from user:alice should include both alice and alice/project:x
    let created = engine
        .link_session_facts("s1", Some("user:alice"))
        .await
        .unwrap();
    assert_eq!(created, 2); // f1↔f2
    assert_eq!(engine.graph_degree(f1), 2);
    assert_eq!(engine.graph_degree(f2), 2);
    assert_eq!(engine.graph_degree(f3), 0);
}

#[tokio::test]
async fn link_session_facts_scope_not_found() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    add_session_fact(&engine, "a", "s1").await;

    let result = engine
        .link_session_facts("s1", Some("user:nonexistent"))
        .await;
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

#[tokio::test]
async fn reranker_rejects_out_of_bounds_index() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(OutOfBoundsReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "real fact".into(),
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

    let result = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("real"))
        .await;

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

#[tokio::test]
async fn reranker_rejects_duplicates() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(DuplicatingReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "dup fact alpha".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "dup fact beta".into(),
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

    let result = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("dup"))
        .await;

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

#[tokio::test]
async fn reranker_allows_valid_subset() {
    // A well-behaved reranker (ReverseReranker) should still work fine
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "guard alpha".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "guard beta".into(),
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

    let result = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("guard"))
        .await;

    assert!(
        result.is_ok(),
        "valid subset should pass: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().len(), 2);
}

#[tokio::test]
async fn reranker_allows_filtering_subset() {
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
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "filterable first".into(),
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
    engine
        .add_fact(
            &AddFactRequest {
                content: "filterable second".into(),
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

    let result = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("filterable"))
        .await;

    assert!(
        result.is_ok(),
        "filtering subset should pass: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().len(), 1);
}

#[tokio::test]
async fn reranker_rejects_non_finite_score() {
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
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "score test fact".into(),
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

    let result = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("score"))
        .await;

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

#[tokio::test]
async fn reranker_rejects_output_too_long() {
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
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    engine
        .add_fact(
            &AddFactRequest {
                content: "length contract fact".into(),
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

    let result = engine
        .query(&SearchQuery::new(SearchMode::Fts, 10).text("length"))
        .await;

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

#[tokio::test]
async fn embed_batch_default_impl_loops_embed() {
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

#[tokio::test]
async fn embed_batch_empty_returns_empty() {
    let embedder = MockEmbedder { dim: DIM };
    let result = embedder.embed_batch(&[]).unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn add_facts_batch_inserts_all_facts() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let entries: Vec<AddFactRequest> = (0..5)
        .map(|i| AddFactRequest {
            content: format!("batch fact {i}"),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        })
        .collect();

    let ids = engine
        .add_facts_batch(&entries, embedder.clone(), None)
        .await
        .unwrap();
    assert_eq!(ids.len(), 5);

    // All IDs should be unique and positive
    let unique: std::collections::HashSet<i64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 5);
    assert!(ids.iter().all(|&id| id > 0));

    // Verify facts are actually in the DB
    for (i, &id) in ids.iter().enumerate() {
        let fact = engine.get_fact(id).await.unwrap();
        assert_eq!(fact.content, format!("batch fact {i}"));
    }
}

#[tokio::test]
async fn add_facts_batch_empty_returns_empty() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let ids = engine
        .add_facts_batch(&[], embedder.clone(), None)
        .await
        .unwrap();
    assert!(ids.is_empty());
}

#[tokio::test]
async fn add_facts_batch_with_scopes() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

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

    let ids = engine
        .add_facts_batch(&entries, embedder.clone(), None)
        .await
        .unwrap();
    assert_eq!(ids.len(), 3);

    // Verify scope assignments via fact retrieval
    let f0 = engine.get_fact(ids[0]).await.unwrap();
    let f2 = engine.get_fact(ids[2]).await.unwrap();
    // f0 should be in a non-root scope, f2 in root (scope_id=1)
    assert_ne!(f0.scope_id, f2.scope_id);
    assert_eq!(f2.scope_id, 1); // root scope
}

#[tokio::test]
async fn add_facts_batch_with_classifier() {
    struct AlwaysPin;
    impl PersistenceClassifier for AlwaysPin {
        fn should_pin(&self, _input: &ClassifierInput) -> bool {
            true
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });
    let classifier = AlwaysPin;

    let entries = vec![AddFactRequest {
        content: "important fact".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    }];

    let ids = engine
        .add_facts_batch(
            &entries,
            embedder.clone(),
            Some(std::sync::Arc::new(classifier) as std::sync::Arc<dyn PersistenceClassifier>),
        )
        .await
        .unwrap();
    let fact = engine.get_fact(ids[0]).await.unwrap();
    assert!(fact.is_pinned);
}

/// Multi-entry batch with a *discriminating* classifier, interleaving
/// classifier-decided slots (`opts.pinned == None`) with caller-pinned slots
/// (`Some(true)`/`Some(false)`).
///
/// This is the regression guard for the `compute_batch_pins` slot/pending
/// stitching (`src/engine/ingest.rs`): each `None` slot must consume the *next*
/// classifier result IN ORDER, and the `ClassifierInput` built per entry must
/// carry *that* entry's own `fact_type`/`content` — not a neighbour's. A
/// single-entry batch with a constant classifier (see the test above) cannot
/// detect a mis-alignment: with one slot and one pending result, any ordering
/// bug is invisible. Here the classifier verdict depends on `fact_type`
/// (`Semantic` → pin, `Episodic` → no pin), and the `None`/`Some` slots are
/// interleaved so an off-by-one in the stitch flips an observable `is_pinned`.
#[tokio::test]
async fn add_facts_batch_classifier_interleaved_slots_align() {
    /// Pins iff the fact is `Semantic` — proves the per-entry `ClassifierInput`
    /// carries the right `fact_type` (and that results map to the right slot).
    struct PinSemantic;
    impl PersistenceClassifier for PinSemantic {
        fn should_pin(&self, input: &ClassifierInput) -> bool {
            input.fact_type == FactType::Semantic
        }
    }

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    // The classifier-decided slots (None) are deliberately NON-palindromic in
    // their verdicts (slots 0/2/4 → pin/skip/skip), so a mis-ordered stitch
    // (e.g. results applied in reverse) flips an observable `is_pinned` rather
    // than merely shuffling equal values.
    //
    // (content, fact_type, opts.pinned, expected is_pinned)
    //   slot 0: Semantic,  None       → classifier pins  → true
    //   slot 1: Episodic,  Some(true) → caller override  → true  (classifier would say false)
    //   slot 2: Episodic,  None       → classifier skips → false
    //   slot 3: Semantic,  Some(false)→ caller override  → false (classifier would say true)
    //   slot 4: Episodic,  None       → classifier skips → false
    let cases: [(&str, FactType, Option<bool>, bool); 5] = [
        ("auto-semantic-pinned", FactType::Semantic, None, true),
        ("caller-pin-episodic", FactType::Episodic, Some(true), true),
        ("auto-episodic-a", FactType::Episodic, None, false),
        (
            "caller-unpin-semantic",
            FactType::Semantic,
            Some(false),
            false,
        ),
        ("auto-episodic-b", FactType::Episodic, None, false),
    ];

    let entries: Vec<AddFactRequest> = cases
        .iter()
        .map(|(content, fact_type, pinned, _)| AddFactRequest {
            content: (*content).into(),
            fact_type: *fact_type,
            source_event_id: None,
            scope: None,
            opts: pinned.map(|p| AddFactOptions {
                pinned: Some(p),
                ..Default::default()
            }),
        })
        .collect();

    let ids = engine
        .add_facts_batch(
            &entries,
            embedder.clone(),
            Some(std::sync::Arc::new(PinSemantic) as std::sync::Arc<dyn PersistenceClassifier>),
        )
        .await
        .unwrap();
    assert_eq!(ids.len(), cases.len());

    for (id, (content, _, _, expected_pinned)) in ids.iter().zip(cases.iter()) {
        let fact = engine.get_fact(*id).await.unwrap();
        // The id-order must mirror the entry order, so each fact's content
        // confirms we are asserting against the matching slot.
        assert_eq!(
            fact.content, *content,
            "fact ids must preserve batch entry order"
        );
        assert_eq!(
            fact.is_pinned, *expected_pinned,
            "slot for {content:?} got is_pinned={} (expected {expected_pinned})",
            fact.is_pinned
        );
    }
}

#[tokio::test]
async fn add_facts_batch_rejects_embedding_count_mismatch() {
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
        .add_facts_batch(
            &entries,
            std::sync::Arc::new(BadBatchEmbedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("1 embeddings for 2 entries"),
        "expected count mismatch error, got: {err}"
    );
}

#[tokio::test]
async fn add_facts_batch_rollback_on_insert_failure() {
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
    let result = engine
        .add_facts_batch(
            &entries,
            std::sync::Arc::new(BadDimBatchEmbedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await;
    assert!(result.is_err());

    // Verify rollback: no facts should be in the DB
    let query = SearchQuery::new(SearchMode::Hybrid, 10)
        .text("rollback test")
        .embedding(vec![0.5; DIM]);
    let results = engine.query(&query).await.unwrap();
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
        .add_facts_batch(
            &good_entries,
            std::sync::Arc::new(good_embedder) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    assert_eq!(ids.len(), 1);
    let fact = engine.get_fact(ids[0]).await.unwrap();
    // If scopes were leaked, scope_id would already exist.
    // The fact that this succeeds proves the DB is consistent.
    assert_ne!(fact.scope_id, 1, "should be in a non-root scope");
}

#[tokio::test]
async fn add_facts_batch_temporal_consistency() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let entries: Vec<AddFactRequest> = (0..3)
        .map(|i| AddFactRequest {
            content: format!("temporal {i}"),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        })
        .collect();

    let ids = engine
        .add_facts_batch(&entries, embedder.clone(), None)
        .await
        .unwrap();

    // All facts should have the same t_created (within a reasonable window)
    let mut facts: Vec<Fact> = Vec::with_capacity(ids.len());
    for &id in &ids {
        facts.push(engine.get_fact(id).await.unwrap());
    }
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

#[tokio::test]
async fn record_outcome_returns_event_id() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let fact = make_new_fact("outcome target", vec![0.5; DIM]);
    let fact_id = insert_raw_fact(&engine, &fact).await;

    let event_id = engine
        .record_outcome(fact_id, crate::types::Outcome::Positive)
        .await
        .unwrap();
    assert!(event_id > 0);
}

#[tokio::test]
async fn record_outcome_nonexistent_fact_returns_not_found() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    let result = engine
        .record_outcome(999, crate::types::Outcome::Negative)
        .await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::NotFound(_)));
}

#[tokio::test]
async fn record_outcome_read_only_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Create DB with a fact
    let fact_id = {
        let engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
        let fact = make_new_fact("pinned for ro", vec![0.5; DIM]);
        insert_raw_fact(&engine, &fact).await
    };

    // Re-open read-only
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();

    let result = engine
        .record_outcome(fact_id, crate::types::Outcome::Positive)
        .await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::ReadOnly));
}

#[tokio::test]
async fn get_outcome_counts_nonexistent_fact_returns_not_found() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    let result = engine.get_outcome_counts(999).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), MemoryError::NotFound(_)));
}

#[tokio::test]
async fn get_outcome_counts_no_outcomes_returns_zeros() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let fact = make_new_fact("no outcomes", vec![0.5; DIM]);
    let fact_id = insert_raw_fact(&engine, &fact).await;

    let counts = engine.get_outcome_counts(fact_id).await.unwrap();
    assert_eq!(counts.positive, 0);
    assert_eq!(counts.negative, 0);
    assert_eq!(counts.neutral, 0);
}

#[tokio::test]
async fn get_outcome_counts_tallies_correctly() {
    use crate::types::Outcome;

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let fact = make_new_fact("tallied fact", vec![0.5; DIM]);
    let fact_id = insert_raw_fact(&engine, &fact).await;

    // Record mixed outcomes
    engine
        .record_outcome(fact_id, Outcome::Positive)
        .await
        .unwrap();
    engine
        .record_outcome(fact_id, Outcome::Positive)
        .await
        .unwrap();
    engine
        .record_outcome(fact_id, Outcome::Negative)
        .await
        .unwrap();
    engine
        .record_outcome(fact_id, Outcome::Neutral)
        .await
        .unwrap();
    engine
        .record_outcome(fact_id, Outcome::Positive)
        .await
        .unwrap();

    let counts = engine.get_outcome_counts(fact_id).await.unwrap();
    assert_eq!(counts.positive, 3);
    assert_eq!(counts.negative, 1);
    assert_eq!(counts.neutral, 1);
}

#[tokio::test]
async fn get_outcome_counts_isolates_per_fact() {
    use crate::types::Outcome;

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let f1 = insert_raw_fact(&engine, &make_new_fact("fact one", vec![0.5; DIM])).await;
    let f2 = insert_raw_fact(&engine, &make_new_fact("fact two", vec![0.3; DIM])).await;

    engine.record_outcome(f1, Outcome::Positive).await.unwrap();
    engine.record_outcome(f1, Outcome::Positive).await.unwrap();
    engine.record_outcome(f2, Outcome::Negative).await.unwrap();

    let c1 = engine.get_outcome_counts(f1).await.unwrap();
    assert_eq!(c1.positive, 2);
    assert_eq!(c1.negative, 0);

    let c2 = engine.get_outcome_counts(f2).await.unwrap();
    assert_eq!(c2.positive, 0);
    assert_eq!(c2.negative, 1);
}

#[tokio::test]
async fn get_outcome_counts_batch_matches_per_fact_loop() {
    use crate::types::Outcome;

    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let f1 = insert_raw_fact(&engine, &make_new_fact("batch one", vec![0.5; DIM])).await;
    let f2 = insert_raw_fact(&engine, &make_new_fact("batch two", vec![0.3; DIM])).await;
    let f3 = insert_raw_fact(
        &engine,
        &make_new_fact("batch three (no outcomes)", vec![0.1; DIM]),
    )
    .await;

    engine.record_outcome(f1, Outcome::Positive).await.unwrap();
    engine.record_outcome(f1, Outcome::Positive).await.unwrap();
    engine.record_outcome(f1, Outcome::Negative).await.unwrap();
    engine.record_outcome(f2, Outcome::Neutral).await.unwrap();
    // f3 deliberately has no outcomes.

    let nonexistent = 999_999;
    let ids = [f1, f2, f3, nonexistent];
    let batch = engine.get_outcome_counts_batch(&ids).await.unwrap();

    // Batch must equal the per-fact loop for every existing fact.
    for &id in &[f1, f2, f3] {
        let loop_counts = engine.get_outcome_counts(id).await.unwrap();
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

#[tokio::test]
async fn get_outcome_counts_batch_empty_input_no_query() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let batch = engine.get_outcome_counts_batch(&[]).await.unwrap();
    assert!(batch.is_empty());
}

#[tokio::test]
async fn outcome_serde_round_trip() {
    use crate::types::Outcome;

    for variant in [Outcome::Positive, Outcome::Negative, Outcome::Neutral] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: Outcome = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
}

#[tokio::test]
async fn outcome_display() {
    use crate::types::Outcome;

    assert_eq!(Outcome::Positive.to_string(), "positive");
    assert_eq!(Outcome::Negative.to_string(), "negative");
    assert_eq!(Outcome::Neutral.to_string(), "neutral");
}

// --- MemoryEngineBuilder (#113) ---

#[tokio::test]
async fn builder_in_memory_matches_open_memory() {
    // No `.path()` => in-memory engine, identical to `open_memory`.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert_eq!(engine.embed_dim, DIM);
    assert!(engine.reranker_name().is_none());
    // In-memory pool has no backing file.
    assert!(!engine.is_file_backed());
}

// NOTE (#541): #543's `builder_rejects_in_memory_read_only` (a RUNTIME check
// that `read_only` on an in-memory builder returns `Err`) was removed. The
// typestate builder makes that combination unrepresentable at COMPILE time —
// `read_only` exists only on `MemoryEngineBuilder<File>`. The guarantee is
// enforced by a `compile_fail` doctest in `engine::builder`.

#[tokio::test]
async fn builder_file_backed_matches_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("builder.db");
    let engine = MemoryEngine::builder(DIM).path(&path).build().unwrap();
    assert_eq!(engine.embed_dim, DIM);
    assert!(engine.is_file_backed());
    // The file was created on disk.
    assert!(path.exists());
}

#[tokio::test]
async fn builder_wires_reranker() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(ReverseReranker))
        .build()
        .unwrap();
    assert_eq!(engine.reranker_name(), Some("reverse"));
}

#[tokio::test]
async fn builder_wires_search_config() {
    let engine = MemoryEngine::builder(DIM)
        .search_config(SearchConfig { ann_threshold: 0 })
        .build()
        .unwrap();
    // TODO(#631-test): engine internal removed. The `search_config` is no longer a
    // field on `MemoryEngine` — after #631 it is consumed at build time into the
    // backend's open config (`with_open_config`) and has no public getter. The
    // assertion below observed that private field directly; there is no public
    // equivalent to re-point it at, so it is disabled. `build()` succeeding above
    // still exercises that the builder accepts and threads the search config.
    let _ = &engine;
    // assert_eq!(
    //     engine.search_config.as_ref().map(|c| c.ann_threshold),
    //     Some(0),
    //     "search_config should be threaded through build()"
    // );
}

#[tokio::test]
async fn builder_threads_upcaster_registry_in_memory() {
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

    // Ingest an Interaction event (the threaded registry stamps it at its latest
    // revision = 2), then downgrade its stored revision to 1 via the test-only
    // `raw_exec` seam (#727) so replay must upcast it back through the engine's
    // 1->2 upcaster. If the custom registry had been dropped (the #543 bug), the
    // engine's empty registry would stamp at revision 1, the UPDATE would be a
    // no-op, and replay would leave `upcasted` absent.
    let id = engine
        .ingest(&NewEvent {
            timestamp: chrono::Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({ "msg": "hello" }),
            source: "test".into(),
            session_id: Some("s1".into()),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        })
        .await
        .unwrap();

    engine
        .storage()
        .raw_exec(&format!(
            "UPDATE events SET event_revision = 1 WHERE id = {id}"
        ))
        .await
        .unwrap();

    let filter = crate::inspect::ReplayFilter {
        upcast: true,
        ..Default::default()
    };
    let events = engine.replay_events(&filter).await.unwrap();
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

#[tokio::test]
async fn builder_read_only_rejects_missing_file() {
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

#[tokio::test]
async fn builder_read_only_opens_existing_file() {
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
    assert!(engine.is_read_only());
}

#[tokio::test]
async fn builder_embed_dim_mismatch_is_rejected() {
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
                std::sync::Arc::new(MockEmbedder { dim: DIM })
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
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

#[tokio::test]
async fn add_fact_invalid_scope_path_returns_scope_label_conflict() {
    // An empty path segment ("a//b" → segments ["a", "", "b"]) is rejected by
    // `ScopeStore::validate_label` while resolving the scope inside `add_fact`'s
    // write lock. The error must surface verbatim at the engine boundary as a
    // typed `Conflict(ScopeLabel)` — not a generic Database/Internal error.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let err = engine
        .add_fact(
            &AddFactRequest {
                content: "fact with a malformed scope".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("a//b".into()),
                opts: None,
            },
            embedder.clone(),
            None,
        )
        .await
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
        engine.list_active_facts(None).await.unwrap().is_empty(),
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
            embedder.clone(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err_ws,
            MemoryError::Conflict(crate::error::ConflictError::ScopeLabel(_))
        ),
        "expected Conflict(ScopeLabel) for a leading-whitespace label, got {err_ws:?}"
    );
}

#[tokio::test]
async fn add_fact_rejects_out_of_range_importance() {
    // CONTRACT (issue #571, follow-up to #130): `AddFactOptions::importance` is
    // documented as "Must be in [0, 1]". `add_fact` now ENFORCES this loudly —
    // an out-of-range (or non-finite) value is rejected with
    // `Conflict(PolicyParameter)` BEFORE anything is persisted (no event, no
    // fact), rather than being silently stored verbatim. A valid [0, 1] value
    // still succeeds.
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> =
        std::sync::Arc::new(MockEmbedder { dim: DIM });

    let request = |importance: f64| AddFactRequest {
        content: format!("importance = {importance}"),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: Some(AddFactOptions {
            base_importance: Some(importance),
            ..Default::default()
        }),
    };

    // Above the range: rejected as Conflict(PolicyParameter), nothing persisted.
    let err_high = engine
        .add_fact(&request(5.0), embedder.clone(), None)
        .await
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
        .add_fact(&request(-0.1), embedder.clone(), None)
        .await
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
        engine.list_active_facts(None).await.unwrap().len(),
        0,
        "rejected out-of-range inserts must not persist any fact"
    );

    // In-range still works and is stored verbatim (base + materialized score).
    let id = engine
        .add_fact(&request(0.8), embedder.clone(), None)
        .await
        .expect("in-range importance 0.8 must succeed");
    let fact = engine.get_fact(id).await.unwrap();
    assert!(
        (fact.base_importance - 0.8).abs() < f64::EPSILON,
        "in-range importance stored verbatim: got {}",
        fact.base_importance
    );
    assert!(
        (fact.importance_score - 0.8).abs() < f64::EPSILON,
        "importance_score seeded from the base importance: got {}",
        fact.importance_score
    );
}

#[tokio::test]
async fn restore_json_corrupt_json_returns_serialization_error() {
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

#[tokio::test]
async fn restore_sqlite_non_sqlite_file_returns_database_error() {
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

#[tokio::test]
async fn restore_sqlite_missing_file_returns_not_found() {
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

    async fn add_test_fact(engine: &MemoryEngine, content: &str) -> i64 {
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
                std::sync::Arc::new(embedder) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn snapshot_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Create engine, add data, write snapshot
        {
            let mut engine = open_file_engine(dir.path());
            add_test_fact(&engine, "fact one").await;
            add_test_fact(&engine, "fact two").await;
            engine.close().await.unwrap();
        }

        // Verify snapshot file exists
        let snap_path = super::snapshot::snapshot_path(&db_path);
        assert!(snap_path.exists(), "snapshot file should exist");

        // Re-open — should load from snapshot
        let engine = open_file_engine(dir.path());
        let facts = engine.list_active_facts(None).await.unwrap();
        assert_eq!(facts.len(), 2);
    }

    #[tokio::test]
    async fn snapshot_fallback_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        // Create engine, add data, do NOT write snapshot
        {
            let engine = MemoryEngine::builder(DIM)
                .path(dir.path().join("test.db"))
                .build()
                .unwrap();
            add_test_fact(&engine, "fact one").await;
            // Remove snapshot if Drop wrote one
            let snap_path = super::snapshot::snapshot_path(&dir.path().join("test.db"));
            let _ = std::fs::remove_file(&snap_path);
        }

        // Re-open — should fall back to full rebuild
        let engine = open_file_engine(dir.path());
        let facts = engine.list_active_facts(None).await.unwrap();
        assert_eq!(facts.len(), 1);
    }

    #[tokio::test]
    async fn snapshot_fallback_on_stale_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let snap_path = super::snapshot::snapshot_path(&db_path);

        // Phase 1: create engine with one fact, write snapshot explicitly.
        {
            let mut engine = open_file_engine(dir.path());
            add_test_fact(&engine, "original fact").await;
            engine.close().await.unwrap();
        }

        // Save the snapshot bytes so we can restore them later.
        let snapshot_bytes = std::fs::read(&snap_path).unwrap();

        // Phase 2: add more data. Drop writes a fresh (updated) snapshot.
        {
            let engine = open_file_engine(dir.path());
            add_test_fact(&engine, "second fact").await;
            // Drop is warn-only post-#631 (no sidecar write); the on-disk snapshot
            // stays the phase-1 one, now stale against the 2-fact DB.
        }

        // Phase 3: overwrite the snapshot with the stale one from phase 1.
        std::fs::write(&snap_path, &snapshot_bytes).unwrap();

        // Phase 4: re-open — stale snapshot fingerprint should mismatch,
        // engine falls back to full rebuild and sees both facts.
        let engine = open_file_engine(dir.path());
        let facts = engine.list_active_facts(None).await.unwrap();
        assert_eq!(facts.len(), 2, "should see both facts via full rebuild");
    }

    #[tokio::test]
    async fn snapshot_skipped_for_memory_engine() {
        let mut engine = MemoryEngine::builder(DIM).build().unwrap();
        let result = engine.close().await.unwrap();
        assert!(!result, "in-memory engine should skip snapshot");
    }

    #[tokio::test]
    async fn snapshot_skipped_for_read_only() {
        let dir = tempfile::tempdir().unwrap();

        // First open in read-write to create the DB
        {
            let _engine = open_file_engine(dir.path());
        }

        // Open read-only
        let mut engine = MemoryEngine::builder(DIM)
            .path(dir.path().join("test.db"))
            .read_only(true)
            .build()
            .unwrap();
        let result = engine.close().await.unwrap();
        assert!(!result, "read-only engine should skip snapshot");
    }

    /// Post-#631 contract: the sidecar flush moved to the async `close()`; `Drop`
    /// is warn-only and writes nothing (it cannot run an `async` port method). A
    /// file-backed engine dropped without `close()` therefore leaves NO snapshot —
    /// the in-memory projections are rebuilt from the DB on next open (correct, just
    /// slower). This is the documented behavior change.
    #[tokio::test]
    async fn drop_without_close_does_not_write_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let snap_path = super::snapshot::snapshot_path(&db_path);

        {
            let engine = open_file_engine(dir.path());
            add_test_fact(&engine, "will NOT be snapshotted").await;
            // Drop without close(): no sidecar is written.
        }

        assert!(
            !snap_path.exists(),
            "Drop must NOT write the sidecar snapshot (only close() flushes it now)"
        );

        // The data is still recoverable on re-open via the full DB rebuild.
        let engine = open_file_engine(dir.path());
        assert_eq!(engine.list_active_facts(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn snapshot_and_full_rebuild_agree() {
        let dir = tempfile::tempdir().unwrap();

        // Create engine with data
        {
            let mut engine = open_file_engine(dir.path());
            add_test_fact(&engine, "alpha").await;
            add_test_fact(&engine, "beta").await;
            add_test_fact(&engine, "gamma").await;
            engine.close().await.unwrap();
        }

        // Load from snapshot
        let engine_snap = open_file_engine(dir.path());
        let snap_facts = engine_snap.list_active_facts(None).await.unwrap();
        let snap_graph_nodes = engine_snap.graph.read().node_count();
        let snap_graph_edges = engine_snap.graph.read().edge_count();

        // Delete snapshot, force full rebuild
        let snap_path = super::snapshot::snapshot_path(&dir.path().join("test.db"));
        std::fs::remove_file(&snap_path).unwrap();
        let engine_rebuild = open_file_engine(dir.path());
        let rebuild_facts = engine_rebuild.list_active_facts(None).await.unwrap();
        let rebuild_graph_nodes = engine_rebuild.graph.read().node_count();
        let rebuild_graph_edges = engine_rebuild.graph.read().edge_count();

        assert_eq!(snap_facts.len(), rebuild_facts.len());
        assert_eq!(snap_graph_nodes, rebuild_graph_nodes);
        assert_eq!(snap_graph_edges, rebuild_graph_edges);
    }

    // --- #742 Phase 1: the reconstruction dimension fence ---

    #[tokio::test]
    async fn fence_blocks_embedding_touching_ops_until_reopen() {
        // A handle fenced by a different-dim reconstruction refuses embedding-touching
        // reads/writes with the actionable EmbeddingReopenRequired, while
        // dimension-independent accessors keep working.
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let embedder: std::sync::Arc<dyn EmbeddingProvider> =
            std::sync::Arc::new(MockEmbedder { dim: DIM });
        let req = |content: &str| AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        let id = engine
            .add_fact(&req("before fence"), embedder.clone(), None)
            .await
            .unwrap();

        // Not fenced yet.
        assert_eq!(engine.reopen_required(), None);

        // Arm the fence at a new dimension (simulating a different-dim promote).
        engine.force_reopen_fence(8);
        assert_eq!(engine.reopen_required(), Some(8));

        // Representative gated reads/writes now refuse with the actionable error.
        assert!(matches!(
            engine.get_fact(id).await,
            Err(MemoryError::EmbeddingReopenRequired { new_dim: 8 })
        ));
        assert!(matches!(
            engine.list_active_facts(None).await,
            Err(MemoryError::EmbeddingReopenRequired { new_dim: 8 })
        ));
        assert!(matches!(
            engine
                .execute_query(&MemoryQuery::new().text("before"))
                .await,
            Err(MemoryError::EmbeddingReopenRequired { new_dim: 8 })
        ));
        assert!(matches!(
            engine
                .add_fact(&req("after fence"), embedder.clone(), None)
                .await,
            Err(MemoryError::EmbeddingReopenRequired { new_dim: 8 })
        ));

        // Dimension-independent accessors are NOT fenced.
        assert_eq!(engine.embed_dim(), DIM);
        assert_eq!(engine.reopen_required(), Some(8));
        // A fenced flush is a clean no-op (not an error).
        assert!(!engine.flush_snapshot().await.unwrap());
    }

    /// The headline #742 test: a file-backed engine reconstructed to a NEW
    /// dimension, then reopened at that dimension serves the new vectors.
    #[tokio::test]
    async fn reconstruct_different_dim_then_reopen_at_new_dim() {
        const D2: usize = 8; // DIM (4) → 8
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("recon.db");
        let req = |content: &str| AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        };

        // Session 1: ingest @ DIM, reconstruct to D2, get fenced, flush, drop.
        let mut ids = Vec::new();
        {
            let engine =
                MemoryEngine::open_from_config(&EngineConfig::new(db_path.clone(), DIM), None)
                    .unwrap();
            let old: std::sync::Arc<dyn EmbeddingProvider> =
                std::sync::Arc::new(MockEmbedder { dim: DIM });
            for c in ["alpha", "beta", "gamma"] {
                ids.push(engine.add_fact(&req(c), old.clone(), None).await.unwrap());
            }

            let new_provider: std::sync::Arc<dyn EmbeddingProvider> =
                std::sync::Arc::new(MockEmbedder { dim: D2 });
            let new_fp = EmbeddingFingerprint::new("mock", "test", D2);
            let outcome = engine.reconstruct(&new_fp, &new_provider).await.unwrap();
            assert_eq!(outcome.promoted, 3);
            assert_eq!(outcome.new_fingerprint.dim, D2);

            // Fenced: reads refuse, flush is a clean no-op.
            assert_eq!(engine.reopen_required(), Some(D2));
            assert!(matches!(
                engine.get_fact(ids[0]).await,
                Err(MemoryError::EmbeddingReopenRequired { new_dim: D2 })
            ));
            assert!(!engine.flush_snapshot().await.unwrap());
        }

        // Session 2: reopen AT THE NEW DIM — validates clean (meta now D2), rebuilds
        // the index @ D2, and serves the new D2-wide vectors.
        {
            let engine =
                MemoryEngine::open_from_config(&EngineConfig::new(db_path.clone(), D2), None)
                    .unwrap();
            assert_eq!(engine.embed_dim(), D2);
            assert_eq!(
                engine.reopen_required(),
                None,
                "a fresh handle is not fenced"
            );
            for &id in &ids {
                assert_eq!(
                    engine.get_fact(id).await.unwrap().embedding,
                    vec![0.5_f32; D2],
                    "facts.embedding now serves the new D2-wide vectors"
                );
            }
            assert_eq!(
                engine
                    .storage()
                    .load_embedding_fingerprint()
                    .await
                    .unwrap()
                    .unwrap()
                    .dim,
                D2
            );
        }

        // Reopening at the OLD dim is now rejected (the recorded identity is D2).
        let err =
            MemoryEngine::open_from_config(&EngineConfig::new(db_path, DIM), None).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::Migration(MigrationError::EmbedDimMismatch { stored, requested })
                    if stored == D2 && requested == DIM
            ),
            "cold reopen at the old dim must report EmbedDimMismatch, got {err:?}"
        );
    }

    /// Fence coverage: every cheap-to-call gated method refuses on a fenced handle.
    /// (The full 26-method gated set is verified at the source; this exercises the
    /// representative surface across reads/writes/inspection so a regression that
    /// drops a guard is caught.)
    #[tokio::test]
    async fn fence_covers_representative_gated_surface() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let embedder: std::sync::Arc<dyn EmbeddingProvider> =
            std::sync::Arc::new(MockEmbedder { dim: DIM });
        let req = |content: &str| AddFactRequest {
            content: content.into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        };
        engine
            .add_fact(&req("seed"), embedder.clone(), None)
            .await
            .unwrap();
        engine.force_reopen_fence(8);

        let fenced = |r: Result<()>| {
            assert!(matches!(
                r,
                Err(MemoryError::EmbeddingReopenRequired { new_dim: 8 })
            ));
        };
        fenced(engine.get_fact(1).await.map(|_| ()));
        fenced(engine.list_active_facts(None).await.map(|_| ()));
        fenced(
            engine
                .list_summaries(&ConsolidationLevel::Cluster)
                .await
                .map(|_| ()),
        );
        fenced(
            engine
                .execute_query(&MemoryQuery::new().text("x"))
                .await
                .map(|_| ()),
        );
        fenced(
            engine
                .add_fact(&req("blocked"), embedder.clone(), None)
                .await
                .map(|_| ()),
        );
        fenced(engine.explain_fact(1).await.map(|_| ()));
        fenced(engine.fact_history(1).await.map(|_| ()));
        fenced(engine.statistics().await.map(|_| ()));
        fenced(engine.forget(&ForgetPolicy::default()).await.map(|_| ()));
        fenced(
            engine
                .resume_context(&ResumeConfig::default())
                .await
                .map(|_| ()),
        );
    }
}

/// End-to-end coverage for the `include_expired_probe` diagnostics contract
/// (issue #324). These tests drive the full `execute_query` path — the only
/// place that calls `lexical_count_expired` and writes
/// `QueryDiagnostics::expired_matches` — rather than just the builder setter.
///
/// The contract (engine/query.rs, search/hybrid.rs): `expired_matches` is
/// `Some(count)` ONLY when the probe is opted in AND the query carries text;
/// it stays `None` when the probe is off, and `None` for a vector-only query
/// (no FTS5 terms to probe — the documented limitation in `search/hybrid.rs`).
mod expired_probe_e2e {
    use super::*;

    /// Add a fact through the engine's normal write path (which indexes it into
    /// FTS5, so the expired probe's `MATCH` query can find it) and return its id.
    async fn add_indexed_fact(engine: &MemoryEngine, content: &str) -> i64 {
        let embedder: std::sync::Arc<dyn EmbeddingProvider> =
            std::sync::Arc::new(MockEmbedder { dim: DIM });
        engine
            .add_fact(
                &AddFactRequest {
                    content: content.into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                embedder,
                None,
            )
            .await
            .unwrap()
    }

    /// A text query with the probe enabled must populate `expired_matches` with
    /// a non-zero count once a matching fact has been expired.
    #[tokio::test]
    async fn text_query_with_probe_counts_expired_matches() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();

        // Two facts share the keyword; one stays active, one gets expired.
        add_indexed_fact(&engine, "quasar emits intense radiation").await;
        let expired_id = add_indexed_fact(&engine, "quasar discovered last year").await;
        engine
            .storage()
            .expire_fact(expired_id, Utc::now() - chrono::Duration::hours(1))
            .await
            .unwrap();

        let query = MemoryQuery::new().text("quasar").include_expired_probe();
        let response = engine.execute_query(&query).await.unwrap();

        // The probe ran (Some) and saw exactly the one expired match.
        assert_eq!(
            response.diagnostics.expired_matches,
            Some(1),
            "probe must report the single expired 'quasar' fact"
        );
        // Only the still-active fact is returned in results.
        assert_eq!(response.results.len(), 1);
        assert!(response.results[0].fact.content.contains("intense"));
    }

    /// Without `include_expired_probe`, the probe must not run: `expired_matches`
    /// stays `None` even when matching expired facts exist.
    #[tokio::test]
    async fn text_query_without_probe_leaves_expired_matches_none() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();

        add_indexed_fact(&engine, "pulsar spins rapidly").await;
        let expired_id = add_indexed_fact(&engine, "pulsar timing glitch").await;
        engine
            .storage()
            .expire_fact(expired_id, Utc::now() - chrono::Duration::hours(1))
            .await
            .unwrap();

        // Same query, probe NOT enabled.
        let query = MemoryQuery::new().text("pulsar");
        let response = engine.execute_query(&query).await.unwrap();

        assert_eq!(
            response.diagnostics.expired_matches, None,
            "probe must stay off when not opted in"
        );
    }

    /// A vector-only query (no text) with the probe set must still leave
    /// `expired_matches` as `None` — there are no FTS5 terms to probe. This is
    /// the documented limitation in `search/hybrid.rs`.
    #[tokio::test]
    async fn vector_only_query_with_probe_leaves_expired_matches_none() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();

        add_indexed_fact(&engine, "nebula glows faintly").await;
        let expired_id = add_indexed_fact(&engine, "nebula collapses inward").await;
        engine
            .storage()
            .expire_fact(expired_id, Utc::now() - chrono::Duration::hours(1))
            .await
            .unwrap();

        // No text — embedding only — yet the probe flag is set.
        let query = MemoryQuery::new()
            .embedding(vec![0.5; DIM])
            .include_expired_probe();
        let response = engine.execute_query(&query).await.unwrap();

        assert_eq!(
            response.diagnostics.expired_matches, None,
            "vector-only query has no FTS5 terms — probe must be a no-op"
        );
    }

    /// The probe must be FTS-restricted to the query term, not a blanket count of
    /// all expired facts. Expire a fact that does NOT match the query term, then
    /// probe for a different term: `Some(0)` proves the probe RAN (not `None`) yet
    /// found zero matches (not `>= 1`) because the expired fact is off-term.
    #[tokio::test]
    async fn probe_is_fts_restricted_to_query_term() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();

        // The expired fact shares no keyword with the query.
        let expired_id = add_indexed_fact(&engine, "pulsar spins rapidly").await;
        engine
            .storage()
            .expire_fact(expired_id, Utc::now() - chrono::Duration::hours(1))
            .await
            .unwrap();

        // Probe for a DIFFERENT term — no expired 'quasar' fact exists.
        let query = MemoryQuery::new().text("quasar").include_expired_probe();
        let response = engine.execute_query(&query).await.unwrap();

        // Some(0): the probe ran (not None) but is FTS-restricted to 'quasar',
        // so the off-term expired 'pulsar' fact is not counted (not >= 1).
        assert_eq!(
            response.diagnostics.expired_matches,
            Some(0),
            "probe must run yet be restricted to the query term, not count all expired facts"
        );
    }
}
