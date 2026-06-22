//! Backend lifecycle: migrations, schema version, read-only validation, the
//! capability probe, and the embedding-fingerprint identity surface.
//!
//! *Constructing* a backend (open flags, pool URL, init DDL, the `VACUUM INTO`
//! backup, generic config K/V) is backend-specific and stays **off** this shared
//! port. The embedding-fingerprint methods *are* on the port: they are live engine
//! behavior (open/write/promotion read & write the fingerprint), so omitting them
//! would force the engine to reach through to `SQLite`.

use async_trait::async_trait;

use crate::error::Result;
use crate::storage::capabilities::BackendCapabilities;
use crate::types::EmbeddingFingerprint;

/// Backend lifecycle the engine drives post-open.
///
/// Mixes one **synchronous** method (`capabilities` — fixed at open, not a
/// per-call round-trip) with the async lifecycle ones; legal under
/// `#[async_trait]`.
///
/// # Errors
/// Async methods return [`MemoryError::Storage`](crate::error::MemoryError::Storage)
/// on a backend failure, or [`MemoryError::Migration`](crate::error::MemoryError::Migration)
/// for a migration/compatibility failure. The fingerprint methods add two:
/// [`record_embedding_fingerprint_if_absent`](Self::record_embedding_fingerprint_if_absent)
/// returns [`MemoryError::EmbeddingDimension`](crate::error::MemoryError::EmbeddingDimension)
/// when `candidate.dim` disagrees with the recorded (or expected) identity, and
/// [`require_embedding_fingerprint_present`](Self::require_embedding_fingerprint_present)
/// returns [`MemoryError::Internal`](crate::error::MemoryError::Internal) when no
/// fingerprint has been recorded yet (the open-time identity guard).
#[async_trait]
pub trait SchemaManager: Send + Sync {
    /// Run all pending migrations to the current schema version. Idempotent at HEAD.
    async fn migrate(&self) -> Result<()>;
    /// The schema version currently recorded in the store.
    async fn schema_version(&self) -> Result<u32>;
    /// Read-only compatibility check (the read-only open path): validate epoch +
    /// version + config-table presence **without** writing. Errs if the store needs
    /// migration but cannot be written.
    async fn validate_schema_version(&self) -> Result<()>;
    /// Probe backend capabilities. **Synchronous** — capabilities are fixed at open.
    fn capabilities(&self) -> BackendCapabilities;

    // --- embedding-fingerprint identity (transcribes `store::embedding_meta`) ---
    /// Load the persisted embedding fingerprint, if any.
    async fn load_embedding_fingerprint(&self) -> Result<Option<EmbeddingFingerprint>>;
    /// Persist (overwrite) the embedding fingerprint.
    async fn store_embedding_fingerprint(&self, fp: &EmbeddingFingerprint) -> Result<()>;
    /// Record `candidate` if none is stored yet; otherwise return the stored one.
    /// Validates `candidate.dim == expected_dim`.
    async fn record_embedding_fingerprint_if_absent(
        &self,
        candidate: &EmbeddingFingerprint,
        expected_dim: usize,
    ) -> Result<EmbeddingFingerprint>;
    /// Require a fingerprint to be present (the open-time identity guard).
    async fn require_embedding_fingerprint_present(&self) -> Result<()>;

    // -------------------------------------------------------------------------
    // Stage A config accessors (cutover needs them at engine/mod.rs:649,659 +
    // cycle watermarks — currently backend-private via `store::schema::{get,set}_config`).
    // -------------------------------------------------------------------------

    /// Read a config value by key. Returns `None` if the key is absent.
    ///
    /// Delegates to `store::schema::get_config`.
    async fn get_config(&self, key: &str) -> Result<Option<String>>;

    /// Write a config value (upsert).
    ///
    /// Delegates to `store::schema::set_config`. Returns `MemoryError::ReadOnly`
    /// if the backend was opened in read-only mode.
    async fn set_config(&self, key: &str, value: &str) -> Result<()>;

    // -------------------------------------------------------------------------
    // Stage E (cutover) snapshot seam — engine-projection persistence.
    //
    // The engine's in-memory `graph` + `scope_tree` projections are cached to a
    // backend-managed sidecar so re-open can skip the full DB rebuild. The
    // engine owns those projections but cannot reach the backend-private
    // fingerprint + index state (e.g. `SQLite`'s HNSW), so snapshot *assembly*
    // lives below the seam: the engine hands its two projections down and the
    // backend folds in its own fingerprint + index. HNSW never crosses the port.
    // (Snapshot *load* runs concretely during `open_storage` — before the
    // backend is shared as `Arc<dyn StorageBackend>` — because restoring a
    // backend-private index needs `&mut self`.)
    // -------------------------------------------------------------------------

    /// Persist the engine's in-memory projections to a backend-managed sidecar,
    /// keyed by the backend's current state fingerprint.
    ///
    /// Returns `Ok(true)` when a snapshot was written; `Ok(false)` when the
    /// backend has no durable snapshot location (in-memory or read-only). A
    /// backend without a sidecar mechanism (e.g. a future `PgBackend`) returns
    /// `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a
    /// fingerprint read or sidecar write failure.
    ///
    /// Takes the snapshots **by value**: the engine builds them fresh at
    /// `close()` and discards them, so ownership moves straight through the
    /// backend's blocking boundary (no deep-clone of a large graph).
    async fn write_engine_snapshot(
        &self,
        graph: crate::engine::snapshot::GraphSnapshot,
        scope_tree: crate::engine::snapshot::ScopeTreeSnapshot,
    ) -> Result<bool>;

    // -------------------------------------------------------------------------
    // Stage E (cutover) inspection ports — relocate the raw-`&Connection` free
    // functions in `crate::inspect` below the seam. The engine lost its pool, so
    // these methods supply their own db-path / fingerprint from backend-private
    // state. HNSW + driver types never cross the port.
    // -------------------------------------------------------------------------

    /// Compute aggregate engine statistics (fact/edge/summary/scope/event counts
    /// + storage metrics). Replaces `inspect::statistics::compute_statistics`; the
    /// backend supplies its own db path for [`StorageStats::file_path`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a
    /// backend failure.
    async fn statistics(&self) -> Result<crate::inspect::EngineStatistics>;

    /// Export full engine state to a file in the requested [`DumpFormat`].
    ///
    /// Relocates `inspect::dump::{dump_json,_gzip,_zstd,dump_sqlite}` below the
    /// seam: the JSON variants stream via a read connection; the `Sqlite`
    /// (`VACUUM INTO`) variant routes through the write connection so a read-only
    /// backend rejects it with [`MemoryError::ReadOnly`]. Feature-gated compression
    /// dispatch lives in the impl, keeping this trait surface `#[cfg]`-free.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`](crate::error::MemoryError::Io) on filesystem
    /// failure, [`MemoryError::Conflict`](crate::error::MemoryError::Conflict) if a
    /// dump target resolves to the live database or a directory,
    /// [`MemoryError::NotImplemented`](crate::error::MemoryError::NotImplemented) for
    /// a compression format whose feature is disabled, [`MemoryError::ReadOnly`] for
    /// the `Sqlite` format on a read-only backend, or
    /// [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a backend failure.
    async fn dump_state(&self, embed_dim: usize, format: crate::inspect::DumpFormat) -> Result<()>;

    /// Read-only check that a candidate embedding fingerprint is compatible with
    /// the store's recorded identity (the #614/#615 eager fail-fast). Delegates to
    /// `store::embedding_meta::check_compatible`.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingModelMismatch`](crate::error::MemoryError::EmbeddingModelMismatch)
    /// if an identity is recorded and `candidate` differs from it, or
    /// [`MemoryError::Storage`](crate::error::MemoryError::Storage) on a backend failure.
    async fn check_embedding_compatible(&self, candidate: &EmbeddingFingerprint) -> Result<()>;
}
