use std::path::{Path, PathBuf};

use crate::error::{ConflictError, MemoryError, MigrationError, Result};
use crate::pool::ConnectionPool;
use crate::store::upcaster::UpcasterRegistry;

use super::{EngineConfig, MemoryEngine};

impl MemoryEngine {
    /// Restore from a JSON snapshot into a new file-backed engine.
    ///
    /// `config.path` must not already exist. The `config.embed_dim` is validated
    /// against the snapshot's `embed_dim`. All other config fields (pool size,
    /// search config, upcaster registry, backup dir) are passed through.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the target path already exists.
    /// Returns `MemoryError::EmbeddingDimension` if `config.embed_dim` mismatches.
    /// Returns `MemoryError::Io` / `MemoryError::Serialization` on read failure.
    pub fn restore_json(snapshot_path: &Path, config: &EngineConfig) -> Result<Self> {
        if config.path.exists() {
            return Err(MemoryError::Conflict(ConflictError::TargetExists));
        }

        let snapshot = crate::inspect::restore::read_snapshot(snapshot_path)?;

        if config.embed_dim != snapshot.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: config.embed_dim,
                actual: snapshot.embed_dim,
            });
        }

        let pool = ConnectionPool::open(
            &config.path,
            config.embed_dim,
            config.read_pool_size,
            config.backup_dir.as_deref(),
        );

        // On any failure after DB creation, clean up the orphan file.
        let pool = match pool {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&config.path);
                return Err(e);
            }
        };

        let restore_result = {
            let conn = pool.write();
            crate::inspect::restore::restore_snapshot_into(&conn, &snapshot)
        };

        if let Err(e) = restore_result {
            drop(pool);
            let _ = std::fs::remove_file(&config.path);
            return Err(e);
        }

        Self::init_from_pool(
            pool,
            config.embed_dim,
            config.search_config.clone(),
            config.upcaster_registry.clone(),
            None,
        )
    }

    /// Restore from a JSON snapshot into a new in-memory engine.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Io` / `MemoryError::Serialization` on read failure.
    pub fn restore_json_memory(snapshot_path: &Path) -> Result<Self> {
        let snapshot = crate::inspect::restore::read_snapshot(snapshot_path)?;
        let pool = ConnectionPool::open_memory(snapshot.embed_dim)?;

        {
            let conn = pool.write();
            crate::inspect::restore::restore_snapshot_into(&conn, &snapshot)?;
        }

        Self::init_from_pool(
            pool,
            snapshot.embed_dim,
            None,
            UpcasterRegistry::new(),
            None,
        )
    }

    /// Restore from a `dump_sqlite()` backup
    /// into a new file-backed engine.
    ///
    /// **Only accepts clean backups** produced by `dump_state(DumpFormat::Sqlite(..))`.
    /// Copying a live WAL-mode database is unsafe (the WAL sidecar may be missing).
    ///
    /// Neither `config.path` nor its `-wal`, `-shm` or `-journal` sidecar may already
    /// exist, and the backup may have no non-empty `-wal` or `-journal` of its own. A backup of
    /// any schema version is accepted (#1049): its recorded dimension is read where it lies,
    /// before anything migrates, and validated against `config.embed_dim`; the open then
    /// migrates it to the current schema.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the target path or one of its sidecars
    /// already exists.
    /// Returns `MemoryError::NotFound` if the backup path is not an existing
    /// regular file (missing, or a directory / other non-file).
    /// Returns `MemoryError::Migration` if the backup has a non-empty `-wal` or
    /// `-journal`, or records no `schema_version` (it is not a memory-engine database).
    /// Returns `MemoryError::EmbeddingDimension` on dimension mismatch.
    pub fn restore_sqlite(backup_path: &Path, config: &EngineConfig) -> Result<Self> {
        // SQLite replays a leftover `-wal` or `-journal` onto whatever file sits at the
        // path, so a stale sidecar would corrupt the copied backup (#1040).
        let has_sidecar = ["-wal", "-shm", "-journal"]
            .iter()
            .any(|suffix| sidecar(&config.path, suffix).exists());
        if config.path.exists() || has_sidecar {
            return Err(MemoryError::Conflict(ConflictError::TargetExists));
        }
        // Use `is_file()` (not `exists()`): a directory passed as the backup
        // source would pass an `exists()` guard and then fail deep inside
        // `std::fs::copy` with a confusing OS-level error. `is_file()` rejects
        // directories and other non-regular files up front with a clear message.
        if !backup_path.is_file() {
            return Err(MemoryError::NotFound(format!(
                "backup file is not a regular file: {}",
                backup_path.display()
            )));
        }
        // A backup's own non-empty `-wal` holds commits its main file lacks, and a hot
        // `-journal` holds the rollback its main file still needs: copying the main file
        // alone would drop the former or leave the latter torn (#1040).
        for suffix in ["-wal", "-journal"] {
            if std::fs::metadata(sidecar(backup_path, suffix)).is_ok_and(|m| m.len() > 0) {
                return Err(MigrationError::Incompatible(format!(
                    "backup {} has a non-empty {suffix}: it is not a clean \
                     dump_state(DumpFormat::Sqlite) backup",
                    backup_path.display()
                ))
                .into());
            }
        }

        std::fs::copy(backup_path, &config.path)?;

        // Any failure after copy must clean up the orphan file and any sidecar a probe left:
        // none existed before (checked above).
        let cleanup = |e| {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(sidecar(&config.path, suffix));
            }
            e
        };

        // A copy with no `schema_version` is not a memory-engine store: refuse it before the
        // writable open would create our schema inside it (the reader alone cannot tell it from
        // a blank store). The probe connection is closed before any cleanup: removing the file
        // under an open WAL connection leaves its `-wal` and `-shm` behind.
        let has_version = {
            let probe =
                crate::store::schema::open_connection_read_only(&config.path.to_string_lossy())
                    .map_err(cleanup)?;
            crate::store::schema::holds_engine_schema(&probe).map_err(cleanup)?
        };
        if !has_version {
            return Err(cleanup(
                MigrationError::Incompatible(format!(
                    "backup {} is not a memory-engine database",
                    backup_path.display()
                ))
                .into(),
            ));
        }

        // A backup that recorded an identity, at any schema version, must agree on dimension;
        // one with none has nothing to compare (#1049: read where it lies, before migrating).
        let recorded = super::read_recorded_embedding(&config.path).map_err(cleanup)?;
        if let Some(stored) = recorded.dim()
            && config.embed_dim != stored
        {
            return Err(cleanup(MemoryError::EmbeddingDimension {
                expected: config.embed_dim,
                actual: stored,
            }));
        }

        Self::open_from_config(config, None).map_err(cleanup)
    }
}

/// `db` with `suffix` appended to its file name (`memory.db` -> `memory.db-wal`).
fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut path = db.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}
