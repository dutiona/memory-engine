//! Cold-storage manifest: the index of `.pak` archive files. Feature-gated
//! (`archive`).
//!
//! A **separate** trait, held by the engine as `Option<Arc<dyn ColdStorage>>` —
//! NOT a [`StorageBackend`](crate::storage::StorageBackend) supertrait bound, so
//! the umbrella's type stays stable across feature sets. Only the manifest CRUD is
//! on the trait; the `.pak` file mechanics (`write_pak_and_hash` / `read_pak` /
//! `hash_file` / `verify_pak`) stay as feature-gated free functions in
//! `archive/pak.rs` — filesystem/codec plumbing, not a port concern.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::archive::ArchiveManifestEntry;
use crate::error::Result;

/// Cold-storage `.pak` manifest CRUD.
///
/// # Errors
/// Every method returns [`MemoryError::Storage`](crate::error::MemoryError::Storage)
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
}
