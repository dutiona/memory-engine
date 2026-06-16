use std::path::Path;

use memory_engine::MemoryEngine;

/// Peek `embed_dim` from an existing `SQLite` database's `config` table.
///
/// Opens a transient read-only connection and reads the `embed_dim` key.
/// Distinct from the import path's snapshot-header reader (see
/// `peek_embed_dim_from_snapshot` in `commands::import`).
fn peek_embed_dim_from_db(path: &Path) -> anyhow::Result<usize> {
    anyhow::ensure!(
        path.is_file(),
        "database not found (or is a directory): {}",
        path.display()
    );

    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let raw: String = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embed_dim'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "database has no embed_dim in config table — is this a memory-engine database? ({e})"
            )
        })?;
    raw.parse()
        .map_err(|_| anyhow::anyhow!("invalid embed_dim value in config table"))
}

/// Peek the live `schema_version` from an existing database's `config` table.
///
/// Opens a transient read-only connection and reads the version **without**
/// opening the engine — the engine's read-only build validates the schema and
/// the writable build migrates, so neither can inspect a stale database. Used by
/// the `migrate` / `schema` operator commands to decide whether to migrate.
pub fn peek_schema_version_from_db(path: &Path) -> anyhow::Result<u32> {
    anyhow::ensure!(
        path.is_file(),
        "database not found (or is a directory): {}",
        path.display()
    );

    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let raw: String = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "database has no schema_version in config table — is this a memory-engine database? ({e})"
            )
        })?;
    raw.parse()
        .map_err(|_| anyhow::anyhow!("invalid schema_version value in config table: {raw:?}"))
}

/// Open a `MemoryEngine` in **read-only** mode.
///
/// Uses the library's `read_only` config flag so the connection pool
/// never acquires a write lock and never runs migrations.
pub fn open_engine(path: &Path) -> anyhow::Result<MemoryEngine> {
    let embed_dim = peek_embed_dim_from_db(path)?;
    let engine = MemoryEngine::builder(embed_dim)
        .path(path.to_path_buf())
        .read_only(true)
        .build()?;
    Ok(engine)
}

/// Open a `MemoryEngine` with **write** capability.
///
/// Needed for commands that mutate the database (e.g., `add-fact`, `export`
/// with `SQLite` format). Sets `backup_dir` next to the database so any
/// schema migration creates a WAL-safe backup first.
pub fn open_engine_writable(path: &Path) -> anyhow::Result<MemoryEngine> {
    let embed_dim = peek_embed_dim_from_db(path)?;
    let backup_dir = path.parent().map(Path::to_path_buf);
    let mut builder = MemoryEngine::builder(embed_dim).path(path.to_path_buf());
    if let Some(dir) = backup_dir {
        builder = builder.backup_dir(dir);
    }
    let engine = builder.build()?;
    Ok(engine)
}
