#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::{EmbeddingFingerprint, EmbeddingProvider, MemoryEngine, MemoryError};
use tempfile::tempdir;

const DIM: usize = 4;

/// Minimal embedder for the read-only empty-batch test. It is never actually invoked
/// (the read-only gate fires before any embedding), so the bodies are trivial.
struct NoopEmbedder;
impl EmbeddingProvider for NoopEmbedder {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.0; DIM])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("noop", "test", DIM)
    }
}

#[tokio::test]
async fn open_read_only_on_initialized_db() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // First, create and initialize the DB normally
    {
        let _engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
    }

    // Now open read-only
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();
    assert!(engine.is_read_only());

    // Read operations should work
    let stats = engine.statistics().await.unwrap();
    assert_eq!(stats.facts.active, 0);
}

#[tokio::test]
async fn open_read_only_on_nonexistent_db_fails() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    // File doesn't exist — rejected before SQLite can create empty file
    let result = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build();
    assert!(matches!(result, Err(MemoryError::Migration(_))));
}

#[tokio::test]
async fn read_only_engine_rejects_writes() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Initialize
    {
        let _engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
    }

    // Open read-only
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();

    // set_config is the simplest write method — no mock providers needed
    let err = engine
        .set_config("test_key", "test_value")
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::ReadOnly));
}

/// #972: the facade's own write methods (which delegate straight to `self.storage`,
/// bypassing the L3 primitives) fail fast with `ReadOnly` via `self.ensure_writable()` —
/// before any read/work — rather than late at the below-seam `try_write()`. Locks the
/// gate on the facade surface a cross-model (agy) review flagged as ungated.
#[tokio::test]
async fn read_only_engine_rejects_facade_writes() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _e = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
    }
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();

    // Each of these delegates to self.storage and now self-gates on read_only.
    assert!(matches!(
        engine.pin_fact(1).await.unwrap_err(),
        MemoryError::ReadOnly
    ));
    assert!(matches!(
        engine.unpin_fact(1).await.unwrap_err(),
        MemoryError::ReadOnly
    ));
    assert!(matches!(
        engine.expire_edge(1).await.unwrap_err(),
        MemoryError::ReadOnly
    ));
    assert!(matches!(
        engine.delete_lineage(1).await.unwrap_err(),
        MemoryError::ReadOnly
    ));
    assert!(matches!(
        engine.ensure_scope_path("a/b").await.unwrap_err(),
        MemoryError::ReadOnly
    ));
    // record_outcome does a get_fact READ before writing; the gate must precede it —
    // ReadOnly, not the NotFound the (skipped) fact lookup would otherwise raise.
    assert!(matches!(
        engine
            .record_outcome(999, memory_engine::Outcome::Positive)
            .await
            .unwrap_err(),
        MemoryError::ReadOnly
    ));
    // link_session_facts does a scope/active-facts READ before its edge writes.
    assert!(matches!(
        engine.link_session_facts("s1", None).await.unwrap_err(),
        MemoryError::ReadOnly
    ));

    // flush_snapshot is the deliberate exception: a read-only engine has nothing to
    // persist, so close() is a clean no-op (Ok(false)), NOT a ReadOnly error.
    assert!(!engine.flush_snapshot().await.unwrap());
}

/// #972 crux lock: an EMPTY `add_facts_batch` on a read-only engine returns `ReadOnly`
/// (it was `Ok(vec![])` pre-#972). The gate sits at the primitive entry, BEFORE the
/// empty-input short-circuit — so even a no-op write call on a read-only handle is
/// rejected. Pins the deliberate `Ok -> Err` behavior a cross-model review (Codex) asked
/// to lock rather than leave commentary-only.
#[tokio::test]
async fn read_only_engine_rejects_empty_batch() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _e = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
    }
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();
    let embedder: std::sync::Arc<dyn EmbeddingProvider> = std::sync::Arc::new(NoopEmbedder);
    let err = engine
        .add_facts_batch(&[], embedder.clone(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MemoryError::ReadOnly),
        "empty batch on a read-only engine must be ReadOnly, got {err:?}"
    );
    // The separately-implemented partial variant has the same entry gate: its empty
    // return also sits below the gate, so the OUTER Result is Err(ReadOnly).
    let err = engine
        .add_facts_batch_partial(&[], embedder, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MemoryError::ReadOnly),
        "empty partial batch on a read-only engine must be ReadOnly, got {err:?}"
    );
}

/// #972: `reconstruct` and the `bootstrap_*` family are public write APIs that do
/// substantial work (fingerprint validation; file parsing + consumer extraction) before
/// the write. They now fail fast at entry with `ReadOnly`. In particular this locks the
/// `bootstrap_directory` fix: it used to catch the per-session `ReadOnly`, log it, and
/// return `Ok(aggregate)` — silently swallowing the error it documents.
#[tokio::test]
async fn read_only_engine_rejects_reconstruct_and_bootstrap() {
    use std::sync::Arc;
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _e = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
    }
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedder);

    // reconstruct: `ReadOnly` takes precedence over the fingerprint dim check
    // (capability-first). The target fingerprint is deliberately MISMATCHED (`DIM + 1`
    // vs the embedder's `DIM`) so the ordering is mutation-proven: without the entry
    // gate, the dim check would fire first and return `EmbeddingDimension`, failing this
    // assertion. The test stays green ONLY because `ensure_writable()` runs first.
    let fp = EmbeddingFingerprint::new("noop", "test", DIM + 1);
    let err = engine.reconstruct(&fp, &embedder).await.unwrap_err();
    assert!(
        matches!(err, MemoryError::ReadOnly),
        "reconstruct on a read-only engine must be ReadOnly, got {err:?}"
    );

    // bootstrap_directory: fails fast at entry rather than swallowing per-session ReadOnly
    // and returning Ok. Empty session dir → the ReadOnly must still surface.
    let sessions = tempdir().unwrap();
    let err = engine
        .bootstrap_directory(
            sessions.path(),
            embedder,
            Arc::new(memory_engine::KeywordExtractor),
            &memory_engine::BootstrapConfig::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, MemoryError::ReadOnly),
        "bootstrap_directory on a read-only engine must be ReadOnly (not a swallowed Ok), got {err:?}"
    );
}

#[tokio::test]
async fn engine_not_read_only_by_default() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert!(!engine.is_read_only());
}

#[tokio::test]
async fn read_only_engine_query_works() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Initialize and add a fact
    {
        let engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
        engine.set_config("test_key", "test_value").await.unwrap();
    }

    // Open read-only and verify read access
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();

    let val = engine.get_config("test_key").await.unwrap();
    assert_eq!(val, Some("test_value".to_string()));
}

#[tokio::test]
async fn open_read_only_with_reranker() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    {
        let _engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
    }

    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();
    assert!(engine.is_read_only());
}
