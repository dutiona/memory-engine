use std::path::Path;

use crate::error::{ConflictError, MemoryError, Result};
use crate::pool::ConnectionPool;
use crate::store::schema::get_config;
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

    /// Restore from a [`dump_sqlite()`](crate::inspect::dump::dump_sqlite) backup
    /// into a new file-backed engine.
    ///
    /// **Only accepts clean backups** produced by `dump_state(DumpFormat::Sqlite(..))`.
    /// Copying a live WAL-mode database is unsafe (the WAL sidecar may be missing).
    ///
    /// `config.path` must not already exist. The backup's `embed_dim` is validated
    /// against `config.embed_dim`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the target path already exists.
    /// Returns `MemoryError::NotFound` if the backup path is not an existing
    /// regular file (missing, or a directory / other non-file).
    /// Returns `MemoryError::EmbeddingDimension` on dimension mismatch.
    pub fn restore_sqlite(backup_path: &Path, config: &EngineConfig) -> Result<Self> {
        if config.path.exists() {
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

        std::fs::copy(backup_path, &config.path)?;

        // Any failure after copy must clean up the orphan file.
        let cleanup = |e| {
            let _ = std::fs::remove_file(&config.path);
            e
        };

        // Probe the copied DB for embed_dim validation.
        let probe = crate::store::schema::open_connection(&config.path.to_string_lossy())
            .map_err(cleanup)?;
        let db_dim: usize = get_config(&probe, "embed_dim")
            .map_err(cleanup)?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| {
                let _ = std::fs::remove_file(&config.path);
                MemoryError::Internal("backup has no embed_dim in config".into())
            })?;
        drop(probe);

        if config.embed_dim != db_dim {
            let _ = std::fs::remove_file(&config.path);
            return Err(MemoryError::EmbeddingDimension {
                expected: config.embed_dim,
                actual: db_dim,
            });
        }

        Self::open(config).inspect_err(|_| {
            let _ = std::fs::remove_file(&config.path);
        })
    }
}
