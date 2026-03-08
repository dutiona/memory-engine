use rusqlite::Connection;

use crate::error::Result;

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

/// Initialize the full schema (idempotent via `IF NOT EXISTS`).
///
/// Creates all tables, virtual tables, triggers, and indexes.
/// Writes `schema_version=1` to the config table if not already present.
///
/// # Errors
///
/// Returns `MemoryError::Database` if any DDL statement fails.
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(TABLES_DDL)?;
    conn.execute_batch(FTS5_DDL)?;
    conn.execute_batch(TRIGGERS_DDL)?;
    conn.execute_batch(INDEXES_DDL)?;

    // Write default config only if not already set.
    if get_config(conn, "schema_version")?.is_none() {
        set_config(conn, "schema_version", "1")?;
    }

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

const TABLES_DDL: &str = "
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
";

#[cfg(test)]
mod tests {
    use super::*;

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
    fn config_default_schema_version() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        let version = get_config(&conn, "schema_version").unwrap();
        assert_eq!(version, Some("1".to_string()));
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
        assert_eq!(count, 9);
    }
}
