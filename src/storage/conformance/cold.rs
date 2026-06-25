//! `ColdStorage` (.pak manifest) contract bodies. Feature `archive`.
//!
//! Uses `make_with_cold()` — the `(Arc<dyn StorageBackend>, Arc<dyn ColdStorage>)`
//! pair sharing ONE underlying store (a fact inserted via the storage handle is
//! visible to a `commit_archive_atomic` on the cold handle).

use chrono::Utc;

use super::factory::ConformanceBackend;
use super::fixtures::{new_fact, seed_facts};

const HASH: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

/// Manifest entries list oldest-first (`ORDER BY created_at ASC`).
pub async fn manifest_insert_list_oldest_first<F: ConformanceBackend>(f: &F) {
    let (_storage, cold) = f.make_with_cold().await;
    // Distinct created_at so "oldest first" tests the ORDER BY created_at contract,
    // not an insertion-order tie-break SQLite happens to provide but the contract
    // (and Postgres) does not guarantee for equal keys.
    let now = Utc::now();
    let later = now + chrono::Duration::seconds(1);
    let id1 = cold
        .insert_archive_manifest("a.pak", now, 10, 5, 1, 10, now, now, 1024, HASH)
        .await
        .expect("insert 1");
    let id2 = cold
        .insert_archive_manifest("b.pak", later, 10, 5, 11, 20, later, later, 1024, HASH)
        .await
        .expect("insert 2");
    let list = cold.list_archive_manifest().await.expect("list");
    assert_eq!(list.len(), 2, "[{}] two manifest entries", f.name());
    assert_eq!(list[0].id, id1, "[{}] oldest entry first", f.name());
    assert_eq!(list[1].id, id2, "[{}] newest entry last", f.name());
}

/// `delete_archive_manifest` returns `true` for an existing id, `false` otherwise.
pub async fn manifest_delete_existing_and_nonexistent<F: ConformanceBackend>(f: &F) {
    let (_storage, cold) = f.make_with_cold().await;
    let now = Utc::now();
    let id = cold
        .insert_archive_manifest("d.pak", now, 1, 0, 1, 1, now, now, 1, HASH)
        .await
        .expect("insert");
    assert!(
        cold.delete_archive_manifest(id)
            .await
            .expect("delete existing"),
        "[{}] delete of an existing entry must be true",
        f.name()
    );
    assert!(
        !cold
            .delete_archive_manifest(id)
            .await
            .expect("delete again"),
        "[{}] delete of a nonexistent entry must be false",
        f.name()
    );
}

/// Manifest fields round-trip through insert → list.
pub async fn manifest_round_trip_fields<F: ConformanceBackend>(f: &F) {
    let (_storage, cold) = f.make_with_cold().await;
    let now = Utc::now();
    let id = cold
        .insert_archive_manifest("full.pak", now, 42, 7, 100, 141, now, now, 65536, HASH)
        .await
        .expect("insert");
    let list = cold.list_archive_manifest().await.expect("list");
    let entry = list.iter().find(|e| e.id == id).expect("entry present");
    assert_eq!(entry.pak_path, "full.pak", "[{}] pak_path", f.name());
    assert_eq!(entry.fact_count, 42, "[{}] fact_count", f.name());
    assert_eq!(entry.edge_count, 7, "[{}] edge_count", f.name());
    assert_eq!(entry.fact_id_min, 100, "[{}] fact_id_min", f.name());
    assert_eq!(entry.fact_id_max, 141, "[{}] fact_id_max", f.name());
    assert_eq!(entry.size_bytes, 65536, "[{}] size_bytes", f.name());
    assert_eq!(entry.blake3_hash, HASH, "[{}] blake3_hash", f.name());
}

/// `commit_archive_atomic` whose fact hard-delete faults mid-tx ⇒ `Err`, the
/// manifest is byte-identical (the manifest insert that ran earlier in the same
/// transaction rolled back). Generalizes `sqlite/cold_storage.rs:271` via
/// `break_facts_table` instead of a direct `pool.write()` drop.
pub async fn commit_archive_atomic_rollback<F: ConformanceBackend>(f: &F) {
    let (storage, cold) = f.make_with_cold().await;
    let now = Utc::now();
    let fact_ids = seed_facts(&storage, &[new_fact("f1"), new_fact("f2")]).await;
    let before_id = cold
        .insert_archive_manifest("before.pak", now, 1, 0, 1, 1, now, now, 1, HASH)
        .await
        .expect("seed manifest entry");
    assert_eq!(
        cold.list_archive_manifest().await.expect("before").len(),
        1,
        "[{}] one manifest entry before",
        f.name()
    );
    // Crash-inject: drop `facts` so the hard-delete inside commit_archive_atomic faults
    // AFTER the manifest insert ran earlier in the same transaction.
    f.break_facts_table(&storage)
        .await
        .expect("break facts table");
    let err = cold
        .commit_archive_atomic(
            "crash.pak",
            2,
            0,
            fact_ids[0],
            fact_ids[1],
            now,
            now,
            4096,
            HASH,
            &fact_ids,
        )
        .await
        .expect_err("commit must fault on the dropped facts hard-delete");
    // Manifest byte-identical: only the pre-seeded entry, NOT "crash.pak".
    let after = cold.list_archive_manifest().await.expect("after");
    assert_eq!(
        after.len(),
        1,
        "[{}] rollback must leave the manifest byte-identical, got {err:?}",
        f.name()
    );
    assert_eq!(
        after[0].id,
        before_id,
        "[{}] the surviving entry must be the pre-seeded one",
        f.name()
    );
}
