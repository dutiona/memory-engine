//! `impl ColdStorage for SqliteBackend` — delegates to [`ArchiveManifestStore`],
//! the concrete SQL owner of the `archive_manifest` table.
//!
//! Feature-gated: `#[cfg(all(feature = "async", feature = "archive"))]`.
//! Only the manifest CRUD is on this trait; `.pak` file I/O stays as free
//! functions in `archive/pak.rs` — filesystem/codec plumbing, not a port concern.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::SqliteBackend;
use crate::archive::ArchiveManifestEntry;
use crate::error::Result;
use crate::storage::cold_storage::ColdStorage;
use crate::store::archive_manifest::ArchiveManifestStore;

#[async_trait]
impl ColdStorage for SqliteBackend {
    // WRITE
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<i64> {
        let pak_path = pak_path.to_owned();
        let blake3_hash = blake3_hash.to_owned();
        self.block_write(move |c| {
            ArchiveManifestStore::new(c).insert(
                &pak_path,
                created_at,
                fact_count,
                edge_count,
                fact_id_min,
                fact_id_max,
                t_created_min,
                t_created_max,
                size_bytes,
                &blake3_hash,
            )
        })
        .await
    }

    // READ
    async fn list_archive_manifest(&self) -> Result<Vec<ArchiveManifestEntry>> {
        self.block_read(|c| ArchiveManifestStore::new(c).list())
            .await
    }

    // WRITE
    async fn delete_archive_manifest(&self, id: i64) -> Result<bool> {
        self.block_write(move |c| ArchiveManifestStore::new(c).delete(id))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::pool::ConnectionPool;
    use crate::storage::cold_storage::ColdStorage;
    use crate::store::upcaster::UpcasterRegistry;

    const DIM: usize = 4;

    fn backend() -> SqliteBackend {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    async fn insert_entry(be: &SqliteBackend, pak_path: &str) -> i64 {
        let now = Utc::now();
        be.insert_archive_manifest(
            pak_path,
            now,
            10,
            5,
            1,
            10,
            now,
            now,
            1024,
            "deadbeefdeadbeefdeadbeefdeadbeef",
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn manifest_insert_list_oldest_first() {
        let be = backend();
        assert!(be.list_archive_manifest().await.unwrap().is_empty());

        let id1 = insert_entry(&be, "archives/2026-01.pak").await;
        let id2 = insert_entry(&be, "archives/2026-02.pak").await;

        let list = be.list_archive_manifest().await.unwrap();
        assert_eq!(list.len(), 2);
        // list returns oldest-first (ORDER BY created_at ASC).
        assert_eq!(list[0].id, id1);
        assert_eq!(list[1].id, id2);
        assert_eq!(list[0].pak_path, "archives/2026-01.pak");
        assert_eq!(list[1].pak_path, "archives/2026-02.pak");
    }

    #[tokio::test]
    async fn manifest_delete_existing_returns_true() {
        let be = backend();
        let id = insert_entry(&be, "archives/del.pak").await;
        assert_eq!(be.list_archive_manifest().await.unwrap().len(), 1);

        let deleted = be.delete_archive_manifest(id).await.unwrap();
        assert!(deleted);
        assert!(be.list_archive_manifest().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn manifest_delete_nonexistent_returns_false() {
        let be = backend();
        let deleted = be.delete_archive_manifest(9999).await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn manifest_round_trip_fields() {
        let be = backend();
        let now = Utc::now();
        let id = be
            .insert_archive_manifest(
                "archives/full.pak",
                now,
                42,
                7,
                100,
                141,
                now,
                now,
                65536,
                "cafebabecafebabecafebabecafebabe",
            )
            .await
            .unwrap();

        let list = be.list_archive_manifest().await.unwrap();
        let entry = list.iter().find(|e| e.id == id).unwrap();
        assert_eq!(entry.pak_path, "archives/full.pak");
        assert_eq!(entry.fact_count, 42);
        assert_eq!(entry.edge_count, 7);
        assert_eq!(entry.fact_id_min, 100);
        assert_eq!(entry.fact_id_max, 141);
        assert_eq!(entry.size_bytes, 65536);
        assert_eq!(entry.blake3_hash, "cafebabecafebabecafebabecafebabe");
    }
}
