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

const MIGRATIONS: &[MigrationFn] = &[migrate_v1_to_v2, migrate_v2_to_v3];

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

    for (i, migration) in MIGRATIONS.iter().enumerate() {
        let target = (i as u32) + 2; // migrations are 1→2, 2→3, etc.
        if version < target {
            let tx = conn.unchecked_transaction()?;
            migration(&tx)?;
            set_config(&tx, "schema_version", &target.to_string())?;
            tx.commit()?;
        }
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

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE facts ADD COLUMN is_pinned INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE facts ADD COLUMN importance_score REAL NOT NULL DEFAULT 0.5;",
    )?;
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

// --- DDL constants ---

const TABLES_DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    session_id TEXT,
    scope_id INTEGER NOT NULL DEFAULT 1,
    origin_node_id TEXT NOT NULL DEFAULT 'local',
    sequence_id INTEGER NOT NULL DEFAULT 0,
    created_at TEXT
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
        // init_schema creates latest version directly
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
        );
        // migrate is a no-op
        migrate(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
        );
    }

    #[test]
    fn migrate_v1_to_v2_runs_without_error() {
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
        // Second call is a no-op
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
    fn migration_v1_to_v2_adds_scope_columns() {
        let conn = open_memory().unwrap();
        init_schema_v1(&conn).unwrap();
        // Insert a fact before migration
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata)
             VALUES ('test', 'hash', X'00', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}')",
            [],
        ).unwrap();

        migrate(&conn).unwrap();

        // Verify scope_id column exists and defaults to 1
        let scope_id: i64 = conn
            .query_row(
                "SELECT scope_id FROM facts WHERE content = 'test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scope_id, 1);

        // Verify scopes table exists with root
        let root_label: String = conn
            .query_row("SELECT label FROM scopes WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(root_label, "root");

        // Verify scope indexes were created by migration
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

    #[test]
    fn migrate_v2_to_v3_adds_pinned_and_envelope() {
        let conn = open_memory().unwrap();
        init_schema_v2(&conn).unwrap();

        // Insert a fact before migration
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, importance, access_count, last_accessed, metadata, scope_id)
             VALUES ('test', 'hash', X'00', 'episodic', datetime('now'), 0.5, 0, datetime('now'), '{}', 1)",
            [],
        ).unwrap();

        // Insert an event before migration
        conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, scope_id)
             VALUES (datetime('now'), 'Interaction', '{}', 'test', 1)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        // Verify new columns with defaults
        let (is_pinned, importance_score): (i64, f64) = conn
            .query_row(
                "SELECT is_pinned, importance_score FROM facts WHERE content = 'test'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(is_pinned, 0);
        assert!((importance_score - 0.5).abs() < f64::EPSILON);

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
            Some("3".to_string())
        );
    }

    #[test]
    fn fresh_db_creates_v3_schema() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(
            get_config(&conn, "schema_version").unwrap(),
            Some("3".to_string())
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
}
