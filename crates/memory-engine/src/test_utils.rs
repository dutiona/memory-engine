//! Shared test utilities for `memory-engine` unit tests.
//!
//! This module is only compiled in `#[cfg(test)]` mode. It provides common
//! test doubles and factory helpers that would otherwise be duplicated across
//! every `mod tests { … }` block.
//!
//! Relocated to `me-types`/`me-traits` (Wave 2 #816, `me-backend-sqlite` carve):
//! the `NewFact`/`NewEvent` factories build only `me-types` DTOs, and
//! `MockEmbedder` implements the `me-traits` `EmbeddingProvider` trait — this
//! module re-exports them so every existing `crate::test_utils::X` call site
//! keeps resolving unchanged. `setup_memory_db` moved with `store/` to
//! `me-backend-sqlite` (Commit 3) — its only two call sites
//! (`store::activities`, `store::checkpoints`) moved with it, so it is not
//! re-exported here. `new_event`'s only facade call site
//! (`storage::sqlite::event_log`) moved to `me-backend-sqlite` too (Wave 2 #816 /
//! S2, sub-PR 2b's `storage::sqlite` carve), repathed to `me_types::test_util::new_event`
//! directly there, so it is not re-exported here either. `new_fact`/`new_fact_with_type`'s
//! last facade call sites were the `search/hybrid.rs` behavior tests, moved (re-pointed)
//! to `me-query` in the Wave 2 #816 / S4, sub-PR 2 query carve — `me-query` reaches
//! `me_types::test_util` directly as its own dev-dependency, so they are dropped here.

pub use me_traits::test_util::MockEmbedder;
pub use me_types::test_util::new_fact_hashed;

/// Store `content` as a deferred write leaves it (#1021): `embedding = X''` plus its
/// `embed_queue` row, marked failed when `failed`. Returns the fact id. `content` must
/// hold no single quote.
#[cfg(feature = "test-util")]
pub async fn seed_waiting_fact(engine: &crate::MemoryEngine, content: &str, failed: bool) -> i64 {
    let now = chrono::Utc::now().to_rfc3339();
    let hash = &blake3::hash(content.as_bytes()).to_hex()[..32];
    let failed_at = if failed {
        format!("'{now}'")
    } else {
        "NULL".to_owned()
    };
    engine
        .storage()
        .raw_exec(&format!(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed)
                 VALUES ('{content}', '{hash}', X'', 'semantic', '{now}', '{now}');
             INSERT INTO embed_queue (fact_id, enqueued_at, next_attempt_at, failed_at)
                 VALUES (last_insert_rowid(), '{now}', '{now}', {failed_at});"
        ))
        .await
        .expect("seed a waiting fact");
    engine
        .storage()
        .list_all_facts()
        .await
        .expect("list facts")
        .iter()
        .map(|f| f.id)
        .max()
        .expect("the seeded fact")
}
