#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::{MemoryEngine, MemoryError};
use tempfile::tempdir;

const DIM: usize = 4;

#[test]
fn open_read_only_on_initialized_db() {
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
    let stats = engine.statistics().unwrap();
    assert_eq!(stats.facts.active, 0);
}

#[test]
fn open_read_only_on_nonexistent_db_fails() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    // File doesn't exist — rejected before SQLite can create empty file
    let result = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build();
    assert!(matches!(result, Err(MemoryError::Migration(_))));
}

#[test]
fn read_only_engine_rejects_writes() {
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
    let err = engine.set_config("test_key", "test_value").unwrap_err();
    assert!(matches!(err, MemoryError::ReadOnly));
}

#[test]
fn engine_not_read_only_by_default() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    assert!(!engine.is_read_only());
}

#[test]
fn read_only_engine_query_works() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Initialize and add a fact
    {
        let engine = MemoryEngine::builder(DIM)
            .path(db_path.clone())
            .build()
            .unwrap();
        engine.set_config("test_key", "test_value").unwrap();
    }

    // Open read-only and verify read access
    let engine = MemoryEngine::builder(DIM)
        .path(db_path)
        .read_only(true)
        .build()
        .unwrap();

    let val = engine.get_config("test_key").unwrap();
    assert_eq!(val, Some("test_value".to_string()));
}

#[test]
fn open_read_only_with_reranker() {
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
