//! WAL-safe pre-migration backup via `VACUUM INTO`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use me_types::error::{MigrationError, Result};

/// Create a WAL-safe backup of the database before running migrations.
///
/// Uses `VACUUM INTO` which produces an atomic, consistent copy regardless
/// of WAL state (no sidecar files to worry about).
///
/// # Security
///
/// `backup_dir` MUST be a trusted path supplied by the consumer (it originates
/// from `EngineConfig::backup_dir`). `VACUUM INTO` cannot parameterize its
/// target, so the path is interpolated into SQL with single-quote escaping.
/// The null-byte rejection below is defense-in-depth against an SQL-literal
/// terminator slipping past the escaping — it is **not** a sandbox and does not
/// make an untrusted path safe to pass here.
///
/// # Errors
///
/// Returns `MemoryError::Migration` if the connection is in-memory, the backup
/// path contains a null byte, or the backup path cannot be written to.
pub fn backup_before_migration(
    conn: &Connection,
    backup_dir: &Path,
    current_version: u32,
) -> Result<PathBuf> {
    // Extract the source database file path via PRAGMA database_list
    let db_path: String = conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .map_err(|e| MigrationError::Backup(format!("cannot read database path: {e}")))?;

    if db_path.is_empty() || db_path == ":memory:" {
        return Err(MigrationError::Backup("cannot backup in-memory database".to_string()).into());
    }

    let db_name = Path::new(&db_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let backup_path = backup_dir.join(format!("{db_name}.v{current_version}.bak"));

    // Defense-in-depth: reject a null byte in the backup path before ANY use —
    // the filesystem ops below and the SQL interpolation further down. A NUL
    // would truncate the C string the OS / SQLite receives, silently changing
    // the target path. Validate-before-use / fail-fast. Mirrors the guard in
    // `inspect::dump::dump_sqlite`.
    if backup_path.to_string_lossy().contains('\0') {
        return Err(MigrationError::Backup("backup path contains null byte".to_string()).into());
    }

    // Remove existing backup to avoid VACUUM INTO failure on re-run
    if backup_path.exists() {
        std::fs::remove_file(&backup_path).map_err(|e| {
            MigrationError::Backup(format!(
                "cannot remove existing backup {}: {e}",
                backup_path.display()
            ))
        })?;
    }

    // VACUUM INTO creates a standalone, defragmented copy — WAL-safe.
    // SQLite VACUUM INTO does not support parameterized paths, so we escape
    // single quotes manually (SQLite string literal escaping: ' → '').
    // (The path was already null-byte-validated above, before any use.)
    let escaped = backup_path.to_string_lossy().replace('\'', "''");
    let sql = format!("VACUUM INTO '{escaped}'");
    conn.execute_batch(&sql)
        .map_err(|e| MigrationError::Backup(format!("backup failed: {e}")))?;

    Ok(backup_path)
}
