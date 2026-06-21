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
}
