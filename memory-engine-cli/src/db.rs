use std::path::Path;

use memory_engine::{EngineConfig, MemoryEngine};

/// Open a `MemoryEngine` from an existing database file.
///
/// Reads `embed_dim` from the database config table so the user
/// doesn't have to specify it manually.
///
/// # Caveat
///
/// `MemoryEngine::open()` may run schema migrations on the writable pool.
/// We mitigate this by setting `backup_dir` next to the database so any
/// migration creates a WAL-safe backup first. A true read-only open path
/// requires library support (tracked as a follow-up issue).
pub fn open_engine(path: &Path) -> anyhow::Result<MemoryEngine> {
    anyhow::ensure!(path.exists(), "database not found: {}", path.display());

    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let embed_dim: usize = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embed_dim'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| {
            anyhow::anyhow!(
                "database has no embed_dim in config table — is this a memory-engine database?"
            )
        })?
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid embed_dim value in config table"))?;
    drop(conn);

    let backup_dir = path.parent().map(Path::to_path_buf);
    let mut config = EngineConfig::new(path.to_path_buf(), embed_dim);
    config.backup_dir = backup_dir;
    let engine = MemoryEngine::open(&config)?;
    Ok(engine)
}
