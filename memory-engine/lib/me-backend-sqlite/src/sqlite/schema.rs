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
// Un-gated (#1000): the fingerprint-write trait methods below now open explicit
// transactions and map their errors via `StorageError::backend` unconditionally, so this
// import is no longer test-util-only.
use me_types::error::StorageError;

use super::SqliteBackend;
use crate::store::schema::{get_config, migrate, validate_schema_version};
use crate::store::{embedding_meta, embedding_spaces, fact_vectors};
use me_storage::capabilities::{BackendCapabilities, LexicalRanker};
use me_storage::schema::SchemaManager;
use me_types::error::Result;
use me_types::types::{EmbeddingFingerprint, EmbeddingSpace, PromoteOutcome, SpaceStatus};

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
                me_types::error::MemoryError::Migration(
                    me_types::error::MigrationError::Incompatible(format!(
                        "invalid schema_version: {raw}"
                    )),
                )
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
        self.block_write(move |c| {
            // #1000: `embedding_meta::store` re-stamps the active registry row via a
            // two-statement UPDATE-or-INSERT. Run it in an explicit transaction so its
            // atomicity is enforced by the `&Transaction` type, not merely by the
            // block_write write-lock. Behavior-preserving: at a block_write boundary the
            // connection is fresh autocommit, so this can never nest.
            let tx = c.unchecked_transaction().map_err(StorageError::backend)?;
            embedding_meta::store(&tx, &fp)?;
            tx.commit().map_err(StorageError::backend)?;
            Ok(())
        })
        .await
    }

    // WRITE
    async fn record_embedding_fingerprint_if_absent(
        &self,
        candidate: &EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<EmbeddingFingerprint> {
        let candidate = candidate.clone();
        self.block_write(move |c| {
            // #1000: see `store_embedding_fingerprint` — the write path re-stamps the
            // registry, so wrap the record-if-absent in an explicit transaction.
            let tx = c.unchecked_transaction().map_err(StorageError::backend)?;
            let recorded = embedding_meta::record_if_absent(&tx, &candidate, expected_dim)?;
            tx.commit().map_err(StorageError::backend)?;
            Ok(recorded)
        })
        .await
    }

    // READ
    async fn require_embedding_fingerprint_present(&self) -> Result<()> {
        self.block_read(embedding_meta::require_present).await
    }

    // -------------------------------------------------------------------------
    // Stage A config accessors
    // -------------------------------------------------------------------------

    // READ
    async fn get_config(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_owned();
        self.block_read(move |c| get_config(c, &key)).await
    }

    // WRITE
    async fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let key = key.to_owned();
        let value = value.to_owned();
        self.block_write(move |c| crate::store::schema::set_config(c, &key, &value))
            .await
    }

    // -------------------------------------------------------------------------
    // Stage E snapshot seam — assemble the sidecar below the port.
    //
    // Relocates `engine/mod.rs::write_snapshot`: the engine hands down its two
    // in-memory projections; the backend folds in its own DB fingerprint and
    // HNSW snapshot (both backend-private). Runs the fingerprint read + HNSW
    // serialization + file write on one blocking thread via `block_read` (the
    // HNSW `to_snapshot` reuses the same read connection — no nested pool
    // acquisition). Returns `Ok(false)` for in-memory or read-only backends,
    // matching the engine's prior behavior exactly.
    async fn write_engine_snapshot(
        &self,
        graph: me_types::types::snapshot::GraphSnapshot,
        scope_tree: me_types::types::snapshot::ScopeTreeSnapshot,
    ) -> Result<bool> {
        use crate::snapshot;

        let Some(db_path) = self.pool.path().map(std::path::Path::to_path_buf) else {
            return Ok(false); // in-memory engine — no sidecar
        };
        if self.pool.is_read_only() {
            return Ok(false); // read-only open never writes a sidecar
        }

        let embed_dim = self.embed_dim;
        #[cfg(feature = "ann")]
        let hnsw = self.hnsw.clone();

        self.block_read(move |conn| {
            let fingerprint = snapshot::read_fingerprint(conn)?;

            #[cfg(feature = "ann")]
            let hnsw_snap = hnsw
                .as_ref()
                .map(|h| h.to_snapshot(conn, embed_dim))
                .transpose()?;
            #[cfg(not(feature = "ann"))]
            let hnsw_snap: Option<me_types::types::snapshot::HnswSnapshot> = None;

            let header = me_types::types::snapshot::SnapshotHeader {
                format_version: snapshot::FORMAT_VERSION,
                fingerprint,
                embed_dim,
                engine_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            let payload = me_types::types::snapshot::SnapshotPayload {
                graph,
                scope_tree,
                hnsw: hnsw_snap,
            };
            snapshot::write_to_file(&header, &payload, &snapshot::snapshot_path(&db_path))?;
            Ok(true)
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Stage E inspection ports — relocate the raw-`&Connection` `crate::inspect`
    // free functions below the seam. The backend sources its own db path; the
    // JSON dumps run on a read connection, `VACUUM INTO` on the write connection.
    // -------------------------------------------------------------------------

    // READ
    async fn statistics(&self) -> Result<me_types::types::inspect::EngineStatistics> {
        let db_path = self.pool.path().map(std::path::Path::to_path_buf);
        self.block_read(move |c| {
            crate::inspect::statistics::compute_statistics(c, db_path.as_deref())
        })
        .await
    }

    // READ (JSON variants) / WRITE (`VACUUM INTO`)
    async fn dump_state(
        &self,
        embed_dim: usize,
        format: me_types::types::inspect::DumpFormat,
    ) -> Result<()> {
        use crate::inspect::dump;
        use me_types::types::inspect::DumpFormat;

        match format {
            DumpFormat::Json(path) => {
                self.block_read(move |c| dump::dump_json(c, embed_dim, &path))
                    .await
            }
            #[cfg(feature = "compress-gzip")]
            DumpFormat::JsonGzip(path) => {
                self.block_read(move |c| dump::dump_json_gzip(c, embed_dim, &path))
                    .await
            }
            #[cfg(not(feature = "compress-gzip"))]
            DumpFormat::JsonGzip(_) => Err(me_types::error::MemoryError::NotImplemented(
                "gzip compression requires the `compress-gzip` feature".into(),
            )),
            #[cfg(feature = "compress-zstd")]
            DumpFormat::JsonZstd(path) => {
                self.block_read(move |c| dump::dump_json_zstd(c, embed_dim, &path))
                    .await
            }
            #[cfg(not(feature = "compress-zstd"))]
            DumpFormat::JsonZstd(_) => Err(me_types::error::MemoryError::NotImplemented(
                "zstd compression requires the `compress-zstd` feature".into(),
            )),
            DumpFormat::Sqlite(path) => {
                self.block_write(move |c| dump::dump_sqlite(c, &path)).await
            }
            // `DumpFormat` is `#[non_exhaustive]` (now defined in `me-types`, a
            // different crate, so the compiler enforces this even though every
            // current variant is covered above): a future variant added there
            // surfaces as a clear error here instead of a compile break.
            _ => Err(me_types::error::MemoryError::NotImplemented(
                "unrecognized DumpFormat variant".into(),
            )),
        }
    }

    // READ
    async fn check_embedding_compatible(&self, candidate: &EmbeddingFingerprint) -> Result<()> {
        let candidate = candidate.clone();
        self.block_read(move |c| embedding_meta::check_compatible(c, &candidate))
            .await
    }

    // TEST-ONLY raw SQL escape (#727) — `execute_batch` on the write connection, so
    // a read-only pool rejects it with `MemoryError::ReadOnly` and a driver error
    // is mapped to `Storage(Backend)` by the seam.
    #[cfg(feature = "test-util")]
    async fn raw_exec(&self, sql: &str) -> Result<()> {
        let sql = sql.to_owned();
        self.block_write(move |c| {
            c.execute_batch(&sql)
                .map_err(|e| StorageError::backend(e).into())
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Background reconstruction (#623) — delegate to the registry seam +
    // `store::fact_vectors` free functions via the block_read/block_write boundary.
    // -------------------------------------------------------------------------

    // WRITE
    async fn begin_populating_space(
        &self,
        name: &str,
        fingerprint: &EmbeddingFingerprint,
    ) -> Result<()> {
        let space = EmbeddingSpace {
            name: name.to_owned(),
            fingerprint: fingerprint.clone(),
            status: SpaceStatus::Populating,
        };
        // Idempotent: a crash-resumed reconstruction re-opens the same space.
        self.block_write(move |c| embedding_spaces::begin_populating(c, &space))
            .await
    }

    // READ
    async fn next_backfill_window(
        &self,
        space: &str,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        let space = space.to_owned();
        self.block_read(move |c| fact_vectors::next_backfill_window(c, &space, after_id, limit))
            .await
    }

    // WRITE
    async fn write_backfill_batch(&self, space: &str, rows: Vec<(i64, Vec<f32>)>) -> Result<usize> {
        let space = space.to_owned();
        self.block_write(move |c| fact_vectors::write_backfill_batch(c, &space, &rows))
            .await
    }

    // READ
    async fn count_unbackfilled(&self, space: &str) -> Result<usize> {
        let space = space.to_owned();
        self.block_read(move |c| fact_vectors::count_unbackfilled(c, &space))
            .await
    }

    // WRITE (one transaction — the atomic copy-swap)
    async fn promote_space(&self, populating: &str) -> Result<PromoteOutcome> {
        let populating = populating.to_owned();
        self.block_write(move |c| fact_vectors::promote_space(c, &populating))
            .await
    }

    // WRITE
    async fn deprecate_space(&self, name: &str) -> Result<()> {
        let name = name.to_owned();
        self.block_write(move |c| embedding_spaces::deprecate(c, &name))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::SqliteBackend;
    use crate::pool::ConnectionPool;
    use crate::store::schema::CURRENT_SCHEMA_VERSION;
    use crate::store::upcaster::UpcasterRegistry;
    use me_storage::capabilities::{BackendCapabilities, LexicalRanker};
    use me_storage::schema::SchemaManager;
    use me_types::error::MemoryError;
    use me_types::types::EmbeddingFingerprint;

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
