use std::path::Path;

use memory_engine::{EngineConfig, MemoryEngine};

/// Open a `MemoryEngine` from an existing database file.
///
/// Reads `embed_dim` from the database config table so the user
/// doesn't have to specify it manually.
pub fn open_engine(path: &Path) -> anyhow::Result<MemoryEngine> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let embed_dim: usize = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embed_dim'",
            [],
            |row| {
                let s: String = row.get(0)?;
                Ok(s)
            },
        )
        .map_err(|_| {
            anyhow::anyhow!(
                "database has no embed_dim in config table — is this a memory-engine database?"
            )
        })?
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid embed_dim value in config table"))?;
    drop(conn);

    let config = EngineConfig::new(path.to_path_buf(), embed_dim);
    let engine = MemoryEngine::open(&config)?;
    Ok(engine)
}
