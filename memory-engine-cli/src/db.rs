use std::path::Path;

use memory_engine::{EngineConfig, MemoryEngine};

/// Peek `embed_dim` from an existing database's config table.
fn peek_embed_dim(path: &Path) -> anyhow::Result<usize> {
    anyhow::ensure!(path.exists(), "database not found: {}", path.display());

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

/// Open a `MemoryEngine` in **read-only** mode.
///
/// Uses the library's `read_only` config flag so the connection pool
/// never acquires a write lock and never runs migrations.
pub fn open_engine(path: &Path) -> anyhow::Result<MemoryEngine> {
    let embed_dim = peek_embed_dim(path)?;
    let mut config = EngineConfig::new(path.to_path_buf(), embed_dim);
    config.read_only = true;
    let engine = MemoryEngine::open(&config)?;
    Ok(engine)
}

/// Open a `MemoryEngine` with **write** capability.
///
/// Needed for commands that mutate the database (e.g., `add-fact`, `export`
/// with SQLite format). Sets `backup_dir` next to the database so any
/// schema migration creates a WAL-safe backup first.
pub fn open_engine_writable(path: &Path) -> anyhow::Result<MemoryEngine> {
    let embed_dim = peek_embed_dim(path)?;
    let backup_dir = path.parent().map(Path::to_path_buf);
    let mut config = EngineConfig::new(path.to_path_buf(), embed_dim);
    config.backup_dir = backup_dir;
    let engine = MemoryEngine::open(&config)?;
    Ok(engine)
}
