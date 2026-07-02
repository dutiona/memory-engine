use crate::error::StorageError;
use std::path::Path;

use rusqlite::Connection;

use crate::error::{MemoryError, MigrationError, Result};

mod backup;
mod config;
mod ddl;
mod migrations;
mod pragmas;

#[cfg(test)]
mod tests;

use ddl::{FTS5_DDL, INDEXES_DDL, SCOPES_DDL, TABLES_DDL, TRIGGERS_DDL};
use pragmas::{check_foreign_keys, set_foreign_keys, set_pragmas, set_pragmas_read_only};

// Re-export the public schema helpers so every `crate::store::schema::<item>`
// path keeps resolving after the god-module split (behavior-preserving).
pub use backup::backup_before_migration;
pub use config::{get_config, list_config, set_config};

/// Current schema version. Bump when adding migrations.
pub const CURRENT_SCHEMA_VERSION: u32 = 14;

/// Storage epoch — coarse-grained compatibility gate.
///
/// All schema versions within the same epoch are forwards-compatible via
/// the migration chain. Bumping the epoch signals a breaking architectural
/// change (e.g., dropping old migration support). Libraries reject DBs
/// from future epochs with [`MemoryError::UnsupportedEpoch`].
pub const STORAGE_EPOCH: u16 = 1;

/// Open a `SQLite` connection to a file, with pragmas set.
///
/// # Errors
///
/// Returns `MemoryError::Storage` if the connection or pragma setup fails.
pub fn open_connection(path: &str) -> Result<Connection> {
    let conn = Connection::open(path).map_err(StorageError::backend)?;
    set_pragmas(&conn)?;
    Ok(conn)
}

/// Open a read-only `SQLite` connection to a file, with safe pragmas.
///
/// Uses `SQLITE_OPEN_READ_ONLY` flags — no file creation, no WAL mutation.
/// Skips `journal_mode` and `synchronous` pragmas (read-only connections
/// cannot set them and don't need to).
///
/// # Errors
///
/// Returns `MemoryError::Storage` if the connection or pragma setup fails.
pub fn open_connection_read_only(path: &str) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(StorageError::backend)?;
    set_pragmas_read_only(&conn)?;
    Ok(conn)
}

/// Open an in-memory `SQLite` connection, with pragmas set.
///
/// # Errors
///
/// Returns `MemoryError::Storage` if the connection or pragma setup fails.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory().map_err(StorageError::backend)?;
    set_pragmas(&conn)?;
    Ok(conn)
}

/// Initialize schema for a database.
///
/// **Fresh database (no config table):** Creates the full latest schema and sets
/// `schema_version` to `CURRENT_SCHEMA_VERSION`.
///
/// **Existing database:** Returns immediately — all DDL evolution happens through
/// [`migrate`]. This avoids running v2-only DDL against a v1 schema where new
/// columns don't exist yet.
///
/// # Errors
///
/// Returns `MemoryError::Storage` if any DDL statement fails.
pub fn init_schema(conn: &Connection) -> Result<()> {
    let is_fresh: bool = conn
        .query_row(
            "SELECT COUNT(*) = 0 FROM sqlite_master WHERE type='table' AND name='config'",
            [],
            |r| r.get(0),
        )
        .map_err(StorageError::backend)?;
    if !is_fresh {
        return Ok(()); // existing DB — let migrate() handle evolution
    }
    // Fresh DB: create full latest schema
    conn.execute_batch(TABLES_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(SCOPES_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(FTS5_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(TRIGGERS_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(INDEXES_DDL)
        .map_err(StorageError::backend)?;
    set_config(conn, "schema_version", &CURRENT_SCHEMA_VERSION.to_string())?;
    set_config(conn, "storage_epoch", &STORAGE_EPOCH.to_string())?;
    Ok(())
}

// --- Migration framework ---

type MigrationFn = fn(&Connection) -> Result<()>;

/// `(function, disable_foreign_keys)` — set second element to `true` for
/// table-rebuild migrations that DROP and recreate tables with FK references.
const MIGRATIONS: &[(MigrationFn, bool)] = &[
    (migrations::migrate_v1_to_v2, false),
    (migrations::migrate_v2_to_v3, true),
    (migrations::migrate_v3_to_v4, false),
    (migrations::migrate_v4_to_v5, false),
    (migrations::migrate_v5_to_v6, false),
    (migrations::migrate_v6_to_v7, false),
    (migrations::migrate_v7_to_v8, false),
    (migrations::migrate_v8_to_v9, false),
    (migrations::migrate_v9_to_v10, false),
    (migrations::migrate_v10_to_v11, false),
    (migrations::migrate_v11_to_v12, false),
    (migrations::migrate_v12_to_v13, false),
    (migrations::migrate_v13_to_v14, false),
];

/// Run forward-only migrations from the current schema version to
/// `CURRENT_SCHEMA_VERSION`.
///
/// Each migration runs inside a transaction. On failure, the migration rolls
/// back and the version is NOT bumped.
///
/// When `backup_dir` is `Some`, a WAL-safe backup is created via `VACUUM INTO`
/// before running any migration functions. Pass `None` for in-memory databases
/// or when backup is not desired.
///
/// # Errors
///
/// Returns `MemoryError::UnsupportedEpoch` if the DB is from a future epoch.
/// Returns `MemoryError::Migration` if the stored version is newer than
/// supported, or if any migration step fails.
pub fn migrate(conn: &Connection, backup_dir: Option<&Path>) -> Result<()> {
    let version_str = get_config(conn, "schema_version")?.unwrap_or_else(|| "1".to_string());
    let version: u32 = version_str.parse().map_err(|_| {
        MigrationError::Incompatible(format!("invalid schema_version: {version_str}"))
    })?;

    // --- Epoch gate ---
    let epoch_str = get_config(conn, "storage_epoch")?;
    let epoch_raw = epoch_str.as_deref().unwrap_or("1"); // pre-epoch DBs are implicitly epoch 1
    let db_epoch: u16 = epoch_raw
        .parse()
        .map_err(|_| MigrationError::Incompatible(format!("invalid storage_epoch: {epoch_raw}")))?;
    if db_epoch > STORAGE_EPOCH {
        return Err(MemoryError::UnsupportedEpoch {
            db_epoch,
            supported_epoch: STORAGE_EPOCH,
        });
    }

    if version > CURRENT_SCHEMA_VERSION {
        return Err(MigrationError::SchemaVersionUnsupported {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        }
        .into());
    }

    // Nothing to migrate
    if version == CURRENT_SCHEMA_VERSION {
        // Ensure epoch is set for pre-epoch DBs that are already at latest version
        if epoch_str.is_none() {
            set_config(conn, "storage_epoch", &STORAGE_EPOCH.to_string())?;
        }
        return Ok(());
    }

    // --- WAL-safe backup before migration ---
    if let Some(dir) = backup_dir {
        backup_before_migration(conn, dir, version)?;
    }

    for (i, (migration, disable_fk)) in MIGRATIONS.iter().enumerate() {
        let target = u32::try_from(i + 2).unwrap_or(u32::MAX); // migrations are 1→2, 2→3, etc.
        if version < target {
            if *disable_fk {
                set_foreign_keys(conn, false)?;
            }
            let result: Result<()> = (|| {
                let tx = conn
                    .unchecked_transaction()
                    .map_err(StorageError::backend)?;
                migration(&tx)?;
                if *disable_fk {
                    // Verify FK integrity BEFORE committing. PRAGMA foreign_key_check
                    // works regardless of the foreign_keys setting — it's an explicit
                    // scan, not runtime enforcement. If the rebuilt tables contain
                    // orphan references, we abort here and the transaction rolls back.
                    check_foreign_keys(&tx)?;
                }
                set_config(&tx, "schema_version", &target.to_string())?;
                tx.commit().map_err(StorageError::backend)?;
                Ok(())
            })();
            if *disable_fk {
                // Re-enable FK enforcement unconditionally, even if migration failed.
                // Transaction rollback already restored the data, but the
                // connection-level PRAGMA must be restored explicitly.
                set_foreign_keys(conn, true)?;
            }
            result?;
        }
    }

    // Stamp epoch for pre-epoch migrated DBs
    if epoch_str.is_none() {
        set_config(conn, "storage_epoch", &STORAGE_EPOCH.to_string())?;
    }

    Ok(())
}

/// Validate that the database schema is compatible with this library version
/// without attempting any writes (no migrations, no init).
///
/// Returns `Ok(())` if the schema version matches [`CURRENT_SCHEMA_VERSION`]
/// and the epoch is compatible. Returns an error if:
/// - The database has no config table (fresh/uninitialized)
/// - The schema version is newer than supported
/// - The schema version is older and needs migration
/// - The storage epoch is from the future
///
/// This is the read-only counterpart of [`init_schema`] + [`migrate`].
pub fn validate_schema_version(conn: &Connection) -> Result<()> {
    // Check if config table exists
    let has_config: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='config'",
            [],
            |r| r.get(0),
        )
        .map_err(StorageError::backend)?;

    if !has_config {
        return Err(MigrationError::Incompatible(
            "database has no config table; cannot open read-only on an uninitialized database"
                .to_string(),
        )
        .into());
    }

    // Check epoch
    let epoch_str = get_config(conn, "storage_epoch")?;
    let epoch_raw = epoch_str.as_deref().unwrap_or("1");
    let db_epoch: u16 = epoch_raw
        .parse()
        .map_err(|_| MigrationError::Incompatible(format!("invalid storage_epoch: {epoch_raw}")))?;
    if db_epoch > STORAGE_EPOCH {
        return Err(MemoryError::UnsupportedEpoch {
            db_epoch,
            supported_epoch: STORAGE_EPOCH,
        });
    }

    // Check schema version
    let version_str = get_config(conn, "schema_version")?.unwrap_or_else(|| "1".to_string());
    let version: u32 = version_str.parse().map_err(|_| {
        MigrationError::Incompatible(format!("invalid schema_version: {version_str}"))
    })?;

    if version > CURRENT_SCHEMA_VERSION {
        return Err(MigrationError::SchemaVersionUnsupported {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        }
        .into());
    }

    if version < CURRENT_SCHEMA_VERSION {
        return Err(MigrationError::SchemaVersionNeedsMigration {
            found: version,
            target: CURRENT_SCHEMA_VERSION,
        }
        .into());
    }

    Ok(())
}
