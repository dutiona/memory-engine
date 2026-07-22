use std::path::Path;

use memory_engine::MemoryEngine;

/// Peek the embedding dimension from an existing `SQLite` database.
///
/// Reads the persisted identity's `dim` from the `embedding_spaces` registry's active
/// row (#622, the v13+ home of the identity). Falls back to the legacy `embedding_meta`
/// config row (#613) for an un-migrated v12 database, then to the bare `embed_dim` key for
/// pre-#613 databases. Errors if none is present — a database that was created but never
/// had an embedding written has no recorded dimension; write commands that know the
/// dimension from their input (e.g. `add-fact`'s `--embedding`) should use
/// [`open_engine_writable_with_dim`] instead of peeking.
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

    // Preferred (v13+): the embedding_spaces registry's active row records `dim` (#622).
    // Guard on the table existing so an un-migrated v12 DB (no such table) falls through
    // to the legacy config paths below rather than erroring.
    let has_registry: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='embedding_spaces'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if has_registry {
        let dim: Option<i64> = conn
            .query_row(
                "SELECT dim FROM embedding_spaces WHERE status = 'active'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(dim) = dim {
            return usize::try_from(dim)
                .map_err(|_| anyhow::anyhow!("embedding_spaces 'dim' out of range"));
        }
    }

    // Legacy fallback: the pre-#622 embedding_meta config row records `dim` (#613).
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

/// Open a `MemoryEngine` with **write** capability, peeking the dimension from
/// the existing store.
///
/// Used by every command that mutates a previously-embedded database and does not
/// already know its embedding dimension — e.g. `record-outcome`, `consolidate`,
/// `migrate`, `export` with `SQLite` format, and the no-`--embed-dim` branch of
/// `batch-ingest` / `bootstrap`. Commands that derive the dimension from their own
/// input (`add-fact`, and `batch-ingest` / `bootstrap` *with* `--embed-dim`) use
/// [`open_engine_writable_with_dim`] instead. Sets `backup_dir` next to the
/// database so any schema migration creates a WAL-safe backup first.
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    // --- peek_embed_dim_from_db ---

    #[test]
    fn peek_embed_dim_from_db_nonexistent_path_returns_error() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.db");
        let err = peek_embed_dim_from_db(&missing).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not found") || msg.contains("directory"),
            "expected 'not found' or 'directory' in error, got: {msg}"
        );
    }

    #[test]
    fn peek_embed_dim_from_db_non_sqlite_file_returns_error() {
        // `std::fs::write` closes the file handle before SQLite opens the path,
        // avoiding file-locking conflicts on Windows (per gemini review).
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("garbage.db");
        std::fs::write(&path, b"this is not a sqlite database\n").unwrap();
        // A non-SQLite file opens lazily, then the config-table query fails with
        // the rusqlite "not a database" error — pin that, not just any Err.
        let err = peek_embed_dim_from_db(&path).unwrap_err();
        assert!(
            err.to_string().contains("not a database"),
            "expected 'not a database', got: {err}"
        );
    }

    #[test]
    fn peek_embed_dim_from_db_fresh_engine_no_embedding_returns_error() {
        // A freshly created engine with no facts yet has no embedding_meta.
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("fresh.db");
        let engine = memory_engine::MemoryEngine::builder(4)
            .path(db_path.clone())
            .build()
            .unwrap();
        drop(engine);

        let err = peek_embed_dim_from_db(&db_path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no embedding identity") || msg.contains("embed_dim"),
            "expected 'no embedding identity' or 'embed_dim' in error, got: {msg}"
        );
    }

    // --- peek_schema_version_from_db ---

    #[test]
    fn peek_schema_version_from_db_nonexistent_path_returns_error() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.db");
        let err = peek_schema_version_from_db(&missing).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not found") || msg.contains("directory"),
            "expected 'not found' or 'directory' in error, got: {msg}"
        );
    }

    #[test]
    fn peek_schema_version_from_db_non_sqlite_file_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("garbage.db");
        std::fs::write(&path, b"not a real database\n").unwrap();
        // Wrapped as "… is this a memory-engine database? (… not a database …)".
        let err = peek_schema_version_from_db(&path).unwrap_err();
        assert!(
            err.to_string().contains("not a database"),
            "expected 'not a database', got: {err}"
        );
    }

    #[test]
    fn peek_schema_version_from_db_valid_engine_returns_current_version() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let engine = memory_engine::MemoryEngine::builder(4)
            .path(db_path.clone())
            .build()
            .unwrap();
        drop(engine);

        let version = peek_schema_version_from_db(&db_path).unwrap();
        assert_eq!(version, memory_engine::CURRENT_SCHEMA_VERSION);
    }

    // --- #622: identity in the embedding_spaces registry ---

    #[test]
    fn peek_reads_dim_from_embedding_spaces_registry() {
        // A v13 store records the identity (incl. dim) in the embedding_spaces table,
        // not the config row. The peek must read it from there.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("peek.db");
        // Build creates the v13 schema (empty registry); the temporary engine drops here.
        memory_engine::MemoryEngine::builder(8)
            .path(path.clone())
            .build()
            .unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO embedding_spaces (name, model, provider, dim, status)
             VALUES ('default', 'm', 'tei', 8, 'active')",
            [],
        )
        .unwrap();
        drop(conn);
        assert_eq!(peek_embed_dim_from_db(&path).unwrap(), 8);
    }
}
