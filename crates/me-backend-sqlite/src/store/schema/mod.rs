use me_types::error::StorageError;
use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use me_types::error::{MemoryError, MigrationError, Result};

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
pub(crate) use migrations::decode_v12_identity;

/// Current schema version. Bump when adding migrations.
///
/// This is **`SQLite`'s physical migration counter** and nothing else. It is *not*
/// comparable to another backend's (Postgres numbers its own migrations independently,
/// `CURRENT_PG_SCHEMA_VERSION` = 1), and it is **not** the version stamped into a `.pak`
/// archive — that is `me_types::types::archive::ARCHIVE_SCHEMA_VERSION`, a
/// backend-independent *content*-schema version. Bumping this constant does **not** bump
/// the archive one; if a migration also changes the shape of the `Fact`/`Edge` DTOs a
/// `.pak` serializes, `ARCHIVE_SCHEMA_VERSION` must be bumped too (the `ArchivePak`
/// serde-shape test in `archive/types.rs` reddens to remind you). (Wave 2 #816 / S4.)
pub const CURRENT_SCHEMA_VERSION: u32 = 18;

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
/// Uses `SQLITE_OPEN_READ_ONLY` flags: the database file is never created or written.
/// A WAL-mode database still needs a writable directory unless its `-wal` and `-shm`
/// already exist: SQLite creates them when missing, and a read-only connection leaves
/// them behind on close.
/// Skips `journal_mode` (a read-write open records it in the file) and `synchronous`
/// (it only governs writes).
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
/// `schema_version` to `CURRENT_SCHEMA_VERSION`, in one transaction: a first open that fails
/// or dies midway leaves no partial schema, which a writable open would refuse as foreign
/// ([`refuse_foreign_file`]). The freshness test runs inside that write transaction, so two
/// openers cannot both create the schema.
///
/// **Existing database:** Returns immediately — all DDL evolution happens through
/// [`migrate`]. This avoids running v2-only DDL against a v1 schema where new
/// columns don't exist yet.
///
/// # Errors
///
/// Returns `MemoryError::Storage` if any DDL statement fails.
pub fn init_schema(conn: &Connection) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(StorageError::backend)?;
    let is_fresh: bool = tx
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
    for ddl in [TABLES_DDL, SCOPES_DDL, FTS5_DDL, TRIGGERS_DDL, INDEXES_DDL] {
        tx.execute_batch(ddl).map_err(StorageError::backend)?;
    }
    set_config(&tx, "schema_version", &CURRENT_SCHEMA_VERSION.to_string())?;
    set_config(&tx, "storage_epoch", &STORAGE_EPOCH.to_string())?;
    tx.commit().map_err(StorageError::backend)?;
    Ok(())
}

/// Whether `conn` holds a memory-engine store: a `config(key, value)` table holding
/// `schema_version`, which every store since the first records at init (#1133).
///
/// # Errors
///
/// Returns `MemoryError::Storage` on a query failure.
pub fn holds_engine_schema(conn: &Connection) -> Result<bool> {
    let columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('config') WHERE name IN ('key', 'value')",
            [],
            |r| r.get(0),
        )
        .map_err(StorageError::backend)?;
    Ok(columns == 2 && get_config(conn, "schema_version")?.is_some())
}

/// Refuse to open writable an existing SQLite file that holds a schema but no memory-engine
/// store (#1133): the writable open's first write (`journal_mode = WAL`) and its DDL would change
/// another application's file. A missing path, a zero-length file and a database with no schema
/// object are blank and accepted. The check reads through one read-only connection.
///
/// # Errors
///
/// `MemoryError::Migration` (`Incompatible`) for such a file; `MemoryError::Storage` when the file
/// cannot be read (busy, I/O, not a database).
pub(crate) fn refuse_foreign_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let conn = open_connection_read_only(&path.to_string_lossy())?;
    let blank: bool = conn
        .query_row("SELECT COUNT(*) = 0 FROM sqlite_master", [], |r| r.get(0))
        .map_err(StorageError::backend)?;
    if blank || holds_engine_schema(&conn)? {
        return Ok(());
    }
    Err(MigrationError::Incompatible(format!(
        "{} is not a memory-engine database: it holds a schema but no schema_version",
        path.display()
    ))
    .into())
}

// --- Migration framework ---

// Migrations run inside the per-step transaction opened by `migrate` (#938): the type is
// `fn(&Transaction)`, not `fn(&Connection)`, so a migration physically cannot be invoked
// against a bare auto-committing connection — the multi-statement table rebuilds must be
// all-or-nothing. (The connection-level FK pragma toggles stay outside the tx, on
// `&Connection`, since `PRAGMA foreign_keys` is not transactional.)
type MigrationFn = fn(&Transaction) -> Result<()>;

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
    (migrations::migrate_v14_to_v15, false),
    (migrations::migrate_v15_to_v16, false),
    (migrations::migrate_v16_to_v17, false),
    (migrations::migrate_v17_to_v18, false),
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
///
/// # Errors
///
/// Returns `MemoryError::Storage`/`MemoryError::Migration` for any of the
/// conditions listed above (missing config table, unsupported/stale schema
/// version, or a future storage epoch).
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
