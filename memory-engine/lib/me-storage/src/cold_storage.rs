//! Cold-storage manifest: the index of `.pak` archive files. Feature-gated
//! (`archive`).
//!
//! A **separate** trait, held by the engine as `Option<Arc<dyn ColdStorage>>` —
//! NOT a [`StorageBackend`](crate::StorageBackend) supertrait bound, so
//! the umbrella's type stays stable across feature sets. Only the manifest CRUD is
//! on the trait; the `.pak` file mechanics (`write_pak_and_hash` / `read_pak` /
//! `hash_file` / `verify_pak`) stay as feature-gated free functions in
//! `archive/pak.rs` — filesystem/codec plumbing, not a port concern.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use me_types::error::Result;
use me_types::types::archive::ArchiveManifestEntry;

/// Cold-storage `.pak` manifest CRUD.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage)
/// on a backend failure.
#[async_trait]
pub trait ColdStorage: Send + Sync {
    /// Insert a manifest entry for a newly created `.pak` file; returns its row id.
    #[allow(clippy::too_many_arguments)] // mirrors the verbatim manifest row shape
    async fn insert_archive_manifest(
        &self,
        pak_path: &str,
        created_at: DateTime<Utc>,
        fact_count: i64,
        edge_count: i64,
        fact_id_min: i64,
        fact_id_max: i64,
        t_created_min: DateTime<Utc>,
        t_created_max: DateTime<Utc>,
        size_bytes: i64,
        blake3_hash: &str,
    ) -> Result<i64>;
    /// List all manifest entries, oldest first.
    async fn list_archive_manifest(&self) -> Result<Vec<ArchiveManifestEntry>>;
    /// Delete a manifest entry by id; returns `true` if it existed.
    async fn delete_archive_manifest(&self, id: i64) -> Result<bool>;

    // -------------------------------------------------------------------------
    // Stage A atomic port method (Fork B, §3 of the #631 plan)
    // -------------------------------------------------------------------------

    /// Single write transaction: manifest insert + hard-delete edges (by fact ids)
    /// + hard-delete facts — atomic, crash-safe.
    ///
    /// This is the verbatim body of `engine/archive.rs:238–279` moved below the
    /// seam. The `.pak` file I/O stays engine-side; this method only commits the
    /// database side of the archive operation.
    ///
    /// # Contract
    ///
    /// `Ok ⟹ all sub-ops committed; Err ⟹ store byte-identical (tx rolled back)`.
    ///
    /// `created_at` is captured as `Utc::now()` inside the transaction, matching the
    /// original `commit_archive` which stamps the manifest row at commit time (not
    /// call-site time). This preserves ordering correctness for
    /// `list_archive_manifest` (`ORDER BY created_at ASC`).
    ///
    /// If this method returns `Err`, the caller is responsible for removing the
    /// already-written `.pak` file (the CWE-459 orphan guard, preserved from the
    /// original `commit_archive`).
    #[allow(clippy::too_many_arguments)]
    async fn commit_archive_atomic(
        &self,
        pak_filename: &str,
        fact_count: i64,
        edge_count: i64,
        fact_id_min: i64,
        fact_id_max: i64,
        t_created_min: DateTime<Utc>,
        t_created_max: DateTime<Utc>,
        pak_size_bytes: i64,
        blake3_hash: &str,
        fact_ids: &[i64],
    ) -> Result<()>;
}
