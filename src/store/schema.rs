use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{MemoryError, Result};

/// Current schema version. Bump when adding migrations.
const CURRENT_SCHEMA_VERSION: u32 = 6;

/// Storage epoch — coarse-grained compatibility gate.
///
/// All schema versions within the same epoch are forwards-compatible via
/// the migration chain. Bumping the epoch signals a breaking architectural
/// change (e.g., dropping old migration support). Libraries reject DBs
/// from future epochs with [`MemoryError::UnsupportedEpoch`].
const STORAGE_EPOCH: u16 = 1;

/// Open a `SQLite` connection to a file, with pragmas set.
///
/// # Errors
///
/// Returns `MemoryError::Database` if the connection or pragma setup fails.
pub fn open_connection(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    set_pragmas(&conn)?;
    Ok(conn)
}

/// Open an in-memory `SQLite` connection, with pragmas set.
///
/// # Errors
///
/// Returns `MemoryError::Database` if the connection or pragma setup fails.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    set_pragmas(&conn)?;
    Ok(conn)
}

fn set_pragmas(conn: &Connection) -> Result<()> {
    // All PRAGMA SET statements can return result rows in bundled SQLite.
    // Use prepare+execute to avoid ExecuteReturnedResults from execute/execute_batch.
    for pragma in &[
        "PRAGMA journal_mode = WAL",
        "PRAGMA foreign_keys = ON",
        "PRAGMA busy_timeout = 5000",
        "PRAGMA synchronous = NORMAL",
    ] {
        let mut stmt = conn.prepare(pragma)?;
        // Consume all rows (PRAGMAs return 0 or 1 rows).
        let _ = stmt.query([])?;
    }
    Ok(())
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
/// Returns `MemoryError::Database` if any DDL statement fails.
pub fn init_schema(conn: &Connection) -> Result<()> {
    let is_fresh: bool = conn.query_row(
        "SELECT COUNT(*) = 0 FROM sqlite_master WHERE type='table' AND name='config'",
        [],
        |r| r.get(0),
    )?;
    if !is_fresh {
        return Ok(()); // existing DB — let migrate() handle evolution
    }
    // Fresh DB: create full latest schema
    conn.execute_batch(TABLES_DDL)?;
    conn.execute_batch(SCOPES_DDL)?;
    conn.execute_batch(FTS5_DDL)?;
    conn.execute_batch(TRIGGERS_DDL)?;
    conn.execute_batch(INDEXES_DDL)?;
    set_config(conn, "schema_version", &CURRENT_SCHEMA_VERSION.to_string())?;
    set_config(conn, "storage_epoch", &STORAGE_EPOCH.to_string())?;
    Ok(())
}

/// Read a config value by key.
///
/// # Errors
///
/// Returns `MemoryError::Database` on query failure.
pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM config WHERE key = ?1")?;
    let mut rows = stmt.query_map([key], |row| row.get(0))?;
    match rows.next() {
        Some(Ok(val)) => Ok(Some(val)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// List all config key-value pairs.
///
/// # Errors
///
/// Returns `MemoryError::Database` on query failure.
pub fn list_config(conn: &Connection) -> Result<std::collections::HashMap<String, String>> {
    use std::collections::HashMap;
    let mut stmt = conn.prepare("SELECT key, value FROM config")?;
    let rows = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let value: String = row.get(1)?;
        Ok((key, value))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (key, value) = row?;
        map.insert(key, value);
    }
    Ok(map)
}

/// Write a config value (upsert).
///
/// # Errors
///
/// Returns `MemoryError::Database` on write failure.
pub fn set_config(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

// --- Migration framework ---

type MigrationFn = fn(&Connection) -> Result<()>;

/// `(function, disable_foreign_keys)` — set second element to `true` for
/// table-rebuild migrations that DROP and recreate tables with FK references.
const MIGRATIONS: &[(MigrationFn, bool)] = &[
    (migrate_v1_to_v2, false),
    (migrate_v2_to_v3, true),
    (migrate_v3_to_v4, false),
    (migrate_v4_to_v5, false),
    (migrate_v5_to_v6, false),
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
    let version: u32 = version_str
        .parse()
        .map_err(|_| MemoryError::Migration(format!("invalid schema_version: {version_str}")))?;

    // --- Epoch gate ---
    let epoch_str = get_config(conn, "storage_epoch")?;
    let epoch_raw = epoch_str.as_deref().unwrap_or("1"); // pre-epoch DBs are implicitly epoch 1
    let db_epoch: u16 = epoch_raw
        .parse()
        .map_err(|_| MemoryError::Migration(format!("invalid storage_epoch: {epoch_raw}")))?;
    if db_epoch > STORAGE_EPOCH {
        return Err(MemoryError::UnsupportedEpoch {
            db_epoch,
            supported_epoch: STORAGE_EPOCH,
        });
    }

    if version > CURRENT_SCHEMA_VERSION {
        return Err(MemoryError::Migration(format!(
            "schema_version {version} is newer than supported {CURRENT_SCHEMA_VERSION}; \
             consider upgrading the memory-engine crate"
        )));
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
        let target = (i as u32) + 2; // migrations are 1→2, 2→3, etc.
        if version < target {
            if *disable_fk {
                set_foreign_keys(conn, false)?;
            }
            let result: Result<()> = (|| {
                let tx = conn.unchecked_transaction()?;
                migration(&tx)?;
                if *disable_fk {
                    // Verify FK integrity BEFORE committing. PRAGMA foreign_key_check
                    // works regardless of the foreign_keys setting — it's an explicit
                    // scan, not runtime enforcement. If the rebuilt tables contain
                    // orphan references, we abort here and the transaction rolls back.
                    check_foreign_keys(&tx)?;
                }
                set_config(&tx, "schema_version", &target.to_string())?;
                tx.commit()?;
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

/// Create a WAL-safe backup of the database before running migrations.
///
/// Uses `VACUUM INTO` which produces an atomic, consistent copy regardless
/// of WAL state (no sidecar files to worry about).
///
/// # Errors
///
/// Returns `MemoryError::Migration` if the connection is in-memory or the
/// backup path cannot be written to.
pub fn backup_before_migration(
    conn: &Connection,
    backup_dir: &Path,
    current_version: u32,
) -> Result<PathBuf> {
    // Extract the source database file path via PRAGMA database_list
    let db_path: String = conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .map_err(|e| MemoryError::Migration(format!("cannot read database path: {e}")))?;

    if db_path.is_empty() || db_path == ":memory:" {
        return Err(MemoryError::Migration(
            "cannot backup in-memory database".to_string(),
        ));
    }

    let db_name = Path::new(&db_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let backup_path = backup_dir.join(format!("{db_name}.v{current_version}.bak"));

    // Remove existing backup to avoid VACUUM INTO failure on re-run
    if backup_path.exists() {
        std::fs::remove_file(&backup_path).map_err(|e| {
            MemoryError::Migration(format!(
                "cannot remove existing backup {}: {e}",
                backup_path.display()
            ))
        })?;
    }

    // VACUUM INTO creates a standalone, defragmented copy — WAL-safe.
    // SQLite VACUUM INTO does not support parameterized paths, so we escape
    // single quotes manually (SQLite string literal escaping: ' → '').
    let escaped = backup_path.to_string_lossy().replace('\'', "''");
    let sql = format!("VACUUM INTO '{escaped}'");
    conn.execute_batch(&sql)
        .map_err(|e| MemoryError::Migration(format!("backup failed: {e}")))?;

    Ok(backup_path)
}

fn set_foreign_keys(conn: &Connection, enabled: bool) -> Result<()> {
    let sql = if enabled {
        "PRAGMA foreign_keys = ON"
    } else {
        "PRAGMA foreign_keys = OFF"
    };
    conn.execute_batch(sql)?;
    Ok(())
}

fn check_foreign_keys(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    if rows.next()?.is_some() {
        return Err(MemoryError::Migration(
            "foreign key violations detected after table rebuild".to_string(),
        ));
    }
    Ok(())
}

fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    // Frozen snapshot of SCOPES_DDL at v2 — do not reference the global constant,
    // which may evolve in future schema versions.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scopes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            parent_id INTEGER REFERENCES scopes(id),
            label TEXT NOT NULL,
            depth INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_scopes_parent ON scopes(parent_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_scopes_parent_label
            ON scopes(parent_id, label);
        -- Insert root scope (sentinel). Only root has parent_id=NULL.
        INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (1, NULL, 'root', 0);",
    )?;
    conn.execute_batch(
        "ALTER TABLE facts ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE edges ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE events ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE summaries ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;",
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_facts_scope ON facts(scope_id);
         CREATE INDEX IF NOT EXISTS idx_edges_scope ON edges(scope_id);
         CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope_id);
         CREATE INDEX IF NOT EXISTS idx_summaries_scope ON summaries(scope_id);",
    )?;
    Ok(())
}

/// Rebuild tables to add `REFERENCES scopes(id)` on `scope_id` columns.
///
/// `ALTER TABLE` cannot add FK constraints in SQLite, so this migration
/// recreates each table with the full column definition. Requires
/// `PRAGMA foreign_keys = OFF` (handled by the migration framework).
///
/// Rebuild order respects FK dependencies: events → facts → edges → summaries.
fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    // 1. Drop FTS triggers and virtual table (they reference facts)
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS facts_fts_ai;
         DROP TRIGGER IF EXISTS facts_fts_ad;
         DROP TRIGGER IF EXISTS facts_fts_au;
         DROP TABLE IF EXISTS facts_fts;",
    )?;

    // 2. Rebuild events (no FK deps from other data tables)
    conn.execute_batch(
        "CREATE TABLE events_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            event_type TEXT NOT NULL,
            payload TEXT NOT NULL DEFAULT '{}',
            source TEXT NOT NULL,
            session_id TEXT,
            scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
        );
        INSERT INTO events_new (id, timestamp, event_type, payload, source, session_id, scope_id)
            SELECT id, timestamp, event_type, payload, source, session_id, scope_id FROM events;
        DROP TABLE events;
        ALTER TABLE events_new RENAME TO events;",
    )?;

    // 3. Rebuild facts (source_event_id references events)
    conn.execute_batch(
        "CREATE TABLE facts_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            embedding BLOB NOT NULL,
            fact_type TEXT NOT NULL CHECK(fact_type IN ('episodic', 'semantic', 'procedural')),
            t_created TEXT NOT NULL,
            t_expired TEXT,
            t_valid TEXT,
            t_invalid TEXT,
            source_event_id INTEGER REFERENCES events(id),
            importance REAL NOT NULL DEFAULT 0.5,
            access_count INTEGER NOT NULL DEFAULT 0,
            last_accessed TEXT NOT NULL,
            metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata)),
            scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
        );
        INSERT INTO facts_new (id, content, content_hash, embedding, fact_type, t_created,
            t_expired, t_valid, t_invalid, source_event_id, importance, access_count,
            last_accessed, metadata, scope_id)
            SELECT id, content, content_hash, embedding, fact_type, t_created,
            t_expired, t_valid, t_invalid, source_event_id, importance, access_count,
            last_accessed, metadata, scope_id FROM facts;
        DROP TABLE facts;
        ALTER TABLE facts_new RENAME TO facts;",
    )?;

    // 4. Rebuild edges (source/target_fact_id reference facts)
    conn.execute_batch(
        "CREATE TABLE edges_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_fact_id INTEGER NOT NULL REFERENCES facts(id),
            target_fact_id INTEGER NOT NULL REFERENCES facts(id),
            relation_type TEXT NOT NULL,
            weight REAL NOT NULL DEFAULT 1.0,
            t_created TEXT NOT NULL,
            t_expired TEXT,
            scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
        );
        INSERT INTO edges_new (id, source_fact_id, target_fact_id, relation_type, weight,
            t_created, t_expired, scope_id)
            SELECT id, source_fact_id, target_fact_id, relation_type, weight,
            t_created, t_expired, scope_id FROM edges;
        DROP TABLE edges;
        ALTER TABLE edges_new RENAME TO edges;",
    )?;

    // 5. Rebuild summaries (no FK deps from other data tables)
    conn.execute_batch(
        "CREATE TABLE summaries_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            embedding BLOB NOT NULL,
            level TEXT NOT NULL CHECK(level IN ('local', 'cluster', 'global')),
            source_fact_ids TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
        );
        INSERT INTO summaries_new (id, content, embedding, level, source_fact_ids,
            created_at, scope_id)
            SELECT id, content, embedding, level, source_fact_ids,
            created_at, scope_id FROM summaries;
        DROP TABLE summaries;
        ALTER TABLE summaries_new RENAME TO summaries;",
    )?;

    // 6. Recreate FTS5 and repopulate from rebuilt facts table
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
            content,
            content='facts',
            content_rowid='id',
            tokenize='porter unicode61'
        );
        INSERT INTO facts_fts(rowid, content) SELECT id, content FROM facts;",
    )?;

    // 7. Recreate triggers (inlined for v3 — frozen snapshot)
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS facts_fts_ai AFTER INSERT ON facts BEGIN
            INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS facts_fts_ad AFTER DELETE ON facts BEGIN
            INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS facts_fts_au AFTER UPDATE ON facts BEGIN
            INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
            INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content);
        END;",
    )?;

    // 8. Recreate indexes (inlined for v3 — frozen snapshot)
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id) WHERE session_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
        CREATE INDEX IF NOT EXISTS idx_facts_expired ON facts(t_expired);
        CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type);
        CREATE INDEX IF NOT EXISTS idx_facts_valid ON facts(t_valid, t_invalid);
        CREATE INDEX IF NOT EXISTS idx_facts_hash ON facts(content_hash);
        CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_fact_id);
        CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_fact_id);
        CREATE INDEX IF NOT EXISTS idx_edges_expired ON edges(t_expired);
        CREATE INDEX IF NOT EXISTS idx_facts_scope ON facts(scope_id);
        CREATE INDEX IF NOT EXISTS idx_edges_scope ON edges(scope_id);
        CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope_id);
        CREATE INDEX IF NOT EXISTS idx_summaries_scope ON summaries(scope_id);",
    )?;

    Ok(())
}

/// Add Phase 3b columns: `is_pinned`, `importance_score` on facts,
/// and event envelope fields on events.
fn migrate_v3_to_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE facts ADD COLUMN is_pinned INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE facts ADD COLUMN importance_score REAL NOT NULL DEFAULT 0.5;",
    )?;
    // Backfill: seed importance_score from base importance for existing rows
    conn.execute("UPDATE facts SET importance_score = importance", [])?;
    conn.execute_batch(
        "ALTER TABLE events ADD COLUMN origin_node_id TEXT NOT NULL DEFAULT 'local';
         ALTER TABLE events ADD COLUMN sequence_id INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE events ADD COLUMN created_at TEXT;",
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_facts_pinned ON facts(is_pinned) WHERE is_pinned = 1;
         CREATE INDEX IF NOT EXISTS idx_facts_importance_score ON facts(importance_score);
         CREATE INDEX IF NOT EXISTS idx_facts_t_valid_due ON facts(t_valid) WHERE t_valid IS NOT NULL AND t_expired IS NULL;
         CREATE INDEX IF NOT EXISTS idx_events_origin_seq ON events(origin_node_id, sequence_id);",
    )?;
    Ok(())
}

/// Add `event_revision` column for per-event-type envelope versioning.
fn migrate_v4_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch("ALTER TABLE events ADD COLUMN event_revision INTEGER NOT NULL DEFAULT 1;")?;
    Ok(())
}

/// Add `surfaced_at` column for tracking when due facts are first returned to consumers.
fn migrate_v5_to_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch("ALTER TABLE facts ADD COLUMN surfaced_at TEXT;")?;
    Ok(())
}

// --- DDL constants ---

const TABLES_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    session_id TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id),
    origin_node_id TEXT NOT NULL DEFAULT 'local',
    sequence_id INTEGER NOT NULL DEFAULT 0,
    created_at TEXT,
    event_revision INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,  -- blake3 hex[:16] for dedup
    embedding BLOB NOT NULL,
    fact_type TEXT NOT NULL CHECK(fact_type IN ('episodic', 'semantic', 'procedural')),
    t_created TEXT NOT NULL,
    t_expired TEXT,
    t_valid TEXT,
    t_invalid TEXT,
    source_event_id INTEGER REFERENCES events(id),
    importance REAL NOT NULL DEFAULT 0.5,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata)),
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id),
    is_pinned INTEGER NOT NULL DEFAULT 0,
    importance_score REAL NOT NULL DEFAULT 0.5,
    surfaced_at TEXT
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_fact_id INTEGER NOT NULL REFERENCES facts(id),
    target_fact_id INTEGER NOT NULL REFERENCES facts(id),
    relation_type TEXT NOT NULL,
    weight REAL NOT NULL DEFAULT 1.0,
    t_created TEXT NOT NULL,
    t_expired TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE IF NOT EXISTS summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    embedding BLOB NOT NULL,
    level TEXT NOT NULL CHECK(level IN ('local', 'cluster', 'global')),
    source_fact_ids TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

const SCOPES_DDL: &str = "
CREATE TABLE IF NOT EXISTS scopes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER REFERENCES scopes(id),
    label TEXT NOT NULL,
    depth INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_scopes_parent ON scopes(parent_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_scopes_parent_label
    ON scopes(parent_id, label);

-- Insert root scope (sentinel). Only root has parent_id=NULL.
INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (1, NULL, 'root', 0);
";

const FTS5_DDL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    content,
    content='facts',
    content_rowid='id',
    tokenize='porter unicode61'
);
";

const TRIGGERS_DDL: &str = "
CREATE TRIGGER IF NOT EXISTS facts_fts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content);
END;
";

const INDEXES_DDL: &str = "
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
CREATE INDEX IF NOT EXISTS idx_facts_expired ON facts(t_expired);
CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type);
CREATE INDEX IF NOT EXISTS idx_facts_valid ON facts(t_valid, t_invalid);
CREATE INDEX IF NOT EXISTS idx_facts_hash ON facts(content_hash);
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_expired ON edges(t_expired);
CREATE INDEX IF NOT EXISTS idx_facts_scope ON facts(scope_id);
CREATE INDEX IF NOT EXISTS idx_edges_scope ON edges(scope_id);
CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope_id);
CREATE INDEX IF NOT EXISTS idx_summaries_scope ON summaries(scope_id);
CREATE INDEX IF NOT EXISTS idx_facts_pinned ON facts(is_pinned) WHERE is_pinned = 1;
CREATE INDEX IF NOT EXISTS idx_facts_importance_score ON facts(importance_score);
CREATE INDEX IF NOT EXISTS idx_facts_t_valid_due ON facts(t_valid) WHERE t_valid IS NOT NULL AND t_expired IS NULL;
CREATE INDEX IF NOT EXISTS idx_events_origin_seq ON events(origin_node_id, sequence_id);
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: creates v1 schema (Phase 1 tables only, no scopes, no scope_id).
    fn init_schema_v1(conn: &Connection) -> Result<()> {
        conn.execute_batch(TABLES_V1_DDL)?;
        conn.execute_batch(FTS5_DDL)?;
        conn.execute_batch(TRIGGERS_DDL)?;
        conn.execute_batch(INDEXES_V1_DDL)?;
        set_config(conn, "schema_version", "1")?;
        Ok(())
    }

    /// V1 tables DDL (no scope_id columns, no FK to scopes).
    const TABLES_V1_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    session_id TEXT
);

CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    embedding BLOB NOT NULL,
    fact_type TEXT NOT NULL CHECK(fact_type IN ('episodic', 'semantic', 'procedural')),
    t_created TEXT NOT NULL,
    t_expired TEXT,
    t_valid TEXT,
    t_invalid TEXT,
    source_event_id INTEGER REFERENCES events(id),
    importance REAL NOT NULL DEFAULT 0.5,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata))
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_fact_id INTEGER NOT NULL REFERENCES facts(id),
    target_fact_id INTEGER NOT NULL REFERENCES facts(id),
    relation_type TEXT NOT NULL,
    weight REAL NOT NULL DEFAULT 1.0,
    t_created TEXT NOT NULL,
    t_expired TEXT
);

CREATE TABLE IF NOT EXISTS summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    embedding BLOB NOT NULL,
    level TEXT NOT NULL CHECK(level IN ('local', 'cluster', 'global')),
    source_fact_ids TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

    /// V1 indexes DDL (no scope_id indexes).
    const INDEXES_V1_DDL: &str = "
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
CREATE INDEX IF NOT EXISTS idx_facts_expired ON facts(t_expired);
CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type);
CREATE INDEX IF NOT EXISTS idx_facts_valid ON facts(t_valid, t_invalid);
CREATE INDEX IF NOT EXISTS idx_facts_hash ON facts(content_hash);
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_expired ON edges(t_expired);
";

    /// Test helper: creates v2 schema (v1 + scopes + scope_id columns, no v3 columns).
    fn init_schema_v2(conn: &Connection) -> Result<()> {
        conn.execute_batch(TABLES_V2_DDL)?;
        conn.execute_batch(SCOPES_DDL)?;
        conn.execute_batch(FTS5_DDL)?;
        conn.execute_batch(TRIGGERS_DDL)?;
        conn.execute_batch(INDEXES_V2_DDL)?;
        set_config(conn, "schema_version", "2")?;
        Ok(())
    }

    /// V2 tables DDL (has scope_id columns but no v3 columns).
    const TABLES_V2_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    session_id TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    embedding BLOB NOT NULL,
    fact_type TEXT NOT NULL CHECK(fact_type IN ('episodic', 'semantic', 'procedural')),
    t_created TEXT NOT NULL,
    t_expired TEXT,
    t_valid TEXT,
    t_invalid TEXT,
    source_event_id INTEGER REFERENCES events(id),
    importance REAL NOT NULL DEFAULT 0.5,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata)),
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_fact_id INTEGER NOT NULL REFERENCES facts(id),
    target_fact_id INTEGER NOT NULL REFERENCES facts(id),
    relation_type TEXT NOT NULL,
    weight REAL NOT NULL DEFAULT 1.0,
    t_created TEXT NOT NULL,
    t_expired TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    embedding BLOB NOT NULL,
    level TEXT NOT NULL CHECK(level IN ('local', 'cluster', 'global')),
    source_fact_ids TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    scope_id INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

    /// V2 indexes DDL (has scope_id indexes but no v3 indexes).
    const INDEXES_V2_DDL: &str = "
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
CREATE INDEX IF NOT EXISTS idx_facts_expired ON facts(t_expired);
CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type);
CREATE INDEX IF NOT EXISTS idx_facts_valid ON facts(t_valid, t_invalid);
CREATE INDEX IF NOT EXISTS idx_facts_hash ON facts(content_hash);
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_expired ON edges(t_expired);
CREATE INDEX IF NOT EXISTS idx_facts_scope ON facts(scope_id);
CREATE INDEX IF NOT EXISTS idx_edges_scope ON edges(scope_id);
CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope_id);
CREATE INDEX IF NOT EXISTS idx_summaries_scope ON summaries(scope_id);
";

    // --- Frozen V4 DDL snapshot ---
    // Complete standalone DDL for v4 schema. Depends on NO live DDL constants
    // to prevent fixture drift when tables evolve in future versions.

    /// Test helper: creates v4 schema (v3 + is_pinned, importance_score, event envelope).
    fn init_schema_v4(conn: &Connection) -> Result<()> {
        conn.execute_batch(TABLES_V4_DDL)?;
        conn.execute_batch(SCOPES_DDL_V4)?;
        conn.execute_batch(FTS5_DDL_V4)?;
        conn.execute_batch(TRIGGERS_DDL_V4)?;
        conn.execute_batch(INDEXES_V4_DDL)?;
        set_config(conn, "schema_version", "4")?;
        Ok(())
    }

    const TABLES_V4_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    session_id TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id),
    origin_node_id TEXT NOT NULL DEFAULT 'local',
    sequence_id INTEGER NOT NULL DEFAULT 0,
    created_at TEXT
);

CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    embedding BLOB NOT NULL,
    fact_type TEXT NOT NULL CHECK(fact_type IN ('episodic', 'semantic', 'procedural')),
    t_created TEXT NOT NULL,
    t_expired TEXT,
    t_valid TEXT,
    t_invalid TEXT,
    source_event_id INTEGER REFERENCES events(id),
    importance REAL NOT NULL DEFAULT 0.5,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata)),
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id),
    is_pinned INTEGER NOT NULL DEFAULT 0,
    importance_score REAL NOT NULL DEFAULT 0.5
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_fact_id INTEGER NOT NULL REFERENCES facts(id),
    target_fact_id INTEGER NOT NULL REFERENCES facts(id),
    relation_type TEXT NOT NULL,
    weight REAL NOT NULL DEFAULT 1.0,
    t_created TEXT NOT NULL,
    t_expired TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE IF NOT EXISTS summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    embedding BLOB NOT NULL,
    level TEXT NOT NULL CHECK(level IN ('local', 'cluster', 'global')),
    source_fact_ids TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

    const SCOPES_DDL_V4: &str = "
CREATE TABLE IF NOT EXISTS scopes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER REFERENCES scopes(id),
    label TEXT NOT NULL,
    depth INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_scopes_parent ON scopes(parent_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_scopes_parent_label
    ON scopes(parent_id, label);

INSERT OR IGNORE INTO scopes (id, parent_id, label, depth) VALUES (1, NULL, 'root', 0);
";

    const FTS5_DDL_V4: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    content,
    content='facts',
    content_rowid='id',
    tokenize='porter unicode61'
);
";

    const TRIGGERS_DDL_V4: &str = "
CREATE TRIGGER IF NOT EXISTS facts_fts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content);
END;
";

    const INDEXES_V4_DDL: &str = "
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
CREATE INDEX IF NOT EXISTS idx_facts_expired ON facts(t_expired);
CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type);
CREATE INDEX IF NOT EXISTS idx_facts_valid ON facts(t_valid, t_invalid);
CREATE INDEX IF NOT EXISTS idx_facts_hash ON facts(content_hash);
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_fact_id);
CREATE INDEX IF NOT EXISTS idx_edges_expired ON edges(t_expired);
CREATE INDEX IF NOT EXISTS idx_facts_scope ON facts(scope_id);
CREATE INDEX IF NOT EXISTS idx_edges_scope ON edges(scope_id);
CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope_id);
CREATE INDEX IF NOT EXISTS idx_summaries_scope ON summaries(scope_id);
CREATE INDEX IF NOT EXISTS idx_facts_pinned ON facts(is_pinned) WHERE is_pinned = 1;
CREATE INDEX IF NOT EXISTS idx_facts_importance_score ON facts(importance_score);
CREATE INDEX IF NOT EXISTS idx_facts_t_valid_due ON facts(t_valid) WHERE t_valid IS NOT NULL AND t_expired IS NULL;
CREATE INDEX IF NOT EXISTS idx_events_origin_seq ON events(origin_node_id, sequence_id);
";

    /// Frozen v5 schema for testing v5→v6 migration.
    /// Identical to v4 but with `event_revision` on events table.
    fn init_schema_v5(conn: &Connection) -> Result<()> {
        conn.execute_batch(TABLES_V5_DDL)?;
        conn.execute_batch(SCOPES_DDL_V4)?;
        conn.execute_batch(FTS5_DDL_V4)?;
        conn.execute_batch(TRIGGERS_DDL_V4)?;
        conn.execute_batch(INDEXES_V4_DDL)?;
        set_config(conn, "schema_version", "5")?;
        Ok(())
    }

    const TABLES_V5_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    session_id TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id),
    origin_node_id TEXT NOT NULL DEFAULT 'local',
    sequence_id INTEGER NOT NULL DEFAULT 0,
    created_at TEXT,
    event_revision INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    embedding BLOB NOT NULL,
    fact_type TEXT NOT NULL CHECK(fact_type IN ('episodic', 'semantic', 'procedural')),
    t_created TEXT NOT NULL,
    t_expired TEXT,
    t_valid TEXT,
    t_invalid TEXT,
    source_event_id INTEGER REFERENCES events(id),
    importance REAL NOT NULL DEFAULT 0.5,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata)),
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id),
    is_pinned INTEGER NOT NULL DEFAULT 0,
    importance_score REAL NOT NULL DEFAULT 0.5
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_fact_id INTEGER NOT NULL REFERENCES facts(id),
    target_fact_id INTEGER NOT NULL REFERENCES facts(id),
    relation_type TEXT NOT NULL,
    weight REAL NOT NULL DEFAULT 1.0,
    t_created TEXT NOT NULL,
    t_expired TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE IF NOT EXISTS summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    embedding BLOB NOT NULL,
    level TEXT NOT NULL CHECK(level IN ('local', 'cluster', 'global')),
    source_fact_ids TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

    #[test]
    fn init_schema_creates_all_tables() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(tables.contains(&"events".to_string()));
        assert!(tables.contains(&"facts".to_string()));
        assert!(tables.contains(&"edges".to_string()));
        assert!(tables.contains(&"summaries".to_string()));
        assert!(tables.contains(&"config".to_string()));
        assert!(tables.contains(&"scopes".to_string()));
    }

    #[test]
    fn init_schema_idempotent() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap(); // second call must not error
    }

    #[test]
    fn fts5_trigger_fires_on_insert() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed)
             VALUES ('hello world test', 'abc', X'00', 'episodic', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn config_no_default_embed_dim() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dim = get_config(&conn, "embed_dim").unwrap();
        assert!(dim.is_none());
    }

    #[test]
    fn config_set_and_get() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        set_config(&conn, "test_key", "test_value").unwrap();
        assert_eq!(
            get_config(&conn, "test_key").unwrap(),
            Some("test_value".to_string())
        );
        // Upsert overwrites
        set_config(&conn, "test_key", "new_value").unwrap();
        assert_eq!(
            get_config(&conn, "test_key").unwrap(),
            Some("new_value".to_string())
        );
    }

    #[test]
    fn all_nine_indexes_created() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // 9 original + 2 scopes indexes + 4 scope_id indexes + 4 v3 indexes
        assert_eq!(count, 19);
    }

    // --- Migration framework tests ---

    #[test]
    fn fresh_db_creates_latest_schema() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );
        // migrate is a no-op on fresh DB
        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );
    }

    #[test]
    fn migrate_v1_through_v5_runs_without_error() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("1".to_string())
        );
        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );
    }

    #[test]
    fn migrate_skips_if_current() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();
        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );
    }

    #[test]
    fn migrate_rejects_future_version() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        set_config(&conn, "schema_version", "99").unwrap();
        let err = migrate(&conn, None).unwrap_err();
        assert!(matches!(err, MemoryError::Migration(_)));
    }

    #[test]
    fn migration_v1_through_v3_preserves_data() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        // Insert data before migration
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source)
             VALUES (datetime('now'), 'test', '{}', 'unit-test')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata)
             VALUES ('test fact', 'hash', X'00', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}')",
            [],
        )
        .unwrap();

        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );

        // Data survived both migrations
        let scope_id: i64 = conn
            .query_row(
                "SELECT scope_id FROM facts WHERE content = 'test fact'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scope_id, 1);

        // FTS still works after table rebuild
        let fts_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_count, 1);

        // Scopes table exists with root
        let root_label: String = conn
            .query_row("SELECT label FROM scopes WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(root_label, "root");

        // All scope indexes present
        let scope_indexes: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%_scope'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for expected in &[
            "idx_facts_scope",
            "idx_edges_scope",
            "idx_events_scope",
            "idx_summaries_scope",
        ] {
            assert!(
                scope_indexes.contains(&(*expected).to_string()),
                "missing index {expected} after migration"
            );
        }
    }

    /// Creates a v2-migrated schema: v1 tables + ALTER TABLE scope_id (no FK).
    fn init_schema_v2_migrated(conn: &Connection) -> Result<()> {
        init_schema_v1(conn)?;
        conn.execute_batch(SCOPES_DDL)?;
        conn.execute_batch(
            "ALTER TABLE facts ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE edges ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE events ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE summaries ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;",
        )?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_facts_scope ON facts(scope_id);
             CREATE INDEX IF NOT EXISTS idx_edges_scope ON edges(scope_id);
             CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope_id);
             CREATE INDEX IF NOT EXISTS idx_summaries_scope ON summaries(scope_id);",
        )?;
        set_config(conn, "schema_version", "2")?;
        Ok(())
    }

    #[test]
    fn migrate_v2_to_v3_enforces_scope_fk() {
        let conn = open_memory().unwrap();
        init_schema_v2_migrated(&conn).unwrap();

        // Before migration: orphan scope_id succeeds (no FK constraint)
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'test', '{}', 'test', 999)",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM events WHERE scope_id = 999", [])
            .unwrap();

        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );

        // After migration: orphan scope_id fails (FK enforced)
        let result = conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'test', '{}', 'test', 999)",
            [],
        );
        assert!(
            result.is_err(),
            "expected FK violation for orphan scope_id after v2→v3 migration"
        );
    }

    #[test]
    fn migrate_v2_to_v3_preserves_fts() {
        let conn = open_memory().unwrap();
        init_schema_v2_migrated(&conn).unwrap();
        // Insert a fact with FTS content
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata, scope_id)
             VALUES ('searchable content here', 'h1', X'00', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}', 1)",
            [],
        ).unwrap();

        migrate(&conn, None).unwrap();

        // FTS index was rebuilt and repopulated
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'searchable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // FTS trigger fires for new inserts after migration
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata, scope_id)
             VALUES ('another document', 'h2', X'00', 'semantic', datetime('now'), 0.5, 0, datetime('now'), '{}', 1)",
            [],
        ).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'another'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn migrate_v2_to_v3_rejects_orphan_scope_ids() {
        let conn = open_memory().unwrap();
        init_schema_v2_migrated(&conn).unwrap();
        // Insert a row with an orphan scope_id (no FK enforcement in v2)
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'test', '{}', 'test', 999)",
            [],
        )
        .unwrap();

        // Migration must fail — orphan scope_id violates FK check
        let err = migrate(&conn, None).unwrap_err();
        assert!(
            matches!(err, MemoryError::Migration(_)),
            "expected Migration error for orphan scope_id, got: {err:?}"
        );

        // DB must NOT be stuck at v3 — rollback should leave it at v2
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("2".to_string()),
            "schema_version should remain 2 after failed migration"
        );
    }

    #[test]
    fn migrate_v3_to_v4_adds_pinned_and_envelope() {
        let conn = open_memory().unwrap();
        init_schema_v2(&conn).unwrap();

        // Insert a fact before migration (importance=0.8 to verify backfill)
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata, scope_id)
             VALUES ('test', 'hash', X'00', 'episodic', datetime('now'), 0.8, 0, datetime('now'), '{}', 1)",
            [],
        ).unwrap();

        // Insert an event before migration
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'Interaction', '{}', 'test', 1)",
            [],
        )
        .unwrap();

        migrate(&conn, None).unwrap();

        // Verify new columns with defaults
        let (is_pinned, importance_score): (i64, f64) = conn
            .query_row(
                "SELECT is_pinned, importance_score FROM facts WHERE content = 'test'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(is_pinned, 0);
        // importance_score should be backfilled from importance (0.8), not default (0.5)
        assert!((importance_score - 0.8).abs() < f64::EPSILON);

        // Verify event envelope fields
        let (origin, seq_id): (String, i64) = conn
            .query_row(
                "SELECT origin_node_id, sequence_id FROM events WHERE source = 'test'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, "local");
        assert_eq!(seq_id, 0);

        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );
    }

    #[test]
    fn fresh_db_creates_v6_schema() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed, is_pinned, importance_score)
             VALUES ('test', 'h', X'00', 'episodic', datetime('now'), datetime('now'), 1, 0.9)",
            [],
        ).unwrap();
        let pinned: i64 = conn
            .query_row(
                "SELECT is_pinned FROM facts WHERE content = 'test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pinned, 1);
    }

    #[test]
    fn init_schema_noop_on_existing_db() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("1".to_string())
        );
        // Second init_schema call should be a no-op (config table exists)
        init_schema(&conn).unwrap();
        // Version should still be 1, not overwritten to 2
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("1".to_string())
        );
    }

    // --- Storage epoch tests ---

    #[test]
    fn init_schema_sets_storage_epoch() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "storage_epoch").unwrap(),
            Some("1".to_string())
        );
    }

    #[test]
    fn migrate_rejects_future_epoch() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        set_config(&conn, "storage_epoch", "99").unwrap();
        let err = migrate(&conn, None).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::UnsupportedEpoch {
                    db_epoch: 99,
                    supported_epoch: 1
                }
            ),
            "expected UnsupportedEpoch, got: {err:?}"
        );
    }

    #[test]
    fn migrate_sets_epoch_for_pre_epoch_db() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        // Pre-epoch DB has no storage_epoch config
        assert!(get_config(&conn, "storage_epoch").unwrap().is_none());
        migrate(&conn, None).unwrap();
        // After migration, epoch is stamped
        assert_eq!(
            get_config(&conn, "storage_epoch").unwrap(),
            Some("1".to_string())
        );
    }

    #[test]
    fn migrate_sets_epoch_for_current_version_db() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        // Manually set to current version but no epoch
        set_config(&conn, "schema_version", &CURRENT_SCHEMA_VERSION.to_string()).unwrap();
        assert!(get_config(&conn, "storage_epoch").unwrap().is_none());
        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "storage_epoch").unwrap(),
            Some("1".to_string())
        );
    }

    // --- WAL-safe backup tests ---

    #[test]
    fn backup_returns_error_for_memory_db() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = backup_before_migration(&conn, dir.path(), 4).unwrap_err();
        assert!(
            matches!(err, MemoryError::Migration(ref msg) if msg.contains("in-memory")),
            "expected in-memory error, got: {err:?}"
        );
    }

    #[test]
    fn backup_creates_wal_safe_copy() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();

        // Create a real file-backed DB
        let conn = open_connection(&db_path.to_string_lossy()).unwrap();
        init_schema(&conn).unwrap();
        // Insert some data
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'Interaction', '{}', 'test', 1)",
            [],
        )
        .unwrap();

        let backup_path = backup_before_migration(&conn, &backup_dir, 4).unwrap();
        assert!(backup_path.exists(), "backup file should exist");
        assert!(
            backup_path.to_string_lossy().contains("test.db.v4.bak"),
            "backup should be named test.db.v4.bak, got: {backup_path:?}"
        );

        // Verify backup is a valid SQLite database with data
        let backup_conn = Connection::open(&backup_path).unwrap();
        let count: i64 = backup_conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "backup should contain the event");
    }

    #[test]
    fn backup_nonexistent_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = open_connection(&db_path.to_string_lossy()).unwrap();
        init_schema(&conn).unwrap();

        let bad_dir = dir.path().join("nonexistent");
        let err = backup_before_migration(&conn, &bad_dir, 4).unwrap_err();
        assert!(
            matches!(err, MemoryError::Migration(ref msg) if msg.contains("backup failed")),
            "expected backup failed error, got: {err:?}"
        );
    }

    #[test]
    fn migrate_without_backup_dir_skips_backup() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        // Should work fine with None — no backup attempted on in-memory
        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );
    }

    // --- v4→v5 migration tests ---

    #[test]
    fn migrate_v4_to_v5_adds_event_revision() {
        let conn = open_memory().unwrap();
        init_schema_v4(&conn).unwrap();

        // Insert an event at v4 (no event_revision column yet)
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'Interaction', '{}', 'test', 1)",
            [],
        )
        .unwrap();

        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );

        // Existing events get default revision = 1
        let revision: i64 = conn
            .query_row(
                "SELECT event_revision FROM events WHERE source = 'test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(revision, 1);

        // New events can specify revision
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id, event_revision)
             VALUES (datetime('now'), 'ToolCall', '{}', 'test2', 1, 3)",
            [],
        )
        .unwrap();
        let rev2: i64 = conn
            .query_row(
                "SELECT event_revision FROM events WHERE source = 'test2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev2, 3);
    }

    #[test]
    fn fresh_db_has_event_revision_column() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Insert with explicit event_revision
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id, event_revision)
             VALUES (datetime('now'), 'Interaction', '{}', 'test', 1, 2)",
            [],
        )
        .unwrap();
        let rev: i64 = conn
            .query_row(
                "SELECT event_revision FROM events WHERE source = 'test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, 2);

        // Default is 1
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'ToolCall', '{}', 'test2', 1)",
            [],
        )
        .unwrap();
        let rev_default: i64 = conn
            .query_row(
                "SELECT event_revision FROM events WHERE source = 'test2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev_default, 1);
    }

    #[test]
    fn migrate_v1_through_v5_preserves_events() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();

        // Insert event at v1
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source)
             VALUES (datetime('now'), 'Interaction', '{\"key\":\"val\"}', 'v1-src')",
            [],
        )
        .unwrap();

        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );

        // Event survived all migrations, got default revision
        let (source, revision): (String, i64) = conn
            .query_row(
                "SELECT source, event_revision FROM events WHERE source = 'v1-src'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(source, "v1-src");
        assert_eq!(revision, 1);
    }

    // --- v5→v6 migration tests ---

    #[test]
    fn migrate_v5_to_v6_adds_surfaced_at() {
        let conn = open_memory().unwrap();
        init_schema_v5(&conn).unwrap();

        // Insert a fact at v5 (no surfaced_at column yet)
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed)
             VALUES ('test fact', 'h', X'00', 'episodic', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        migrate(&conn, None).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("6".to_string())
        );

        // Existing facts have surfaced_at = NULL
        let surfaced_at: Option<String> = conn
            .query_row(
                "SELECT surfaced_at FROM facts WHERE content = 'test fact'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            surfaced_at.is_none(),
            "pre-existing facts should have NULL surfaced_at"
        );

        // New facts can have surfaced_at set
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed, surfaced_at)
             VALUES ('new fact', 'h2', X'00', 'episodic', datetime('now'), datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        let surfaced: Option<String> = conn
            .query_row(
                "SELECT surfaced_at FROM facts WHERE content = 'new fact'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(surfaced.is_some(), "new facts can have surfaced_at set");
    }

    #[test]
    fn fresh_db_has_surfaced_at_column() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        // Insert with explicit surfaced_at
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed, surfaced_at)
             VALUES ('test', 'h', X'00', 'episodic', datetime('now'), datetime('now'), '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let ts: Option<String> = conn
            .query_row(
                "SELECT surfaced_at FROM facts WHERE content = 'test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ts, Some("2026-01-01T00:00:00Z".to_string()));

        // Default is NULL
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed)
             VALUES ('test2', 'h2', X'00', 'episodic', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        let ts_default: Option<String> = conn
            .query_row(
                "SELECT surfaced_at FROM facts WHERE content = 'test2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(ts_default.is_none());
    }

    // --- Schema snapshot test (insta) ---

    /// Deterministic projection of the schema: sorted by (type, name), SQL normalized.
    /// This avoids SQLite formatting variance across versions.
    fn deterministic_schema_dump(conn: &Connection) -> String {
        let mut stmt = conn
            .prepare(
                "SELECT type, name, sql FROM sqlite_master
                 WHERE sql IS NOT NULL
                 ORDER BY type, name",
            )
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |row| {
                let obj_type: String = row.get(0)?;
                let name: String = row.get(1)?;
                let sql: String = row.get(2)?;
                // Normalize whitespace for determinism
                let normalized = sql.split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(format!("-- {obj_type}: {name}\n{normalized};"))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        rows.join("\n\n")
    }

    #[test]
    fn schema_v6_snapshot() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let schema = deterministic_schema_dump(&conn);
        insta::assert_snapshot!("schema_v6", schema);
    }

    // --- Property-based migration tests (proptest) ---

    proptest::proptest! {
        #[test]
        fn migration_preserves_event_count(n_events in 1_usize..20) {
            let conn = open_memory().unwrap();
            init_schema_v1(&conn).unwrap();

            for i in 0..n_events {
                conn.execute(
                    "INSERT INTO events (timestamp, event_type, payload, source)
                     VALUES (datetime('now'), 'Interaction', '{}', ?1)",
                    rusqlite::params![format!("src-{i}")],
                ).unwrap();
            }

            let count_before: i64 = conn
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count_before, n_events as i64);

            migrate(&conn, None).unwrap();

            let count_after: i64 = conn
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count_after, n_events as i64);
        }

        #[test]
        fn migration_preserves_fact_content_hashes(n_facts in 1_usize..10) {
            let conn = open_memory().unwrap();
            init_schema_v1(&conn).unwrap();

            let mut expected_hashes = Vec::new();
            for i in 0..n_facts {
                let hash = format!("hash-{i}");
                conn.execute(
                    "INSERT INTO facts (content, content_hash, embedding, fact_type,
                     t_created, importance, access_count, last_accessed, metadata)
                     VALUES (?1, ?2, X'00', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}')",
                    rusqlite::params![format!("fact-{i}"), &hash],
                ).unwrap();
                expected_hashes.push(hash);
            }

            migrate(&conn, None).unwrap();

            let mut stmt = conn
                .prepare("SELECT content_hash FROM facts ORDER BY id")
                .unwrap();
            let actual_hashes: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert_eq!(actual_hashes, expected_hashes);
        }

        #[test]
        fn migration_v1_to_v5_fk_integrity(n_events in 1_usize..5) {
            let conn = open_memory().unwrap();
            init_schema_v1(&conn).unwrap();

            for i in 0..n_events {
                conn.execute(
                    "INSERT INTO events (timestamp, event_type, payload, source)
                     VALUES (datetime('now'), 'Interaction', '{}', ?1)",
                    rusqlite::params![format!("src-{i}")],
                ).unwrap();
            }

            // Insert a fact referencing event 1
            if n_events > 0 {
                conn.execute(
                    "INSERT INTO facts (content, content_hash, embedding, fact_type,
                     t_created, source_event_id, importance, access_count, last_accessed, metadata)
                     VALUES ('test', 'h1', X'00', 'episodic', datetime('now'), 1, 0.5, 0, datetime('now'), '{}')",
                    [],
                ).unwrap();
            }

            migrate(&conn, None).unwrap();

            // PRAGMA foreign_key_check returns rows for violations — empty = clean
            let mut stmt = conn.prepare("PRAGMA foreign_key_check").unwrap();
            let violations: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert!(violations.is_empty(), "FK violations: {violations:?}");
        }
    }
}
