//! Integration tests for import/export round-trips.

#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::EmbeddingFingerprint;
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
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", 4)
    }
}

/// Create a populated engine for testing.
async fn make_engine() -> MemoryEngine {
    let engine = MemoryEngine::builder(DIM).build().unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "alpha fact".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
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
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    engine
}

#[tokio::test]
async fn json_restore_to_file_engine() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();

    let target_path = dir.path().join("restored.db");
    let config = EngineConfig::new(target_path, DIM);
    let restored = MemoryEngine::restore_json(&json_path, &config).unwrap();

    // Verify data.
    let stats = restored.statistics().await.unwrap();
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
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    assert!(new_id > 2, "new id {new_id} should be > max imported id 2");
}

#[tokio::test]
async fn json_restore_to_memory_engine() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&json_path).unwrap();
    let stats = restored.statistics().await.unwrap();
    assert_eq!(stats.facts.total, 2);
}

#[tokio::test]
async fn sqlite_restore_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.db");
    let engine = MemoryEngine::builder(DIM)
        .path(source_path)
        .build()
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "sqlite test".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();

    // Dump to SQLite backup.
    let backup_path = dir.path().join("backup.db");
    engine
        .dump_state(&DumpFormat::Sqlite(backup_path.clone()))
        .await
        .unwrap();
    drop(engine);

    // Restore from backup.
    let target_path = dir.path().join("restored.db");
    let target_config = EngineConfig::new(target_path, DIM);
    let restored = MemoryEngine::restore_sqlite(&backup_path, &target_config).unwrap();

    let stats = restored.statistics().await.unwrap();
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
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    assert!(new_id > 1);
}

/// #1040: a WAL left by an unclean stop would be replayed onto the copied backup, so a
/// leftover sidecar makes the target count as existing.
#[tokio::test]
async fn restore_sqlite_fails_if_target_sidecar_exists() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let backup_path = dir.path().join("backup.db");
    engine
        .dump_state(&DumpFormat::Sqlite(backup_path.clone()))
        .await
        .unwrap();

    for suffix in ["-wal", "-shm", "-journal"] {
        let target_path = dir.path().join("restored.db");
        let sidecar = dir.path().join(format!("restored.db{suffix}"));
        std::fs::write(&sidecar, "stale").unwrap();
        let config = EngineConfig::new(target_path.clone(), DIM);
        let err = MemoryEngine::restore_sqlite(&backup_path, &config).unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "{suffix}: expected TargetExists, got {err}"
        );
        assert!(!target_path.exists(), "{suffix}: nothing may be copied");
        std::fs::remove_file(&sidecar).unwrap();
    }
}

/// #1040: a backup's non-empty `-wal` holds commits its main file lacks, and a hot
/// `-journal` the rollback it still needs, so restoring the main file alone is refused.
#[tokio::test]
async fn restore_sqlite_refuses_backup_with_journal_sidecar() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let backup_path = dir.path().join("backup.db");
    engine
        .dump_state(&DumpFormat::Sqlite(backup_path.clone()))
        .await
        .unwrap();

    for suffix in ["-wal", "-journal"] {
        let sidecar = dir.path().join(format!("backup.db{suffix}"));
        std::fs::write(&sidecar, "pending").unwrap();
        let target_path = dir.path().join("restored.db");
        let config = EngineConfig::new(target_path.clone(), DIM);
        let err = MemoryEngine::restore_sqlite(&backup_path, &config).unwrap_err();
        assert!(
            matches!(err, memory_engine::error::MemoryError::Migration(_))
                && err.to_string().contains(suffix),
            "{suffix}: got {err}"
        );
        assert!(!target_path.exists(), "{suffix}: nothing may be copied");
        std::fs::remove_file(&sidecar).unwrap();
    }

    // An empty `-wal` (what a read-only open leaves) holds no commits.
    std::fs::write(dir.path().join("backup.db-wal"), "").unwrap();
    let config = EngineConfig::new(dir.path().join("restored.db"), DIM);
    MemoryEngine::restore_sqlite(&backup_path, &config).unwrap();
}

/// #1040: a probe failure must not leave the target's WAL sidecars behind, or the next
/// restore to the same path is refused.
#[tokio::test]
async fn restore_sqlite_probe_failure_leaves_no_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let backup_path = dir.path().join("not-an-engine.db");
    rusqlite::Connection::open(&backup_path)
        .unwrap()
        .execute_batch("CREATE TABLE t(x);")
        .unwrap();

    let config = EngineConfig::new(dir.path().join("restored.db"), DIM);
    MemoryEngine::restore_sqlite(&backup_path, &config).unwrap_err();
    for suffix in ["", "-wal", "-shm"] {
        let leftover = dir.path().join(format!("restored.db{suffix}"));
        assert!(!leftover.exists(), "left behind: {}", leftover.display());
    }
}

#[tokio::test]
async fn restore_json_fails_if_target_exists() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();

    // Create the target file so it exists.
    let target_path = dir.path().join("existing.db");
    std::fs::write(&target_path, "placeholder").unwrap();

    let config = EngineConfig::new(target_path, DIM);
    let err = MemoryEngine::restore_json(&json_path, &config).unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[tokio::test]
async fn restore_json_fails_on_embed_dim_mismatch() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();

    let target_path = dir.path().join("restored.db");
    // Config says dim=8 but snapshot has dim=4.
    let config = EngineConfig::new(target_path, 8);
    let err = MemoryEngine::restore_json(&json_path, &config).unwrap_err();
    assert!(err.to_string().contains("embedding dimension"));
}

#[tokio::test]
async fn restore_sqlite_fails_if_backup_missing() {
    let dir = tempfile::tempdir().unwrap();
    let backup_path = dir.path().join("nonexistent.db");
    let target_path = dir.path().join("target.db");
    let config = EngineConfig::new(target_path, DIM);
    let err = MemoryEngine::restore_sqlite(&backup_path, &config).unwrap_err();
    assert!(err.to_string().contains("backup file"));
}

/// Regression for #191: a directory passed as the backup source must be
/// rejected by the `is_file()` guard with a precise `NotFound` ("not a regular
/// file") error, *before* `std::fs::copy` runs. With the old `exists()` guard
/// the directory passed the check and `copy` later failed with a confusing
/// OS-level I/O error.
#[tokio::test]
async fn restore_sqlite_fails_if_backup_is_directory() {
    let dir = tempfile::tempdir().unwrap();

    // The backup "source" is a directory, not a file.
    let backup_dir = dir.path().join("a_directory");
    std::fs::create_dir(&backup_dir).unwrap();

    let target_path = dir.path().join("target.db");
    let config = EngineConfig::new(target_path.clone(), DIM);

    let err = MemoryEngine::restore_sqlite(&backup_dir, &config).unwrap_err();

    // Must be the precise pre-copy guard error, not a copy/I/O error.
    assert!(
        matches!(err, memory_engine::error::MemoryError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("backup file") && msg.contains("not a regular file"),
        "expected a 'not a regular file' message, got: {msg}"
    );
    assert!(
        !msg.contains("I/O error"),
        "must not surface a raw copy/I/O error, got: {msg}"
    );

    // The guard fired before any copy, so no orphan target file was created.
    assert!(
        !target_path.exists(),
        "target file should not have been created when the guard rejects the source"
    );
}

#[cfg(feature = "compress-gzip")]
#[tokio::test]
async fn gzip_json_restore_roundtrip() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let gz_path = dir.path().join("dump.json.gz");
    engine
        .dump_state(&DumpFormat::JsonGzip(gz_path.clone()))
        .await
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&gz_path).unwrap();
    let stats = restored.statistics().await.unwrap();
    assert_eq!(stats.facts.total, 2);
}

#[cfg(feature = "compress-zstd")]
#[tokio::test]
async fn zstd_json_restore_roundtrip() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let zst_path = dir.path().join("dump.json.zst");
    engine
        .dump_state(&DumpFormat::JsonZstd(zst_path.clone()))
        .await
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&zst_path).unwrap();
    let stats = restored.statistics().await.unwrap();
    assert_eq!(stats.facts.total, 2);
}

#[tokio::test]
async fn post_restore_ids_dont_collide() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();

    let restored = MemoryEngine::restore_json_memory(&json_path).unwrap();

    // Get max fact id from the original.
    let original_stats = engine.statistics().await.unwrap();
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
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        assert!(
            id > max_original,
            "new id {id} collides with imported range"
        );
    }
}

fn forget_record() -> memory_engine::types::NewEvent {
    memory_engine::types::NewEvent {
        timestamp: chrono::Utc::now(),
        event_type: memory_engine::types::EventType::MemoryOp,
        payload: serde_json::json!({}),
        source: "test".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "test".into(),
        sequence_id: 0,
        created_at: None,
    }
}

/// A JSON export carries the tombstones (#1023): a restored store still knows the fact is
/// forgotten, still blocks its content and can still unforget it.
#[tokio::test]
async fn export_and_import_carry_tombstones() {
    let engine = make_engine().await;
    let id = engine.list_active_facts(None).await.unwrap()[0].id;
    let content = engine.get_fact(id).await.unwrap().content;
    engine
        .forget_fact(
            id,
            &["/".into()],
            "wrong",
            "unscoped:test",
            &forget_record(),
        )
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();
    let restored = MemoryEngine::restore_json_memory(&json_path).unwrap();
    assert!(matches!(
        restored.explain_fact(id).await.unwrap().state,
        memory_engine::inspect::FactState::Expired {
            reason: memory_engine::inspect::ExpiredReason::Forgotten { .. }
        }
    ));
    let err = restored
        .add_facts_batch(
            &[AddFactRequest {
                content,
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            }],
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            memory_engine::MemoryError::Conflict(memory_engine::ConflictError::Forgotten)
        ),
        "{err:?}"
    );
    assert!(matches!(
        restored.unforget_fact(id).await.unwrap(),
        memory_engine::types::UnforgetOutcome::Restored { .. }
    ));
}

/// A tombstone naming a fact or an event the snapshot lacks is refused before any write.
#[tokio::test]
async fn a_snapshot_with_a_dangling_tombstone_is_refused() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
    for stone in [
        serde_json::json!({ "fact_id": 999, "content_hash": "0".repeat(32),
            "reason": null, "forgotten_at": "2026-01-01T00:00:00Z", "record_event_id": null,
            "prior_t_expired": null, "prior_t_invalid": null }),
        serde_json::json!({ "fact_id": null, "content_hash": "0".repeat(32),
            "reason": null, "forgotten_at": "2026-01-01T00:00:00Z", "record_event_id": 999,
            "prior_t_expired": null, "prior_t_invalid": null }),
        serde_json::json!({ "fact_id": null, "content_hash": "short",
            "reason": null, "forgotten_at": "2026-01-01T00:00:00Z", "record_event_id": null,
            "prior_t_expired": null, "prior_t_invalid": null }),
    ] {
        snapshot["tombstones"] = serde_json::json!([stone]);
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let target = dir.path().join("restored.db");
        let config = EngineConfig::new(target.clone(), DIM);
        assert!(
            MemoryEngine::restore_json(&bad, &config).is_err(),
            "{stone}"
        );
        assert!(!target.exists(), "nothing written for {stone}");
    }
}

// ---------------------------------------------------------------------------
// #1049: a backup from before schema v15 restores and migrates
// ---------------------------------------------------------------------------

/// A `dump_state(Sqlite)` backup of a store stamped `mock/test/4`, rolled back by raw SQL to
/// `version` (18 keeps the current layout): 14 (no served columns), 13 (also no `fact_vectors`), 12 (the registry folded
/// back into the `embedding_meta` config value). `waiting` makes a store whose one fact waits
/// for its vector instead (no identity, only `embed_dim`).
async fn older_backup(dir: &std::path::Path, version: u32, waiting: bool) -> std::path::PathBuf {
    let source = dir.join(format!("source-{version}-{waiting}.db"));
    let mut engine = MemoryEngine::builder(DIM).path(&source).build().unwrap();
    let request = AddFactRequest {
        content: "an older fact".into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: None,
        opts: None,
    };
    if waiting {
        engine.add_fact_deferred(&request).await.unwrap();
    } else {
        engine
            .add_fact(
                &request,
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
    }
    engine.close().await.unwrap();
    let mut engine = MemoryEngine::builder(DIM).path(&source).build().unwrap();
    let backup = dir.join(format!("backup-{version}-{waiting}.db"));
    engine
        .dump_state(&DumpFormat::Sqlite(backup.clone()))
        .await
        .unwrap();
    engine.close().await.unwrap();
    let conn = rusqlite::Connection::open(&backup).unwrap();
    if version <= 14 {
        conn.execute_batch(
            "ALTER TABLE embedding_spaces DROP COLUMN artifact_sha256;
             ALTER TABLE embedding_spaces DROP COLUMN pooling;
             ALTER TABLE embedding_spaces DROP COLUMN normalization;
             ALTER TABLE embedding_spaces DROP COLUMN max_input_tokens;",
        )
        .unwrap();
    }
    if version <= 13 {
        conn.execute_batch("DROP TABLE fact_vectors;").unwrap();
    }
    if version == 12 {
        let meta: Option<String> = conn
            .query_row(
                "SELECT json_object('model', model, 'provider', provider, 'dim', dim,
                        'matryoshka_base_dim', matryoshka_base_dim, 'element_type', element_type)
                   FROM embedding_spaces WHERE status = 'active'",
                [],
                |r| r.get(0),
            )
            .ok();
        if let Some(meta) = meta {
            conn.execute(
                "INSERT INTO config (key, value) VALUES ('embedding_meta', ?1)",
                [meta],
            )
            .unwrap();
        }
        conn.execute_batch("DROP TABLE embedding_spaces;").unwrap();
    }
    conn.execute(
        "UPDATE config SET value = ?1 WHERE key = 'schema_version'",
        [version.to_string()],
    )
    .unwrap();
    drop(conn);
    backup
}

#[tokio::test]
async fn restore_sqlite_migrates_an_older_backup() {
    let dir = tempfile::tempdir().unwrap();
    for version in [14, 13, 12] {
        let backup = older_backup(dir.path(), version, false).await;
        let target = dir.path().join(format!("restored-{version}.db"));
        let mut restored =
            MemoryEngine::restore_sqlite(&backup, &EngineConfig::new(target.clone(), DIM))
                .unwrap_or_else(|e| panic!("v{version}: {e}"));
        assert_eq!(
            restored.statistics().await.unwrap().facts.total,
            1,
            "v{version}"
        );
        restored.close().await.unwrap();
        assert_eq!(
            memory_engine::read_recorded_embedding(&target).unwrap(),
            memory_engine::RecordedEmbedding::Identity(EmbeddingFingerprint::new(
                "mock", "test", 4
            )),
            "v{version}"
        );
        let version_now: String = rusqlite::Connection::open(&target)
            .unwrap()
            .query_row(
                "SELECT value FROM config WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            version_now,
            memory_engine::CURRENT_SCHEMA_VERSION.to_string(),
            "v{version}"
        );
    }
}

#[tokio::test]
async fn restore_sqlite_refuses_an_older_backup_of_another_dimension() {
    let dir = tempfile::tempdir().unwrap();
    for (version, waiting) in [(14, false), (12, false), (18, true)] {
        let backup = older_backup(dir.path(), version, waiting).await;
        let target = dir.path().join(format!("restored-{version}-{waiting}.db"));
        let err = MemoryEngine::restore_sqlite(&backup, &EngineConfig::new(target.clone(), 8))
            .expect_err("another dimension");
        assert!(
            matches!(
                err,
                memory_engine::MemoryError::EmbeddingDimension {
                    expected: 8,
                    actual: 4
                }
            ),
            "v{version} waiting={waiting}: {err:?}"
        );
        for suffix in ["", "-wal", "-shm"] {
            let mut leftover = target.as_os_str().to_owned();
            leftover.push(suffix);
            assert!(
                !std::path::Path::new(&leftover).exists(),
                "left behind: {leftover:?}"
            );
        }
    }
}

/// One memory written as the MCP `remember` writes it: its call record first, then the fact
/// pointing at that record. Returns `(record, fact)`.
async fn remember(engine: &MemoryEngine, content: &str) -> (i64, i64) {
    let record = engine.ingest(&forget_record()).await.unwrap();
    let fact = engine
        .add_fact(
            &AddFactRequest {
                content: content.into(),
                fact_type: FactType::Semantic,
                source_event_id: Some(record),
                scope: None,
                opts: None,
            },
            std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
            None,
        )
        .await
        .unwrap();
    (record, fact)
}

/// #1131: an erase leaves a gap at the top of the ids; an export carries the high-water marks,
/// so the store it is imported into gives none of the erased ids again and every call record
/// still names its own fact. ("export, erase, restore, import": the erase comes first, or the
/// export would carry the erased fact and test nothing; the import is the write after restore.)
#[tokio::test]
async fn an_import_after_erase_reuses_no_id() {
    let dir = tempfile::tempdir().unwrap();
    let engine = MemoryEngine::builder(DIM)
        .path(dir.path().join("memory.db"))
        .build()
        .unwrap();
    let (_, first) = remember(&engine, "alpha fact").await;
    let (second_record, second) = remember(&engine, "beta fact").await;
    let (_, third) = remember(&engine, "gamma fact").await;
    assert_eq!([first, second, third], [1, 2, 3]);
    engine
        .erase(&memory_engine::types::EraseTarget::Fact(third))
        .await
        .unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();

    let config = EngineConfig::new(dir.path().join("restored.db"), DIM);
    let restored = MemoryEngine::restore_json(&json_path, &config).unwrap();
    let (_, fourth) = remember(&restored, "delta fact").await;

    assert_eq!(fourth, 4, "the erased fact's id is never given again");
    assert!(
        restored.get_fact(third).await.is_err(),
        "fact 3 stays erased"
    );
    assert_eq!(
        restored.get_fact(second).await.unwrap().source_event_id,
        Some(second_record),
        "fact 2 still points at its own call record"
    );
}

/// #1131: a snapshot taken before the marks were exported restores from its rows, as before.
#[tokio::test]
async fn a_snapshot_without_sequences_restores_from_its_rows() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
    assert!(value.as_object_mut().unwrap().remove("sequences").is_some());
    std::fs::write(&json_path, serde_json::to_vec(&value).unwrap()).unwrap();

    let restored = MemoryEngine::restore_json_memory(&json_path).unwrap();
    let (_, next) = remember(&restored, "gamma fact").await;
    assert_eq!(next, 3, "the id after the highest restored row");
}

/// #1131: an older binary reads a snapshot carrying `sequences` because the snapshot type
/// ignores a key it does not know.
#[tokio::test]
async fn an_export_with_sequences_reads_as_a_snapshot_with_an_unknown_field() {
    let engine = make_engine().await;
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("dump.json");
    engine
        .dump_state(&DumpFormat::Json(json_path.clone()))
        .await
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "a_field_from_the_future".into(),
        serde_json::json!({"x": 1}),
    );
    let snapshot: memory_engine::inspect_types::EngineSnapshot =
        serde_json::from_value(value).unwrap();
    assert_eq!(snapshot.facts.len(), 2);
}
