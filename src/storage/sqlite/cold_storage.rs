//! `impl ColdStorage for SqliteBackend` — delegates to [`ArchiveManifestStore`],
//! the concrete SQL owner of the `archive_manifest` table.
//!
//! Feature-gated: `#[cfg(feature = "archive")]`.
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

    // -------------------------------------------------------------------------
    // Stage A atomic port method
    // -------------------------------------------------------------------------

    // ATOMIC WRITE — manifest insert + hard-delete edges + hard-delete facts,
    // verbatim body of engine/archive.rs:238-279 moved below the seam.
    //
    // `created_at` is captured as `Utc::now()` inside the transaction, matching
    // the original `commit_archive` (archive.rs:239). This preserves ordering
    // correctness for `list_archive_manifest` (`ORDER BY created_at ASC`).
    #[allow(clippy::cast_possible_wrap, clippy::too_many_arguments)]
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
    ) -> Result<()> {
        use crate::error::ArchiveError;
        use crate::store::edges::EdgeStore;
        use crate::store::facts::FactStore;

        let pak_filename = pak_filename.to_owned();
        let blake3_hash = blake3_hash.to_owned();
        let fact_ids = fact_ids.to_vec();
        let dim = self.embed_dim;
        self.block_write(move |conn| {
            // Verbatim body of engine/archive.rs:239,253-279: capture `now` at the
            // transaction boundary, then manifest insert + FK-safe edge delete + fact
            // hard-delete, all in one transaction.
            let now = Utc::now();
            let tx = conn.unchecked_transaction().map_err(|e| {
                ArchiveError::Transaction(format!("failed to begin transaction: {e}"))
            })?;

            ArchiveManifestStore::new(&tx).insert(
                &pak_filename,
                now,
                fact_count,
                edge_count,
                fact_id_min,
                fact_id_max,
                t_created_min,
                t_created_max,
                pak_size_bytes,
                &blake3_hash,
            )?;

            // Delete edges first (FK safety), then facts.
            EdgeStore::new(&tx).hard_delete_by_facts(&fact_ids)?;
            FactStore::new(&tx, dim).hard_delete_ids(&fact_ids)?;

            tx.commit().map_err(|e| {
                ArchiveError::Transaction(format!("failed to commit archive transaction: {e}"))
            })?;
            Ok(())
        })
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
    use crate::store::facts::FactStore;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::{FactType, NewFact};

    const DIM: usize = 4;

    fn backend_from(pool: Arc<ConnectionPool>) -> SqliteBackend {
        SqliteBackend::from_pool(pool, Arc::new(UpcasterRegistry::new()))
    }

    fn backend() -> SqliteBackend {
        backend_from(Arc::new(ConnectionPool::open_memory(DIM).unwrap()))
    }

    fn new_fact(content: &str) -> NewFact {
        NewFact {
            content: content.into(),
            content_hash: String::new(),
            embedding: vec![0.1_f32; DIM],
            fact_type: FactType::Episodic,
            t_created: Utc::now(),
            t_expired: Some(Utc::now()), // pre-expired so it qualifies as archive candidate
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            is_pinned: false,
        }
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

    // -------------------------------------------------------------------------
    // commit_archive_atomic — crash-injection / rollback test (F5)
    // -------------------------------------------------------------------------

    /// Crash-injection: dropping the `facts` table makes `hard_delete_ids` fail
    /// mid-transaction. The manifest insert and edge delete that ran earlier in the
    /// same tx must be rolled back — manifest + facts tables are byte-identical to
    /// before.
    ///
    /// Proof of atomicity: if the tx did NOT roll back, the `archive_manifest` table
    /// would contain a new entry for "crash.pak". We assert it still has exactly the
    /// one pre-seeded entry, byte-identical to before.
    #[tokio::test]
    #[allow(clippy::significant_drop_tightening)]
    async fn commit_archive_atomic_rollback_on_mid_tx_error() {
        let pool = Arc::new(ConnectionPool::open_memory(DIM).unwrap());

        // Seed two facts so `fact_ids` is non-empty (exercises the delete path).
        let fact_ids: Vec<i64> = {
            let conn = pool.write();
            let store = FactStore::new(&conn, DIM);
            vec![
                store.insert(&new_fact("f1")).unwrap(),
                store.insert(&new_fact("f2")).unwrap(),
            ]
        };

        // Pre-insert one manifest entry to establish a known "before" count.
        let be = backend_from(Arc::clone(&pool));
        let before_id = insert_entry(&be, "archives/before.pak").await;
        let manifest_before = be.list_archive_manifest().await.unwrap();
        assert_eq!(manifest_before.len(), 1);

        // Drop the `facts` table to force `hard_delete_ids` to fail mid-tx.
        // The transaction sequence inside `commit_archive_atomic` is:
        //   1. ArchiveManifestStore::insert  ← succeeds
        //   2. EdgeStore::hard_delete_by_facts ← succeeds (no edges)
        //   3. FactStore::hard_delete_ids    ← FAILS (table gone)
        // The whole tx must roll back.
        {
            let conn = pool.write();
            conn.execute_batch("DROP TABLE facts").unwrap();
        }

        let now = Utc::now();
        let err = be
            .commit_archive_atomic(
                "crash.pak",
                2, // fact_count
                0, // edge_count
                fact_ids[0],
                fact_ids[1],
                now,
                now,
                4096,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &fact_ids,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::MemoryError::Archive(_)
                    | crate::error::MemoryError::Database(_)
                    | crate::error::MemoryError::Storage(_)
            ),
            "expected Archive, Database or Storage error, got {err:?}"
        );

        // Manifest must be byte-identical to before: only the one pre-seeded entry,
        // NOT the "crash.pak" entry from the rolled-back tx.
        let manifest_after = be.list_archive_manifest().await.unwrap();
        assert_eq!(
            manifest_after.len(),
            1,
            "rollback must leave manifest byte-identical; got {manifest_after:?}"
        );
        assert_eq!(
            manifest_after[0].id, before_id,
            "the surviving entry must be the pre-seeded one, not the rolled-back one"
        );
        assert_eq!(manifest_after[0].pak_path, "archives/before.pak");
    }
}
