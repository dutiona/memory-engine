//! Integration tests for import/export round-trips.

use memory_engine::EmbeddingProvider;
use memory_engine::engine::{EngineConfig, MemoryEngine};
use memory_engine::inspect_types::DumpFormat;
use memory_engine::types::{AddFactRequest, FactType};

const DIM: usize = 4;

struct FakeEmbed;
impl EmbeddingProvider for FakeEmbed {
    fn embed(&self, _text: &str) -> memory_engine::error::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
}

/// Create a populated engine for testing.
fn make_engine() -> MemoryEngine {
    let engine = MemoryEngine::open_memory(DIM).unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "beta fact".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();
    engine
}

#[test]
fn json_restore_to_file_engine() {
    let engine = make_engine();
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .unwrap();

    let target_path = dir.path().join("restored.db");
    let config = EngineConfig::new(target_path, DIM);
    let restored = MemoryEngine::restore_json(&json_path, &config).unwrap();

    // Verify data.
    let stats = restored.statistics().unwrap();
    assert_eq!(stats.facts.total, 2);
    assert!(stats.scopes.total >= 1);

    // Verify engine is functional: can add new facts.
    let new_id = restored
        .add_fact(
            &AddFactRequest {
                content: "gamma fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();
    assert!(new_id > 2, "new id {new_id} should be > max imported id 2");
}

#[test]
fn json_restore_to_memory_engine() {
    let engine = make_engine();
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&json_path).unwrap();
    let stats = restored.statistics().unwrap();
    assert_eq!(stats.facts.total, 2);
}

#[test]
fn sqlite_restore_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.db");
    let source_config = EngineConfig::new(source_path.clone(), DIM);
    let engine = MemoryEngine::open(&source_config).unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "sqlite test".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();

    // Dump to SQLite backup.
    let backup_path = dir.path().join("backup.db");
    engine
        .dump_state(&DumpFormat::Sqlite(backup_path.clone()))
        .unwrap();
    drop(engine);

    // Restore from backup.
    let target_path = dir.path().join("restored.db");
    let target_config = EngineConfig::new(target_path, DIM);
    let restored = MemoryEngine::restore_sqlite(&backup_path, &target_config).unwrap();

    let stats = restored.statistics().unwrap();
    assert_eq!(stats.facts.total, 1);

    // Engine is functional.
    let new_id = restored
        .add_fact(
            &AddFactRequest {
                content: "new fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &FakeEmbed,
            None,
        )
        .unwrap();
    assert!(new_id > 1);
}

#[test]
fn restore_json_fails_if_target_exists() {
    let engine = make_engine();
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .unwrap();

    // Create the target file so it exists.
    let target_path = dir.path().join("existing.db");
    std::fs::write(&target_path, "placeholder").unwrap();

    let config = EngineConfig::new(target_path, DIM);
    let err = MemoryEngine::restore_json(&json_path, &config).unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[test]
fn restore_json_fails_on_embed_dim_mismatch() {
    let engine = make_engine();
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .unwrap();

    let target_path = dir.path().join("restored.db");
    // Config says dim=8 but snapshot has dim=4.
    let config = EngineConfig::new(target_path, 8);
    let err = MemoryEngine::restore_json(&json_path, &config).unwrap_err();
    assert!(err.to_string().contains("embedding dimension"));
}

#[test]
fn restore_sqlite_fails_if_backup_missing() {
    let dir = tempfile::tempdir().unwrap();
    let backup_path = dir.path().join("nonexistent.db");
    let target_path = dir.path().join("target.db");
    let config = EngineConfig::new(target_path, DIM);
    let err = MemoryEngine::restore_sqlite(&backup_path, &config).unwrap_err();
    assert!(err.to_string().contains("backup file"));
}

#[cfg(feature = "compress-gzip")]
#[test]
fn gzip_json_restore_roundtrip() {
    let engine = make_engine();
    let dir = tempfile::tempdir().unwrap();
    let gz_path = dir.path().join("dump.json.gz");
    engine
        .dump_state(&DumpFormat::JsonGzip(gz_path.clone()))
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&gz_path).unwrap();
    let stats = restored.statistics().unwrap();
    assert_eq!(stats.facts.total, 2);
}

#[cfg(feature = "compress-zstd")]
#[test]
fn zstd_json_restore_roundtrip() {
    let engine = make_engine();
    let dir = tempfile::tempdir().unwrap();
    let zst_path = dir.path().join("dump.json.zst");
    engine
        .dump_state(&DumpFormat::JsonZstd(zst_path.clone()))
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&zst_path).unwrap();
    let stats = restored.statistics().unwrap();
    assert_eq!(stats.facts.total, 2);
}

#[test]
fn post_restore_ids_dont_collide() {
    let engine = make_engine();
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&json_path).unwrap();

    // Get max fact id from the original.
    let original_stats = engine.statistics().unwrap();
    let max_original = original_stats.facts.total;

    // Add facts to restored engine — IDs should not collide.
    for i in 0..5 {
        let id = restored
            .add_fact(
                &AddFactRequest {
                    content: format!("new fact {i}"),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
            .unwrap();
        assert!(
            id > max_original,
            "new id {id} collides with imported range"
        );
    }
}
