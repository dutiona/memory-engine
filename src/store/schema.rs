use rusqlite::Connection;

use crate::error::{MemoryError, Result};

/// Current schema version. Bump when adding migrations.
const CURRENT_SCHEMA_VERSION: u32 = 3;

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
    // Fresh DB: create full latest (v3) schema
    conn.execute_batch(TABLES_DDL)?;
    conn.execute_batch(SCOPES_DDL)?;
    conn.execute_batch(FTS5_DDL)?;
    conn.execute_batch(TRIGGERS_DDL)?;
    conn.execute_batch(INDEXES_DDL)?;
    set_config(conn, "schema_version", &CURRENT_SCHEMA_VERSION.to_string())?;
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
const MIGRATIONS: &[(MigrationFn, bool)] = &[(migrate_v1_to_v2, false), (migrate_v2_to_v3, true)];

/// Run forward-only migrations from the current schema version to
/// `CURRENT_SCHEMA_VERSION`.
///
/// Each migration runs inside a transaction. On failure, the migration rolls
/// back and the version is NOT bumped.
///
/// # Errors
///
/// Returns `MemoryError::Migration` if the stored version is newer than
/// supported, or if any migration step fails.
pub fn migrate(conn: &Connection) -> Result<()> {
    let version_str = get_config(conn, "schema_version")?.unwrap_or_else(|| "1".to_string());
    let version: u32 = version_str
        .parse()
        .map_err(|_| MemoryError::Migration(format!("invalid schema_version: {version_str}")))?;

    if version > CURRENT_SCHEMA_VERSION {
        return Err(MemoryError::Migration(format!(
            "schema_version {version} is newer than supported {CURRENT_SCHEMA_VERSION}"
        )));
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
    Ok(())
}

fn set_foreign_keys(conn: &Connection, enabled: bool) -> Result<()> {
    let sql = if enabled {
        "PRAGMA foreign_keys = ON"
    } else {
        "PRAGMA foreign_keys = OFF"
    };
    let mut stmt = conn.prepare(sql)?;
    let _ = stmt.query([])?;
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
    // Inlined rather than referencing global constants — migrations must be
    // frozen snapshots of the schema at the target version.
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

// --- DDL constants ---

const TABLES_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    session_id TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id)
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
        // 9 original + 2 scopes indexes + 4 scope_id indexes
        assert_eq!(count, 15);
    }

    // --- Migration framework tests ---

    #[test]
    fn fresh_db_creates_latest_schema() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
        );
        // migrate is a no-op on fresh DB
        migrate(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
        );
    }

    #[test]
    fn migrate_v1_through_v3_runs_without_error() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("1".to_string())
        );
        migrate(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
        );
    }

    #[test]
    fn migrate_skips_if_current() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
        );
    }

    #[test]
    fn migrate_rejects_future_version() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        set_config(&conn, "schema_version", "99").unwrap();
        let err = migrate(&conn).unwrap_err();
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

        migrate(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
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

        migrate(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
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

        migrate(&conn).unwrap();

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
        let err = migrate(&conn).unwrap_err();
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
}
