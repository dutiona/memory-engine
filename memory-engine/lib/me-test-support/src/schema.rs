//! `SchemaManager` lifecycle + embedding-fingerprint identity contracts.

use me_storage::LexicalRanker;
use me_types::error::MemoryError;
use me_types::types::EmbeddingFingerprint;

use super::factory::ConformanceBackend;
use super::fixtures::{DIM, fingerprint};

/// `schema_version() >= 1` and `migrate()` is idempotent at HEAD.
pub async fn schema_version_and_migrate_idempotent<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    assert!(
        be.schema_version().await.expect("schema_version") >= 1,
        "[{}] schema_version must be >= 1",
        f.name()
    );
    be.migrate().await.expect("migrate at HEAD");
    be.migrate().await.expect("migrate idempotent at HEAD");
}

/// `validate_schema_version()` succeeds on a freshly-migrated store.
pub async fn validate_schema_version_ok_on_fresh<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    be.validate_schema_version()
        .await
        .expect("validate ok on fresh store");
}

/// `capabilities()` is **synchronous** and self-consistent: `true_idf ⇔ (ranker ==
/// Bm25)`. The SQLite-specific tier VALUES stay per-backend golden — NOT asserted here.
pub async fn capabilities_self_consistent<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let caps = be.capabilities(); // sync — must NOT be awaited
    assert_eq!(
        caps.true_idf,
        caps.lexical_ranker == LexicalRanker::Bm25,
        "[{}] capabilities must satisfy true_idf ⇔ (lexical_ranker == Bm25)",
        f.name()
    );
}

/// Config K/V round-trips; an absent key reads as `None`.
pub async fn config_round_trip<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    assert!(
        be.get_config("absent-key")
            .await
            .expect("get absent")
            .is_none(),
        "[{}] absent config key must read None",
        f.name()
    );
    be.set_config("conf-key", "conf-val")
        .await
        .expect("set_config");
    assert_eq!(
        be.get_config("conf-key").await.expect("get"),
        Some("conf-val".to_owned()),
        "[{}] config must round-trip",
        f.name()
    );
}

/// `set_config` on a read-only backend yields `ReadOnly` (a `SchemaManager` write path
/// distinct from the `FactGraph` one — the review-named gap).
pub async fn set_config_on_read_only_yields_read_only<F: ConformanceBackend>(f: &F) {
    let be = f.make_read_only().await;
    let err = be
        .set_config("k", "v")
        .await
        .expect_err("set_config on read-only must be rejected");
    assert!(
        matches!(err, MemoryError::ReadOnly),
        "[{}] set_config on read-only must be ReadOnly, got {err:?}",
        f.name()
    );
}

// --- embedding fingerprint identity (4 split contracts) ---

/// `record_if_absent` records the candidate when absent, then returns the STORED
/// identity (ignoring a later, different candidate).
pub async fn fingerprint_record_if_absent_records_then_returns_stored<F: ConformanceBackend>(
    f: &F,
) {
    let be = f.make().await;
    let stored = be
        .record_embedding_fingerprint_if_absent(&fingerprint(), DIM)
        .await
        .expect("record when absent");
    assert_eq!(
        stored,
        fingerprint(),
        "[{}] record_if_absent must record the candidate when absent",
        f.name()
    );
    // The SAME candidate again returns the stored identity (idempotent). A DIFFERENT
    // candidate is rejected, not silently accepted — that path is
    // `fingerprint_model_mismatch_rejected`.
    let stored2 = be
        .record_embedding_fingerprint_if_absent(&fingerprint(), DIM)
        .await
        .expect("record_if_absent must be idempotent when the candidate matches");
    assert_eq!(
        stored2,
        fingerprint(),
        "[{}] record_if_absent must return the STORED identity when the candidate matches",
        f.name()
    );
}

/// `record_if_absent` with `candidate.dim != expected_dim` ⇒ `EmbeddingDimension`.
pub async fn fingerprint_dim_mismatch_is_embedding_dimension<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let wrong_dim = EmbeddingFingerprint::new("conformance-model", "test", DIM + 1);
    let err = be
        .record_embedding_fingerprint_if_absent(&wrong_dim, DIM)
        .await
        .expect_err("dim mismatch must be rejected");
    assert!(
        matches!(err, MemoryError::EmbeddingDimension { .. }),
        "[{}] candidate.dim != expected_dim must be EmbeddingDimension, got {err:?}",
        f.name()
    );
}

/// After an identity is recorded, `check_embedding_compatible` rejects a different
/// model with `EmbeddingModelMismatch` (the #614 eager fail-fast).
pub async fn fingerprint_model_mismatch_rejected<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    be.record_embedding_fingerprint_if_absent(&fingerprint(), DIM)
        .await
        .expect("establish identity");
    let different = EmbeddingFingerprint::new("other-model", "test", DIM);
    let err = be
        .check_embedding_compatible(&different)
        .await
        .expect_err("model mismatch must be rejected (#614)");
    assert!(
        matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
        "[{}] a mismatched model must be EmbeddingModelMismatch, got {err:?}",
        f.name()
    );
}

/// `require_embedding_fingerprint_present` on a fresh store ⇒ `Internal` (the
/// open-time identity guard).
pub async fn require_present_on_fresh_is_internal<F: ConformanceBackend>(f: &F) {
    let be = f.make().await;
    let err = be
        .require_embedding_fingerprint_present()
        .await
        .expect_err("fresh store has no recorded fingerprint");
    assert!(
        matches!(err, MemoryError::Internal(_)),
        "[{}] require_present on a fresh store must be Internal, got {err:?}",
        f.name()
    );
}
