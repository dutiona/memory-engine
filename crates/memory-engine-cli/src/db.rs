use std::path::Path;

use memory_engine::{MemoryEngine, StoreLock, StoreLockError};

/// Peek the embedding dimension from an existing `SQLite` database.
///
/// What the store records, read at its own schema version without migrating it
/// ([`read_recorded_embedding`](memory_engine::read_recorded_embedding)): the identity's `dim`,
/// or the bare `embed_dim` of a store whose facts all wait for their vectors (#1021). Errors if
/// neither is present — a database that was created but never had a fact written has no
/// recorded dimension; write commands that know the dimension from their input (e.g.
/// `add-fact`'s `--embedding`) should use [`open_engine_writable_with_dim`] instead of peeking.
///
/// Distinct from the import path's snapshot-header reader (see
/// `peek_embed_dim_from_snapshot` in `commands::import`).
///
/// Exposed so the `consolidate` command can size the LLM backend's HTTP embedder to
/// the database's embedding dimension. (`db` is a private module, so this is not part
/// of any public API.)
pub fn peek_embed_dim_from_db(path: &Path) -> anyhow::Result<usize> {
    memory_engine::read_recorded_embedding(path)?
        .dim()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "database has no embedding identity yet (nothing has been embedded) — \
             add a fact first, or is this a memory-engine database?"
            )
        })
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

/// A writable engine together with the store lock it holds (ADR-0020 §8).
///
/// Every command that opens the store writable goes through this module, so none can open
/// it beside a running server. The engine drops before the lock.
pub struct Writable {
    engine: MemoryEngine,
    _lock: StoreLock,
}

impl std::ops::Deref for Writable {
    type Target = MemoryEngine;
    fn deref(&self) -> &MemoryEngine {
        &self.engine
    }
}

impl std::ops::DerefMut for Writable {
    fn deref_mut(&mut self) -> &mut MemoryEngine {
        &mut self.engine
    }
}

/// Take the store lock for the database at `path`, or refuse while another process (a
/// running server, another maintenance command) holds it.
///
/// # Errors
///
/// The store is held, or its lock file cannot be opened.
pub fn lock_store(path: &Path) -> anyhow::Result<StoreLock> {
    StoreLock::acquire(path).map_err(|e| match e {
        StoreLockError::Held { .. } => anyhow::anyhow!("{e}: stop the memory-engine server first"),
        StoreLockError::Io { .. } => e.into(),
    })
}

pub fn ensure_store_exists(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.is_file(),
        "database not found (or is a directory): {}",
        path.display()
    );
    Ok(())
}

/// Open a `MemoryEngine` with **write** capability, peeking the dimension from
/// the existing store.
///
/// Used by every command that mutates a previously-embedded database and does not
/// already know its embedding dimension — e.g. `record-outcome`, `consolidate`,
/// `migrate`, and the no-`--embed-dim` branch of
/// `batch-ingest` / `bootstrap`. Commands that derive the dimension from their own
/// input (`add-fact`, and `batch-ingest` / `bootstrap` *with* `--embed-dim`) use
/// [`open_engine_writable_with_dim`] instead. Sets `backup_dir` next to the
/// database so any schema migration creates a WAL-safe backup first. The store lock is
/// taken before the store is read at all.
pub fn open_engine_writable(path: &Path) -> anyhow::Result<Writable> {
    ensure_store_exists(path)?;
    let lock = lock_store(path)?;
    let embed_dim = peek_embed_dim_from_db(path)?;
    build_writable(path, embed_dim, None, lock)
}

/// Open a `MemoryEngine` with **write** capability using an explicitly known
/// dimension, **without** peeking the database.
///
/// For write commands that know the dimension from their own input (e.g.
/// `add-fact` derives it from the `--embedding` length), so they work against a
/// freshly-created, never-embedded database that has no recorded dimension yet.
/// If the database *was* previously embedded at a different dimension, the
/// engine's open path rejects the mismatch against the stored `embedding_meta`.
///
/// A `declared` identity (`add-fact`'s pre-computed vector, #1060) must equal the one the
/// store records, checked read-only before anything migrates (#1065); a blank store accepts
/// any.
pub fn open_engine_writable_with_dim(
    path: &Path,
    embed_dim: usize,
    declared: Option<memory_engine::EmbeddingFingerprint>,
) -> anyhow::Result<Writable> {
    ensure_store_exists(path)?;
    let lock = lock_store(path)?;
    build_writable(path, embed_dim, declared, lock)
}

/// Create a new store at `path` (the `--create` paths of `batch-ingest` and `bootstrap`),
/// holding its lock from before the file exists.
pub fn create_engine(path: &Path, embed_dim: usize) -> anyhow::Result<Writable> {
    let lock = lock_store(path)?;
    ensure_store_absent(path)?;
    let engine = MemoryEngine::builder(embed_dim)
        .path(path.to_path_buf())
        .build()?;
    Ok(Writable {
        engine,
        _lock: lock,
    })
}

/// Refuse `--create` over an existing database. Callers check early, before building their
/// embedder; [`create_engine`] checks again under the lock, which another `--create` may
/// have released after making the file.
pub fn ensure_store_absent(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        !path.exists(),
        "database {} already exists — remove it first or omit --create",
        path.display()
    );
    Ok(())
}

fn build_writable(
    path: &Path,
    embed_dim: usize,
    declared: Option<memory_engine::EmbeddingFingerprint>,
    lock: StoreLock,
) -> anyhow::Result<Writable> {
    let backup_dir = path.parent().map(Path::to_path_buf);
    let mut builder = MemoryEngine::builder(embed_dim).path(path.to_path_buf());
    if let Some(dir) = backup_dir {
        builder = builder.backup_dir(dir);
    }
    if let Some(identity) = declared {
        builder = builder.declared_identity(identity);
    }
    Ok(Writable {
        engine: builder.build()?,
        _lock: lock,
    })
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

    #[tokio::test]
    async fn a_store_whose_facts_all_wait_has_its_dimension() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("waiting.db");
        let mut engine = memory_engine::MemoryEngine::builder(4)
            .path(db_path.clone())
            .build()
            .unwrap();
        engine
            .add_fact_deferred(&memory_engine::AddFactRequest {
                content: "waits for its vector".into(),
                fact_type: memory_engine::FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            })
            .await
            .unwrap();
        engine.close().await.unwrap();

        assert_eq!(peek_embed_dim_from_db(&db_path).unwrap(), 4);
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

    // --- the store lock (ADR-0020 §8) ---

    #[test]
    fn a_writable_open_refuses_while_the_store_is_locked() {
        let dir = TempDir::new().unwrap();
        let existing = dir.path().join("existing.db");
        drop(
            memory_engine::MemoryEngine::builder(4)
                .path(existing.clone())
                .build()
                .unwrap(),
        );
        let fresh = dir.path().join("fresh.db");
        let _a = StoreLock::acquire(&existing).unwrap();
        let _b = StoreLock::acquire(&fresh).unwrap();
        for err in [
            open_engine_writable_with_dim(&existing, 4, None)
                .err()
                .unwrap(),
            create_engine(&fresh, 4).err().unwrap(),
        ] {
            assert!(
                err.to_string()
                    .contains("stop the memory-engine server first"),
                "{err}"
            );
        }
        assert!(!fresh.exists(), "a refused create must create no database");
    }

    #[test]
    fn create_refuses_a_store_that_appeared_before_its_lock() {
        // Another `--create` can finish between a caller's early check and this lock.
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("memory.db");
        drop(
            memory_engine::MemoryEngine::builder(4)
                .path(db.clone())
                .build()
                .unwrap(),
        );
        let err = create_engine(&db, 4).err().unwrap();
        assert!(err.to_string().contains("already exists"), "{err}");
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
