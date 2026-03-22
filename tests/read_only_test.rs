use memory_engine::{EngineConfig, MemoryEngine, MemoryError};
use tempfile::tempdir;

const DIM: usize = 4;

#[test]
fn open_read_only_on_initialized_db() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // First, create and initialize the DB normally
    {
        let config = EngineConfig::new(db_path.clone(), DIM);
        let _engine = MemoryEngine::open(&config).unwrap();
    }

    // Now open read-only
    let mut config = EngineConfig::new(db_path, DIM);
    config.read_only = true;
    let engine = MemoryEngine::open(&config).unwrap();
    assert!(engine.is_read_only());

    // Read operations should work
    let stats = engine.statistics().unwrap();
    assert_eq!(stats.facts.active, 0);
}

#[test]
fn open_read_only_on_nonexistent_db_fails() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    let mut config = EngineConfig::new(db_path, DIM);
    config.read_only = true;
    // File doesn't exist — rejected before SQLite can create empty file
    let result = MemoryEngine::open(&config);
    assert!(matches!(result, Err(MemoryError::Migration(_))));
}

#[test]
fn read_only_engine_rejects_writes() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Initialize
    {
        let config = EngineConfig::new(db_path.clone(), DIM);
        let _engine = MemoryEngine::open(&config).unwrap();
    }

    // Open read-only
    let mut config = EngineConfig::new(db_path, DIM);
    config.read_only = true;
    let engine = MemoryEngine::open(&config).unwrap();

    // set_config is the simplest write method — no mock providers needed
    let err = engine.set_config("test_key", "test_value").unwrap_err();
    assert!(matches!(err, MemoryError::ReadOnly));
}

#[test]
fn engine_not_read_only_by_default() {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    assert!(!engine.is_read_only());
}

#[test]
fn read_only_engine_query_works() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Initialize and add a fact
    {
        let config = EngineConfig::new(db_path.clone(), DIM);
        let engine = MemoryEngine::open(&config).unwrap();
        engine.set_config("test_key", "test_value").unwrap();
    }

    // Open read-only and verify read access
    let mut config = EngineConfig::new(db_path, DIM);
    config.read_only = true;
    let engine = MemoryEngine::open(&config).unwrap();

    let val = engine.get_config("test_key").unwrap();
    assert_eq!(val, Some("test_value".to_string()));
}

#[test]
fn open_read_only_with_reranker() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    {
        let config = EngineConfig::new(db_path.clone(), DIM);
        let _engine = MemoryEngine::open(&config).unwrap();
    }

    let mut config = EngineConfig::new(db_path, DIM);
    config.read_only = true;
    let engine = MemoryEngine::open_with_reranker(&config, None).unwrap();
    assert!(engine.is_read_only());
}
