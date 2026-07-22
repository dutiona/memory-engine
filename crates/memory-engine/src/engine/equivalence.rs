//! Construction-equivalence golden harness (issue #541).
//!
//! These `insta` snapshots were **originally frozen against the five legacy
//! `MemoryEngine::open*` constructors** (see the PR history / the plan in
//! `docs/plans/2026-06-15-engine-builder.md`), then re-pointed at
//! [`MemoryEngineBuilder`] — the snapshots did not change, which is what proved
//! the builder reproduces each constructor's observable configuration. Now that
//! the legacy constructors are deleted, the committed harness exercises only the
//! builder, so it serves as a **regression lock** on that frozen behavior: any
//! future change to the builder's observable construction state fails a snapshot.
//!
//! The harness lives in-crate (not under `tests/`) deliberately: it reads
//! private engine/pool state (`search_config`, `upcaster_registry`,
//! `read_pool_size`) that we do NOT want to expose as public inspection API.
//! Child modules can access an ancestor module's private items, so
//! `engine::equivalence` sees `MemoryEngine`'s private fields directly.
//!
//! `backup_dir` is intentionally absent from the observed tuple: it is consumed
//! during `ConnectionPool::open` (pre-migration `VACUUM INTO`) and not retained
//! on the pool, so it is unobservable on the opened engine. Its preservation is
//! covered behaviorally by the builder unit tests (a backup file is produced).

use super::MemoryEngine;
use crate::error::Result;
use crate::search::strategy::SearchConfig;
use crate::traits::Reranker;
use crate::types::search::SearchResult;

const DIM: usize = 384;

/// A trivial, named reranker test double — only `name()` is observed here.
struct NamedReranker;
impl Reranker for NamedReranker {
    fn rerank(&self, _query: &str, _candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        Ok(Vec::new())
    }
    fn name(&self) -> &'static str {
        "named-test-reranker"
    }
}

/// Render the engine's observable construction state into a stable, snapshot-able
/// string. Every field here is one the legacy constructors set and the builder
/// must reproduce verbatim.
fn observe(engine: &MemoryEngine) -> String {
    // TODO(#631-test): engine internals removed by the cutover. `engine.pool` is
    // gone (the connection pool now lives *inside* the `Arc<dyn StorageBackend>`),
    // so `pool.read_pool_size()` has no accessor, and the `search_config` field was
    // dropped from the struct. Both columns are therefore omitted from the observed
    // tuple below; `pool.is_file_backed()` / `pool.is_read_only()` are replaced by
    // the public `is_file_backed()` / `is_read_only()` methods. The committed insta
    // snapshots must be regenerated to match the reduced tuple.
    format!(
        "embed_dim={}\nfile_backed={}\nread_only={}\nupcaster_count={}\nreranker={:?}",
        engine.embed_dim(),
        engine.is_file_backed(),
        engine.is_read_only(),
        engine.upcaster_registry.registered_count(),
        engine.reranker_name(),
    )
}

// ---------------------------------------------------------------------------
// In-memory backings
// ---------------------------------------------------------------------------

#[test]
fn equiv_in_memory_minimal() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    insta::assert_snapshot!("in_memory_minimal", observe(&engine));
}

#[test]
fn equiv_in_memory_with_search_config() {
    let sc = SearchConfig { ann_threshold: 123 };
    let engine = MemoryEngine::builder(DIM)
        .search_config(sc)
        .build()
        .unwrap();
    insta::assert_snapshot!("in_memory_with_search_config", observe(&engine));
}

#[test]
fn equiv_in_memory_with_reranker() {
    let engine = MemoryEngine::builder(DIM)
        .reranker(Box::new(NamedReranker))
        .build()
        .unwrap();
    // Pins the "reranker reaches init_from_pool, not dropped" wiring.
    assert_eq!(engine.reranker_name(), Some("named-test-reranker"));
    insta::assert_snapshot!("in_memory_with_reranker", observe(&engine));
}

#[test]
fn equiv_in_memory_all_caps() {
    let sc = SearchConfig { ann_threshold: 7 };
    let engine = MemoryEngine::builder(DIM)
        .search_config(sc)
        .reranker(Box::new(NamedReranker))
        .build()
        .unwrap();
    insta::assert_snapshot!("in_memory_all_caps", observe(&engine));
}

#[test]
fn equiv_in_memory_with_upcaster_registry() {
    // Exercises the `upcaster_count` column (otherwise always 0): a populated
    // registry must survive the builder's in-memory path (the #543 bug class).
    let mut registry = crate::store::upcaster::UpcasterRegistry::new();
    registry.register("Interaction", 1, |mut v| {
        v["upcasted"] = serde_json::json!(true);
        Ok(v)
    });
    let engine = MemoryEngine::builder(DIM)
        .upcaster_registry(registry)
        .build()
        .unwrap();
    assert_eq!(engine.upcaster_registry.registered_count(), 1);
    insta::assert_snapshot!("in_memory_with_upcaster", observe(&engine));
}

// ---------------------------------------------------------------------------
// File backings
// ---------------------------------------------------------------------------

#[test]
fn equiv_file_minimal() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MemoryEngine::builder(DIM)
        .path(dir.path().join("equiv.db"))
        .build()
        .unwrap();
    insta::assert_snapshot!("file_minimal", observe(&engine));
}

#[test]
fn equiv_file_with_reranker() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MemoryEngine::builder(DIM)
        .path(dir.path().join("equiv.db"))
        .reranker(Box::new(NamedReranker))
        .build()
        .unwrap();
    // File + reranker: the .reranker() → open_from_config(cfg, reranker) seam.
    assert_eq!(engine.reranker_name(), Some("named-test-reranker"));
    insta::assert_snapshot!("file_with_reranker", observe(&engine));
}

#[test]
fn equiv_file_read_only_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("equiv_ro.db");
    // Create + initialize writable first.
    {
        let _ = MemoryEngine::builder(DIM)
            .path(path.clone())
            .read_pool_size(2)
            .build()
            .unwrap();
    }
    // Reopen read-only.
    let engine = MemoryEngine::builder(DIM)
        .path(path)
        .read_only(true)
        .read_pool_size(2)
        .build()
        .unwrap();
    insta::assert_snapshot!("file_read_only", observe(&engine));
}

// ---------------------------------------------------------------------------
// Behavioral pins (error variants — not config tuples)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn equiv_embed_dim_mismatch_is_migration_error() {
    use crate::error::MemoryError;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dim.db");
    {
        let mut engine = MemoryEngine::builder(768)
            .path(path.clone())
            .build()
            .unwrap();
        // The embedding identity (incl. dim) is recorded on the first embedding
        // write (#613), not at open. Seed it through the storage port to pin
        // dim=768 without standing up an embedder in this builder-equivalence module.
        engine
            .storage()
            .store_embedding_fingerprint(&crate::types::EmbeddingFingerprint::new(
                "mock", "test", 768,
            ))
            .await
            .unwrap();
        // Flush + release the file so the reopen below sees the persisted identity.
        engine.close().await.unwrap();
    }
    let err = MemoryEngine::builder(384).path(path).build().unwrap_err();
    assert!(
        matches!(err, MemoryError::Migration(_)),
        "embed_dim mismatch must yield Migration, got {err:?}"
    );
}
