//! `impl SchemaManager for SqliteBackend` — delegates to the concrete schema
//! functions in [`crate::store::schema`] and [`crate::store::embedding_meta`].
//!
//! ## `migrate` / `backup_dir`
//!
//! The concrete `store::schema::migrate(conn, backup_dir: Option<&Path>)` accepts
//! an optional WAL-safe backup directory. At `SqliteBackend` level we pass `None`:
//! the pool already ran `init_schema` + `migrate` at open time, so calling
//! `SchemaManager::migrate` is an idempotent at-HEAD re-check, not a first-time
//! migration. No backup is taken here; the consumer is responsible for taking a
//! backup before calling the pool's open path if one is needed. If a future
//! engine path requires a backup at this level, this method must be extended and
//! that decision surfaced to the caller — do not guess.
//!
//! ## `capabilities`
//!
//! **Synchronous** — fixed at open. `SQLite` tier: `Bm25` ranker (FTS5 `bm25()`),
//! `true_idf = true`, `server_side_vector = false` (brute-force in-process scan;
//! HNSW moves into the backend in #631).

use async_trait::async_trait;

use super::SqliteBackend;
use crate::error::Result;
use crate::storage::capabilities::{BackendCapabilities, LexicalRanker};
use crate::storage::schema::SchemaManager;
use crate::store::embedding_meta;
use crate::store::schema::{get_config, migrate, validate_schema_version};
use crate::types::EmbeddingFingerprint;

#[async_trait]
impl SchemaManager for SqliteBackend {
    // WRITE (idempotent at HEAD — see module-level doc)
    async fn migrate(&self) -> Result<()> {
        self.block_write(|c| migrate(c, None)).await
    }

    // READ
    async fn schema_version(&self) -> Result<u32> {
        self.block_read(|c| {
            let raw = get_config(c, "schema_version")?.unwrap_or_else(|| "1".to_string());
            raw.parse::<u32>().map_err(|_| {
                crate::error::MemoryError::Migration(crate::error::MigrationError::Incompatible(
                    format!("invalid schema_version: {raw}"),
                ))
            })
        })
        .await
    }

    // READ
    async fn validate_schema_version(&self) -> Result<()> {
        self.block_read(validate_schema_version).await
    }

    /// `SQLite` capabilities: BM25 lexical ranker (FTS5), true corpus IDF,
    /// in-process vector search (not server-side). Fixed at open.
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            lexical_ranker: LexicalRanker::Bm25,
            server_side_vector: false,
            true_idf: true,
        }
    }

    // -------------------------------------------------------------------------
    // embedding-fingerprint identity
    // -------------------------------------------------------------------------

    // READ
    async fn load_embedding_fingerprint(&self) -> Result<Option<EmbeddingFingerprint>> {
        self.block_read(embedding_meta::load).await
    }

    // WRITE
    async fn store_embedding_fingerprint(&self, fp: &EmbeddingFingerprint) -> Result<()> {
        let fp = fp.clone();
        self.block_write(move |c| embedding_meta::store(c, &fp))
            .await
    }

    // WRITE
    async fn record_embedding_fingerprint_if_absent(
        &self,
        candidate: &EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<EmbeddingFingerprint> {
        let candidate = candidate.clone();
        self.block_write(move |c| embedding_meta::record_if_absent(c, &candidate, expected_dim))
            .await
    }

    // READ
    async fn require_embedding_fingerprint_present(&self) -> Result<()> {
        self.block_read(embedding_meta::require_present).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::SqliteBackend;
    use crate::error::MemoryError;
    use crate::pool::ConnectionPool;
    use crate::storage::capabilities::{BackendCapabilities, LexicalRanker};
    use crate::storage::schema::SchemaManager;
    use crate::store::schema::CURRENT_SCHEMA_VERSION;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::EmbeddingFingerprint;

    const DIM: usize = 4;

    fn backend() -> SqliteBackend {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    #[tokio::test]
    async fn schema_version_returns_current() {
        let be = backend();
        let v = be.schema_version().await.unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn migrate_is_idempotent_at_head() {
        let be = backend();
        // Should succeed without error on an already-at-HEAD schema.
        be.migrate().await.unwrap();
        // Calling twice is a no-op.
        be.migrate().await.unwrap();
        assert_eq!(be.schema_version().await.unwrap(), CURRENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn validate_schema_version_ok_on_fresh_db() {
        let be = backend();
        be.validate_schema_version().await.unwrap();
    }

    #[test]
    fn capabilities_sqlite_tier() {
        let be = backend();
        let caps: BackendCapabilities = be.capabilities();
        assert_eq!(caps.lexical_ranker, LexicalRanker::Bm25);
        assert!(caps.true_idf, "SQLite FTS5 uses true IDF");
        assert!(
            !caps.server_side_vector,
            "SQLite uses in-process vector scan"
        );
    }

    // -------------------------------------------------------------------------
    // embedding fingerprint
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn require_present_on_fresh_store_yields_internal() {
        let be = backend();
        let err = be
            .require_embedding_fingerprint_present()
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::Internal(_)),
            "expected Internal error on fresh store, got {err:?}"
        );
    }

    #[tokio::test]
    async fn record_if_absent_dim_mismatch_yields_embedding_dimension() {
        let be = backend();
        let fp = EmbeddingFingerprint::new("model-a", "tei", 16); // dim 16 ≠ expected 4
        let err = be
            .record_embedding_fingerprint_if_absent(&fp, DIM)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension {
                    expected: DIM,
                    actual: 16
                }
            ),
            "expected EmbeddingDimension, got {err:?}"
        );
        // Nothing persisted.
        assert!(be.load_embedding_fingerprint().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn record_store_load_round_trip() {
        let be = backend();
        assert!(be.load_embedding_fingerprint().await.unwrap().is_none());

        let fp = EmbeddingFingerprint::new("Qwen/Qwen3-Embedding-0.6B", "tei", DIM);
        let returned = be
            .record_embedding_fingerprint_if_absent(&fp, DIM)
            .await
            .unwrap();
        assert_eq!(returned, fp);

        let loaded = be.load_embedding_fingerprint().await.unwrap().unwrap();
        assert_eq!(loaded, fp);

        // require_present now succeeds.
        be.require_embedding_fingerprint_present().await.unwrap();
    }

    #[tokio::test]
    async fn store_then_load_overwrite() {
        let be = backend();
        let fp1 = EmbeddingFingerprint::new("model-a", "tei", DIM);
        let fp2 = EmbeddingFingerprint::new("model-b", "ollama", DIM);
        be.store_embedding_fingerprint(&fp1).await.unwrap();
        be.store_embedding_fingerprint(&fp2).await.unwrap();
        let loaded = be.load_embedding_fingerprint().await.unwrap().unwrap();
        assert_eq!(loaded, fp2);
    }

    #[tokio::test]
    async fn record_if_absent_returns_stored_when_candidate_matches() {
        let be = backend();
        let fp = EmbeddingFingerprint::new("model-a", "tei", DIM);
        be.store_embedding_fingerprint(&fp).await.unwrap();
        // A matching candidate is idempotent: the stored identity is returned.
        let returned = be
            .record_embedding_fingerprint_if_absent(&fp, DIM)
            .await
            .unwrap();
        assert_eq!(returned, fp);
    }

    #[tokio::test]
    async fn record_if_absent_rejects_model_mismatch() {
        // #614: a candidate that differs from the stored identity is rejected, not
        // silently ignored. The seam preserves the semantic variant (not opacified
        // to Storage(Backend), since it is not a raw driver error).
        let be = backend();
        let fp_a = EmbeddingFingerprint::new("model-a", "tei", DIM);
        let fp_b = EmbeddingFingerprint::new("model-b", "tei", DIM);
        be.store_embedding_fingerprint(&fp_a).await.unwrap();
        let err = be
            .record_embedding_fingerprint_if_absent(&fp_b, DIM)
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
            "got {err:?}"
        );
    }
}
