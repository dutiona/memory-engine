use std::path::Path;

use memory_engine::MemoryEngine;

/// Peek the embedding dimension from an existing `SQLite` database.
///
/// Reads the persisted identity from the `embedding_meta` config row (#613,
/// ADR 0015) and extracts its `dim`. Falls back to the legacy bare `embed_dim`
/// key for pre-#613 databases. Errors if neither is present — a database that was
/// created but never had an embedding written has no recorded dimension; write
/// commands that know the dimension from their input (e.g. `add-fact`'s
/// `--embedding`) should use [`open_engine_writable_with_dim`] instead of peeking.
///
/// Distinct from the import path's snapshot-header reader (see
/// `peek_embed_dim_from_snapshot` in `commands::import`).
///
/// Exposed so the `consolidate` command can size the LLM backend's HTTP embedder to
/// the database's embedding dimension. (`db` is a private module, so this is not part
/// of any public API.)
pub fn peek_embed_dim_from_db(path: &Path) -> anyhow::Result<usize> {
    use rusqlite::OptionalExtension;

    anyhow::ensure!(
        path.is_file(),
        "database not found (or is a directory): {}",
        path.display()
    );

    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    // Preferred: the embedding_meta identity tuple records `dim` (#613).
    let meta_raw: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embedding_meta'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(raw) = meta_raw {
        let meta: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("corrupt embedding_meta in config table: {e}"))?;
        let dim = meta
            .get("dim")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("embedding_meta is missing a numeric 'dim' field"))?;
        return usize::try_from(dim)
            .map_err(|_| anyhow::anyhow!("embedding_meta 'dim' out of range"));
    }

    // Legacy fallback: a pre-#613 database carried a bare `embed_dim` key.
    let legacy: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE key = 'embed_dim'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(raw) = legacy {
        return raw
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid embed_dim value in config table"));
    }

    anyhow::bail!(
        "database has no embedding identity yet (nothing has been embedded) — \
         add a fact first, or is this a memory-engine database?"
    )
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
    open_engine_writable_with_dim(path, embed_dim)
}

/// Open a `MemoryEngine` with **write** capability using an explicitly known
/// dimension, **without** peeking the database.
///
/// For write commands that know the dimension from their own input (e.g.
/// `add-fact` derives it from the `--embedding` length), so they work against a
/// freshly-created, never-embedded database that has no recorded dimension yet.
/// If the database *was* previously embedded at a different dimension, the
/// engine's open path rejects the mismatch against the stored `embedding_meta`.
pub fn open_engine_writable_with_dim(
    path: &Path,
    embed_dim: usize,
) -> anyhow::Result<MemoryEngine> {
    anyhow::ensure!(
        path.is_file(),
        "database not found (or is a directory): {}",
        path.display()
    );
    let backup_dir = path.parent().map(Path::to_path_buf);
    let mut builder = MemoryEngine::builder(embed_dim).path(path.to_path_buf());
    if let Some(dir) = backup_dir {
        builder = builder.backup_dir(dir);
    }
    let engine = builder.build()?;
    Ok(engine)
}
