//! Construction-equivalence golden harness (issue #541).
//!
//! This is the behavior-preservation proof for replacing the telescoping
//! `MemoryEngine::open*` constructors with [`MemoryEngineBuilder`]. It snapshots
//! the *observable configuration* of an engine built by each of the five legacy
//! constructors, freezes those snapshots, and — once the call sites are migrated
//! — re-points the same assertions at the builder. If the builder produces a
//! different observable configuration, the frozen `insta` snapshot fails.
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
use crate::search::hybrid::SearchResult;
use crate::search::strategy::SearchConfig;
use crate::traits::Reranker;

const DIM: usize = 384;

/// A trivial, named reranker test double — only `name()` is observed here.
struct NamedReranker;
impl Reranker for NamedReranker {
    fn rerank(&self, _query: &str, _candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
        Ok(Vec::new())
    }
    fn name(&self) -> &str {
        "named-test-reranker"
    }
}

/// Render the engine's observable construction state into a stable, snapshot-able
/// string. Every field here is one the legacy constructors set and the builder
/// must reproduce verbatim.
fn observe(engine: &MemoryEngine) -> String {
    format!(
        "embed_dim={}\nfile_backed={}\nread_only={}\nread_pool_size={}\nsearch_config={:?}\nupcaster_count={}\nreranker={:?}",
        engine.embed_dim(),
        engine.pool.is_file_backed(),
        engine.pool.is_read_only(),
        engine.pool.read_pool_size(),
        engine.search_config,
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

#[test]
fn equiv_embed_dim_mismatch_is_migration_error() {
    use crate::error::MemoryError;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dim.db");
    let _ = MemoryEngine::builder(768)
        .path(path.clone())
        .build()
        .unwrap();
    let err = MemoryEngine::builder(384).path(path).build().unwrap_err();
    assert!(
        matches!(err, MemoryError::Migration(_)),
        "embed_dim mismatch must yield Migration, got {err:?}"
    );
}
