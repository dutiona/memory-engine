//! Unit and property tests for the schema lifecycle: fresh init, the
//! full v1→CURRENT migration chain, config helpers, WAL-safe backup,
//! storage-epoch gating, read-only validation, and the two `insta` schema
//! snapshots (`schema_v11` + `schema_v11_migration_chain`).
//!
//! Kept in a sibling `tests` module (rather than inline in `mod.rs`) so the
//! module path stays `store::schema::tests` — the snapshot filenames embed it.

use super::ddl::{FTS5_DDL, SCOPES_DDL, TRIGGERS_DDL};
use super::*;

/// The `facts.id` primary key MUST stay `AUTOINCREMENT`. Issue #209's caller-write
/// cursor is a fact-id high-water mark whose soundness depends on ids being
/// monotonic and **never reused** — which `SQLite` guarantees only with the
/// `AUTOINCREMENT` keyword (a plain `INTEGER PRIMARY KEY` reuses the largest rowid
/// after a delete). A future migration that drops it would silently corrupt the
/// cursor (a new fact could be assigned an id below the cursor and never trip a
/// skip), so this guard fails CI loudly instead.
#[test]
fn facts_id_is_autoincrement() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='facts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        ddl.to_uppercase().contains("AUTOINCREMENT"),
        "facts.id must be AUTOINCREMENT (no rowid reuse) — #209 cursor depends on it; DDL was: {ddl}"
    );
}

/// True if an index with the given name exists in `sqlite_master`.
fn index_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = ?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

/// Test helper: creates v1 schema (Phase 1 tables only, no scopes, no `scope_id`).
fn init_schema_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(TABLES_V1_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(FTS5_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(TRIGGERS_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(INDEXES_V1_DDL)
        .map_err(StorageError::backend)?;
    set_config(conn, "schema_version", "1")?;
    Ok(())
}

/// V1 tables DDL (no `scope_id` columns, no FK to scopes).
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

/// V1 indexes DDL (no `scope_id` indexes).
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

/// Test helper: creates v2 schema (v1 + scopes + `scope_id` columns, no v3 columns).
fn init_schema_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch(TABLES_V2_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(SCOPES_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(FTS5_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(TRIGGERS_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(INDEXES_V2_DDL)
        .map_err(StorageError::backend)?;
    set_config(conn, "schema_version", "2")?;
    Ok(())
}

/// V2 tables DDL (has `scope_id` columns but no v3 columns).
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

/// V2 indexes DDL (has `scope_id` indexes but no v3 indexes).
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

/// Test helper: creates v4 schema (v3 + `is_pinned`, `importance_score`, event envelope).
fn init_schema_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(TABLES_V4_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(SCOPES_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(FTS5_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(TRIGGERS_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(INDEXES_V4_DDL)
        .map_err(StorageError::backend)?;
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
    conn.execute_batch(TABLES_V5_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(SCOPES_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(FTS5_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(TRIGGERS_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(INDEXES_V4_DDL)
        .map_err(StorageError::backend)?;
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
fn config_no_default_embedding_meta() {
    // A fresh v12 DB has neither the legacy `embed_dim` key nor an
    // `embedding_meta` tuple — identity is established on the first embedding
    // write (#613, ADR 0015 §2), not at schema init.
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    assert!(get_config(&conn, "embed_dim").unwrap().is_none());
    assert!(
        crate::store::embedding_meta::load(&conn).unwrap().is_none(),
        "fresh DB must have no embedding_meta"
    );
}

#[test]
fn migrate_v11_to_v12_drops_embed_dim() {
    // The migration deletes the legacy `embed_dim` config key. A fresh v12 DB
    // has no such key, so the DELETE would be vacuous — we must INJECT an
    // `embed_dim` row and roll `schema_version` back to 11 to actually exercise
    // the migration step (review MEDIUM-1).
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    set_config(&conn, "embed_dim", "768").unwrap();
    set_config(&conn, "schema_version", "11").unwrap();
    assert_eq!(
        get_config(&conn, "embed_dim").unwrap().as_deref(),
        Some("768")
    );

    migrate(&conn, None).unwrap();

    // Migrating from v11 runs the full chain forward to CURRENT (v11→v12→v13).
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string()),
        "schema_version bumped to CURRENT"
    );
    assert!(
        get_config(&conn, "embed_dim").unwrap().is_none(),
        "legacy embed_dim key dropped"
    );
    assert!(
        crate::store::embedding_meta::load(&conn).unwrap().is_none(),
        "no backfill: identity stays absent until first embed"
    );

    // Idempotent: re-running migrate from CURRENT is a no-op and does not error.
    migrate(&conn, None).unwrap();
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );
}

/// Roll a fresh DB back to a simulated v12 state: drop the v13 registry table +
/// index and reset `schema_version` to 12 (mirrors how
/// `migrate_v11_to_v12_drops_embed_dim` simulates v11). The caller injects the
/// legacy `embedding_meta` config value to exercise the lift.
fn simulate_v12(conn: &Connection) {
    init_schema(conn).unwrap();
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_embedding_spaces_one_active;
             DROP TABLE IF EXISTS embedding_spaces;",
    )
    .unwrap();
    set_config(conn, "schema_version", "12").unwrap();
}

#[test]
fn migrate_v12_to_v13_roundtrips_fingerprint() {
    // A v12 DB with a stamped `embedding_meta` identity migrates to exactly one
    // `default`/`active` registry row carrying the same tuple, and the legacy key
    // is dropped. Uses an MRL fingerprint so Some(base_dim) + a non-default
    // element_type are both exercised.
    let conn = open_memory().unwrap();
    simulate_v12(&conn);
    let fp = crate::types::EmbeddingFingerprint::with_matryoshka(
        "Qwen/Qwen3-Embedding-0.6B",
        "tei",
        1024,
        2048,
    );
    set_config(
        &conn,
        "embedding_meta",
        &serde_json::to_string(&fp).unwrap(),
    )
    .unwrap();

    migrate(&conn, None).unwrap();

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string()),
        "schema_version bumped to CURRENT (migrate chains v12→v13→v14)"
    );
    // Exactly one active row, columns identical to the stamped tuple.
    let (name, model, provider, dim, base, elem, status): (
        String,
        String,
        String,
        i64,
        Option<i64>,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT name, model, provider, dim, matryoshka_base_dim, element_type, status
                 FROM embedding_spaces WHERE status = 'active'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(name, "default");
    assert_eq!(model, fp.model);
    assert_eq!(provider, fp.provider);
    assert_eq!(dim, i64::try_from(fp.dim).unwrap());
    assert_eq!(
        base,
        fp.matryoshka_base_dim.map(|d| i64::try_from(d).unwrap())
    );
    assert_eq!(elem, fp.element_type);
    assert_eq!(status, "active");
    assert!(
        get_config(&conn, "embedding_meta").unwrap().is_none(),
        "legacy embedding_meta config key dropped"
    );

    // Idempotent: re-run is a no-op, still one row, still v13.
    migrate(&conn, None).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM embedding_spaces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn migrate_v12_to_v13_fresh_store_inserts_nothing() {
    // A v12 DB that never embedded has no `embedding_meta` value: the registry is
    // created empty and identity is established lazily on first write (#613).
    let conn = open_memory().unwrap();
    simulate_v12(&conn);
    assert!(get_config(&conn, "embedding_meta").unwrap().is_none());

    migrate(&conn, None).unwrap();

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM embedding_spaces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "no fabricated row on a never-embedded store");
}

#[test]
fn migrate_v12_to_v13_rejects_corrupt_legacy_value() {
    // A present-but-corrupt `embedding_meta` value fails the migration (rolls the
    // step back) rather than silently discarding a malformed identity.
    let conn = open_memory().unwrap();
    simulate_v12(&conn);
    set_config(&conn, "embedding_meta", "{not json").unwrap();

    let err = migrate(&conn, None).expect_err("corrupt value must fail the migration");
    assert!(
        matches!(err, MemoryError::Migration(_)),
        "expected a migration error, got {err:?}"
    );
    assert_eq!(
        get_config(&conn, "schema_version").unwrap().as_deref(),
        Some("12"),
        "version stays 12 — the step rolled back"
    );
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
fn list_config_returns_all_set_pairs() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();

    // Two keys with deliberately asymmetric values so a key/value tuple
    // swap (k1 paired with v2, etc.) would be caught by the assertions.
    set_config(&conn, "alpha_key", "value_for_alpha").unwrap();
    set_config(&conn, "beta_key", "value_for_beta").unwrap();

    let map = list_config(&conn).unwrap();

    // Both inserted pairs must be present and correctly paired.
    assert_eq!(
        map.get("alpha_key").map(String::as_str),
        Some("value_for_alpha"),
        "alpha_key must round-trip through list_config with its own value"
    );
    assert_eq!(
        map.get("beta_key").map(String::as_str),
        Some("value_for_beta"),
        "beta_key must round-trip through list_config with its own value"
    );

    // list_config reflects writes through set_config (upsert), not stale rows.
    set_config(&conn, "alpha_key", "updated_alpha").unwrap();
    let map = list_config(&conn).unwrap();
    assert_eq!(
        map.get("alpha_key").map(String::as_str),
        Some("updated_alpha"),
        "list_config must reflect the upserted value, not the original"
    );

    // init_schema seeds BOTH tooling-inspection keys named by #481:
    // schema_version AND storage_epoch. list_config is exactly how tooling
    // reads them, so it must surface each with its seeded value (asserting
    // the value, not mere key presence, keeps these non-vacuous: a wrong
    // seed, a +1 drift, or a key/value swap fails here).
    assert_eq!(
        map.get("schema_version").map(String::as_str),
        Some(CURRENT_SCHEMA_VERSION.to_string().as_str()),
        "list_config must surface the seeded schema_version with its value"
    );
    assert_eq!(
        map.get("storage_epoch").map(String::as_str),
        Some(STORAGE_EPOCH.to_string().as_str()),
        "list_config must surface the seeded storage_epoch with its value"
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
    // 9 original + 2 scopes indexes + 4 scope_id indexes + 4 v3 indexes + 1 archive_manifest + 1 lineage + 5 activities/checkpoints + 1 t_created (v11) + 1 embedding_spaces one-active (v13) + 1 fact_vectors space (v14)
    assert_eq!(count, 29);
}

// --- Migration framework tests ---

#[test]
fn fresh_db_creates_latest_schema() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );
    // migrate is a no-op on fresh DB
    migrate(&conn, None).unwrap();
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );
}

#[test]
fn migrate_v10_to_v11_adds_t_created_index() {
    // A fresh DB is created at CURRENT_SCHEMA_VERSION with the index already present. To
    // exercise the v10→v11 migration specifically, simulate a v10 DB by dropping the index
    // and rolling the recorded version back, then migrate forward.
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    conn.execute_batch("DROP INDEX IF EXISTS idx_facts_created;")
        .unwrap();
    set_config(&conn, "schema_version", "10").unwrap();

    // Precondition: index absent at v10.
    assert!(
        !index_exists(&conn, "idx_facts_created"),
        "idx_facts_created should be absent before the v10→v11 migration"
    );

    migrate(&conn, None).unwrap();

    // Postcondition: migrated to current (v11), index present.
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );
    assert!(
        index_exists(&conn, "idx_facts_created"),
        "idx_facts_created should exist after the v10→v11 migration"
    );
}

#[test]
fn fresh_db_has_t_created_index() {
    // The fresh-init path (INDEXES_DDL) must include the index too, so fresh and
    // migrated databases converge on the same index set.
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    assert!(
        index_exists(&conn, "idx_facts_created"),
        "a fresh DB should include idx_facts_created"
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%_scope'")
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

/// Creates a v2-migrated schema: v1 tables + ALTER TABLE `scope_id` (no FK).
fn init_schema_v2_migrated(conn: &Connection) -> Result<()> {
    init_schema_v1(conn)?;
    conn.execute_batch(SCOPES_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch(
        "ALTER TABLE facts ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE edges ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE events ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE summaries ADD COLUMN scope_id INTEGER NOT NULL DEFAULT 1;",
    )
    .map_err(StorageError::backend)?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_facts_scope ON facts(scope_id);
             CREATE INDEX IF NOT EXISTS idx_edges_scope ON edges(scope_id);
             CREATE INDEX IF NOT EXISTS idx_events_scope ON events(scope_id);
             CREATE INDEX IF NOT EXISTS idx_summaries_scope ON summaries(scope_id);",
    )
    .map_err(StorageError::backend)?;
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );
}

#[test]
fn fresh_db_creates_v6_schema() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        matches!(err, MemoryError::Migration(MigrationError::Backup(ref msg)) if msg.contains("in-memory")),
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

#[cfg(unix)]
#[test]
fn backup_rejects_null_byte_in_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let conn = open_connection(&db_path.to_string_lossy()).unwrap();
    init_schema(&conn).unwrap();

    // A backup_dir whose byte form embeds a NUL. The resulting backup path
    // string carries the NUL, which must be rejected before it reaches the
    // VACUUM INTO SQL string (where it would truncate the C path).
    let mut bytes = dir.path().as_os_str().as_bytes().to_vec();
    bytes.extend_from_slice(b"/sub\0dir");
    let evil_dir = PathBuf::from(OsStr::from_bytes(&bytes));

    let err = backup_before_migration(&conn, &evil_dir, 4).unwrap_err();
    assert!(
        matches!(err, MemoryError::Migration(MigrationError::Backup(ref msg)) if msg.contains("null byte")),
        "expected null-byte rejection, got: {err:?}"
    );
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
        matches!(err, MemoryError::Migration(MigrationError::Backup(ref msg)) if msg.contains("backup failed")),
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
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
        Some(CURRENT_SCHEMA_VERSION.to_string())
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

// --- Schema snapshot tests ---

/// Deterministic projection of the schema: sorted by (type, name), SQL normalized.
/// This avoids `SQLite` formatting variance across versions.
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
fn schema_v11_snapshot() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    let schema = deterministic_schema_dump(&conn);
    insta::assert_snapshot!("schema_v11", schema);
}

/// Equivalence oracle: verifies that the full schema DDL (tables, indexes,
/// triggers, virtual tables) produced by `init_schema` at `CURRENT_SCHEMA_VERSION`
/// is byte-identical to a pinned golden string.
///
/// **Scope**: This test only exercises the fresh-DB init path (DDL constants
/// in `mod.rs`). It does NOT exercise any `migrate_v*` function in
/// `migrations.rs`. For a migration-chain oracle see
/// `schema_ddl_migration_chain_snapshot` and
/// `migration_chain_ddl_differs_from_init_known_artifact`.
#[test]
#[allow(clippy::too_many_lines)] // the body is one pinned golden DDL string literal
fn schema_ddl_snapshot_is_stable() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    let actual = deterministic_schema_dump(&conn);

    // Golden captured from the unrefactored schema at v11.
    // DO NOT edit this string unless CURRENT_SCHEMA_VERSION is bumped and
    // a new schema version is intentionally introduced.
    let expected = "\
-- index: idx_activities_dedup
CREATE INDEX idx_activities_dedup ON activities(session_id, tool_name, args_hash, outcome_class, scope_id);

-- index: idx_activities_scope_recent
CREATE INDEX idx_activities_scope_recent ON activities(scope_id, last_seen DESC);

-- index: idx_activities_session
CREATE INDEX idx_activities_session ON activities(session_id);

-- index: idx_activities_status
CREATE INDEX idx_activities_status ON activities(status);

-- index: idx_archive_manifest_path
CREATE UNIQUE INDEX idx_archive_manifest_path ON archive_manifest(pak_path);

-- index: idx_checkpoints_scope
CREATE INDEX idx_checkpoints_scope ON session_checkpoints(scope_path);

-- index: idx_edges_expired
CREATE INDEX idx_edges_expired ON edges(t_expired);

-- index: idx_edges_scope
CREATE INDEX idx_edges_scope ON edges(scope_id);

-- index: idx_edges_source
CREATE INDEX idx_edges_source ON edges(source_fact_id);

-- index: idx_edges_target
CREATE INDEX idx_edges_target ON edges(target_fact_id);

-- index: idx_embedding_spaces_one_active
CREATE UNIQUE INDEX idx_embedding_spaces_one_active ON embedding_spaces(status) WHERE status = 'active';

-- index: idx_events_origin_seq
CREATE INDEX idx_events_origin_seq ON events(origin_node_id, sequence_id);

-- index: idx_events_scope
CREATE INDEX idx_events_scope ON events(scope_id);

-- index: idx_events_session
CREATE INDEX idx_events_session ON events(session_id) WHERE session_id IS NOT NULL;

-- index: idx_events_timestamp
CREATE INDEX idx_events_timestamp ON events(timestamp);

-- index: idx_fact_vectors_space
CREATE INDEX idx_fact_vectors_space ON fact_vectors(space_id);

-- index: idx_facts_created
CREATE INDEX idx_facts_created ON facts(t_created);

-- index: idx_facts_expired
CREATE INDEX idx_facts_expired ON facts(t_expired);

-- index: idx_facts_hash
CREATE INDEX idx_facts_hash ON facts(content_hash);

-- index: idx_facts_importance_score
CREATE INDEX idx_facts_importance_score ON facts(importance_score);

-- index: idx_facts_pinned
CREATE INDEX idx_facts_pinned ON facts(is_pinned) WHERE is_pinned = 1;

-- index: idx_facts_scope
CREATE INDEX idx_facts_scope ON facts(scope_id);

-- index: idx_facts_t_valid_due
CREATE INDEX idx_facts_t_valid_due ON facts(t_valid) WHERE t_valid IS NOT NULL AND t_expired IS NULL;

-- index: idx_facts_type
CREATE INDEX idx_facts_type ON facts(fact_type);

-- index: idx_facts_valid
CREATE INDEX idx_facts_valid ON facts(t_valid, t_invalid);

-- index: idx_lineage_wisdom_fact_id
CREATE UNIQUE INDEX idx_lineage_wisdom_fact_id ON lineage(wisdom_fact_id);

-- index: idx_scopes_parent
CREATE INDEX idx_scopes_parent ON scopes(parent_id);

-- index: idx_scopes_parent_label
CREATE UNIQUE INDEX idx_scopes_parent_label ON scopes(parent_id, label);

-- index: idx_summaries_scope
CREATE INDEX idx_summaries_scope ON summaries(scope_id);

-- table: activities
CREATE TABLE activities ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, tool_name TEXT NOT NULL, args_hash TEXT NOT NULL, args TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(args)), result_summary TEXT, outcome_class TEXT NOT NULL DEFAULT 'success', status TEXT NOT NULL DEFAULT 'recorded' CHECK(status IN ('recorded', 'deduplicated', 'ignored', 'promoted')), occurrence_count INTEGER NOT NULL DEFAULT 1, first_seen TEXT NOT NULL, last_seen TEXT NOT NULL, scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id), promoted_fact_id INTEGER REFERENCES facts(id) );

-- table: archive_manifest
CREATE TABLE archive_manifest ( id INTEGER PRIMARY KEY AUTOINCREMENT, pak_path TEXT NOT NULL, created_at TEXT NOT NULL, fact_count INTEGER NOT NULL, edge_count INTEGER NOT NULL, fact_id_min INTEGER NOT NULL, fact_id_max INTEGER NOT NULL, t_created_min TEXT NOT NULL, t_created_max TEXT NOT NULL, size_bytes INTEGER NOT NULL, blake3_hash TEXT NOT NULL );

-- table: config
CREATE TABLE config ( key TEXT PRIMARY KEY, value TEXT NOT NULL );

-- table: edges
CREATE TABLE edges ( id INTEGER PRIMARY KEY AUTOINCREMENT, source_fact_id INTEGER NOT NULL REFERENCES facts(id), target_fact_id INTEGER NOT NULL REFERENCES facts(id), relation_type TEXT NOT NULL, weight REAL NOT NULL DEFAULT 1.0, t_created TEXT NOT NULL, t_expired TEXT, scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id) );

-- table: embedding_spaces
CREATE TABLE embedding_spaces ( name TEXT PRIMARY KEY, model TEXT NOT NULL, provider TEXT NOT NULL, dim INTEGER NOT NULL, matryoshka_base_dim INTEGER, element_type TEXT NOT NULL DEFAULT 'float32', status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active', 'populating', 'deprecated')), created_at TEXT NOT NULL DEFAULT (datetime('now')) );

-- table: events
CREATE TABLE events ( id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp TEXT NOT NULL, event_type TEXT NOT NULL, payload TEXT NOT NULL DEFAULT '{}', source TEXT NOT NULL, session_id TEXT, scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id), origin_node_id TEXT NOT NULL DEFAULT 'local', sequence_id INTEGER NOT NULL DEFAULT 0, created_at TEXT, event_revision INTEGER NOT NULL DEFAULT 1 );

-- table: fact_vectors
CREATE TABLE fact_vectors ( fact_id INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE, space_id TEXT NOT NULL REFERENCES embedding_spaces(name) ON DELETE CASCADE, embedding BLOB NOT NULL, PRIMARY KEY (fact_id, space_id) ) WITHOUT ROWID;

-- table: facts
CREATE TABLE facts ( id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, content_hash TEXT NOT NULL, -- blake3 hex[:16] for dedup embedding BLOB NOT NULL, fact_type TEXT NOT NULL CHECK(fact_type IN ('episodic', 'semantic', 'procedural')), t_created TEXT NOT NULL, t_expired TEXT, t_valid TEXT, t_invalid TEXT, source_event_id INTEGER REFERENCES events(id), importance REAL NOT NULL DEFAULT 0.5, access_count INTEGER NOT NULL DEFAULT 0, last_accessed TEXT NOT NULL, metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata)), scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id), is_pinned INTEGER NOT NULL DEFAULT 0, importance_score REAL NOT NULL DEFAULT 0.5, surfaced_at TEXT );

-- table: facts_fts
CREATE VIRTUAL TABLE facts_fts USING fts5( content, content='facts', content_rowid='id', tokenize='porter unicode61' );

-- table: facts_fts_config
CREATE TABLE 'facts_fts_config'(k PRIMARY KEY, v) WITHOUT ROWID;

-- table: facts_fts_data
CREATE TABLE 'facts_fts_data'(id INTEGER PRIMARY KEY, block BLOB);

-- table: facts_fts_docsize
CREATE TABLE 'facts_fts_docsize'(id INTEGER PRIMARY KEY, sz BLOB);

-- table: facts_fts_idx
CREATE TABLE 'facts_fts_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;

-- table: lineage
CREATE TABLE lineage ( lineage_id INTEGER PRIMARY KEY AUTOINCREMENT, wisdom_fact_id INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE, source_fact_ids TEXT NOT NULL CHECK(json_valid(source_fact_ids)), provenance TEXT NOT NULL CHECK(json_valid(provenance)) );

-- table: scopes
CREATE TABLE scopes ( id INTEGER PRIMARY KEY AUTOINCREMENT, parent_id INTEGER REFERENCES scopes(id), label TEXT NOT NULL, depth INTEGER NOT NULL DEFAULT 0 );

-- table: session_checkpoints
CREATE TABLE session_checkpoints ( session_id TEXT PRIMARY KEY, scope_path TEXT, summary TEXT, last_activity_id INTEGER REFERENCES activities(id) ON DELETE SET NULL, checkpoint_at TEXT NOT NULL, metadata TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata)) );

-- table: sqlite_sequence
CREATE TABLE sqlite_sequence(name,seq);

-- table: summaries
CREATE TABLE summaries ( id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, embedding BLOB NOT NULL, level TEXT NOT NULL CHECK(level IN ('local', 'cluster', 'global')), source_fact_ids TEXT NOT NULL DEFAULT '[]', created_at TEXT NOT NULL, scope_id INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id) );

-- trigger: facts_fts_ad
CREATE TRIGGER facts_fts_ad AFTER DELETE ON facts BEGIN INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content); END;

-- trigger: facts_fts_ai
CREATE TRIGGER facts_fts_ai AFTER INSERT ON facts BEGIN INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content); END;

-- trigger: facts_fts_au
CREATE TRIGGER facts_fts_au AFTER UPDATE ON facts BEGIN INSERT INTO facts_fts(facts_fts, rowid, content) VALUES ('delete', old.id, old.content); INSERT INTO facts_fts(rowid, content) VALUES (new.id, new.content); END;";

    assert_eq!(
        actual, expected,
        "schema DDL changed — if this is intentional, bump CURRENT_SCHEMA_VERSION and update this golden"
    );
}

/// Migration-chain equivalence oracle: verifies that the full schema DDL
/// produced by running the COMPLETE v1→`CURRENT_SCHEMA_VERSION` migration
/// chain through the real [`migrate`] dispatcher is stable against a pinned
/// golden snapshot.
///
/// This is the primary oracle for [`migrations::migrate_v*`] correctness:
/// a defect in any moved migration function changes the DDL and breaks this
/// test. Unlike `schema_ddl_snapshot_is_stable` (which only exercises
/// `init_schema`), this test directly exercises every `migrate_v*` function
/// registered in `MIGRATIONS`.
///
/// **Note on init-vs-migrated divergence**: The golden here intentionally
/// differs from the `schema_ddl_snapshot_is_stable` golden. `SQLite`'s
/// `ALTER TABLE ADD COLUMN` does not rewrite the `sql` column in
/// `sqlite_master`, so tables rebuilt at v3 and extended at v3→v4 and
/// v5→v6 show only their v3-era column set in `CREATE TABLE` DDL. This is a
/// known structural artifact of the migration path, not a bug; see
/// `migration_chain_ddl_differs_from_init_known_artifact` for a pinned
/// assertion of this difference.
#[test]
fn schema_ddl_migration_chain_snapshot() {
    let conn = open_memory().unwrap();
    // Start from v1 — the oldest supported schema.
    init_schema_v1(&conn).unwrap();
    migrate(&conn, None).unwrap();
    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string()),
        "migration chain must reach CURRENT_SCHEMA_VERSION"
    );
    let actual = deterministic_schema_dump(&conn);
    insta::assert_snapshot!("schema_v11_migration_chain", actual);
}

/// Documents and asserts the known structural difference between the
/// fresh-init DDL and the migration-chain DDL.
///
/// Because `ALTER TABLE ADD COLUMN` (used in migrations v3→v4, v4→v5,
/// v5→v6) does not modify the `sql` column in `sqlite_master`, a v1→v11
/// migrated database produces different DDL text than a direct `init_schema`
/// call, even though both databases are functionally equivalent (same
/// columns, same constraints, same indexes).
///
/// This test pins the fact that the two paths DIFFER, preventing a false
/// future "equality" assertion that would hide a real migration regression.
/// It fails if the known divergence unexpectedly disappears (which could
/// indicate either a fix or a masked bug).
#[test]
fn migration_chain_ddl_differs_from_init_known_artifact() {
    let init_conn = open_memory().unwrap();
    init_schema(&init_conn).unwrap();
    let init_ddl = deterministic_schema_dump(&init_conn);

    let migrated_conn = open_memory().unwrap();
    init_schema_v1(&migrated_conn).unwrap();
    migrate(&migrated_conn, None).unwrap();
    let migrated_ddl = deterministic_schema_dump(&migrated_conn);

    // The two DDL strings must differ: ALTER TABLE ADD COLUMN leaves behind
    // truncated CREATE TABLE SQL for tables that were rebuilt at v3 and later
    // extended. This is intentional: we pin the divergence here so that any
    // unexpected convergence (or unexpected further divergence) is caught.
    assert_ne!(
        init_ddl, migrated_ddl,
        "init_schema and migration-chain DDL unexpectedly converged; \
             if this is intentional (e.g. all migrations now use full table rebuilds), \
             update this test and promote schema_ddl_migration_chain_snapshot as the shared golden"
    );
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
        assert_eq!(count_before, i64::try_from(n_events).unwrap());

        migrate(&conn, None).unwrap();

        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after, i64::try_from(n_events).unwrap());
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

#[test]
fn validate_schema_version_current_ok() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    migrate(&conn, None).unwrap();
    validate_schema_version(&conn).unwrap();
}

#[test]
fn validate_schema_version_future_version_err() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    migrate(&conn, None).unwrap();
    set_config(&conn, "schema_version", "999").unwrap();
    let err = validate_schema_version(&conn).unwrap_err();
    assert!(matches!(err, MemoryError::Migration(_)));
}

#[test]
fn validate_schema_version_future_epoch_err() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    migrate(&conn, None).unwrap();
    set_config(&conn, "storage_epoch", "999").unwrap();
    let err = validate_schema_version(&conn).unwrap_err();
    assert!(matches!(err, MemoryError::UnsupportedEpoch { .. }));
}

#[test]
fn validate_schema_version_fresh_db_err() {
    let conn = open_memory().unwrap();
    let err = validate_schema_version(&conn).unwrap_err();
    assert!(matches!(err, MemoryError::Migration(_)));
}

#[test]
fn validate_schema_version_old_version_err() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    migrate(&conn, None).unwrap();
    let old = (CURRENT_SCHEMA_VERSION - 1).to_string();
    set_config(&conn, "schema_version", &old).unwrap();
    let err = validate_schema_version(&conn).unwrap_err();
    assert!(matches!(err, MemoryError::Migration(_)));
}

// --- v6→v7 migration tests ---

/// Frozen v6 schema: v5 tables + `surfaced_at` on facts, no `archive_manifest`.
fn init_schema_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch(TABLES_V5_DDL)
        .map_err(StorageError::backend)?;
    conn.execute_batch("ALTER TABLE facts ADD COLUMN surfaced_at TEXT;")
        .map_err(StorageError::backend)?;
    conn.execute_batch(SCOPES_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(FTS5_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(TRIGGERS_DDL_V4)
        .map_err(StorageError::backend)?;
    conn.execute_batch(INDEXES_V4_DDL)
        .map_err(StorageError::backend)?;
    set_config(conn, "schema_version", "6")?;
    Ok(())
}

#[test]
fn migrate_v6_to_v7_adds_archive_manifest() {
    let conn = open_memory().unwrap();
    init_schema_v6(&conn).unwrap();

    // archive_manifest must NOT exist before migration
    let count_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='archive_manifest'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count_before, 0,
        "archive_manifest should not exist before migration"
    );

    migrate(&conn, None).unwrap();

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );

    // archive_manifest must exist after migration
    let count_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='archive_manifest'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count_after, 1,
        "archive_manifest should exist after migration"
    );

    // Unique index on pak_path must exist
    let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_archive_manifest_path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(
        index_count, 1,
        "idx_archive_manifest_path should exist after migration"
    );
}

#[test]
fn fresh_db_has_archive_manifest_table() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='archive_manifest'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "archive_manifest should exist in fresh schema");

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );
}

/// Create a v7 schema by initing v8 and removing lineage artifacts.
fn init_schema_v7(conn: &Connection) -> Result<()> {
    init_schema(conn)?;
    conn.execute_batch("DROP TABLE IF EXISTS lineage;")
        .map_err(StorageError::backend)?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_lineage_wisdom_fact_id;")
        .map_err(StorageError::backend)?;
    set_config(conn, "schema_version", "7")?;
    Ok(())
}

#[test]
fn migrate_v7_to_v8_adds_lineage_table() {
    let conn = open_memory().unwrap();
    init_schema_v7(&conn).unwrap();

    // lineage must NOT exist before migration
    let count_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='lineage'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count_before, 0, "lineage should not exist before migration");

    migrate(&conn, None).unwrap();

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );

    // lineage table must exist after migration
    let count_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='lineage'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count_after, 1, "lineage should exist after migration");

    // Verify expected columns
    let col_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('lineage')
                 WHERE name IN ('lineage_id', 'wisdom_fact_id', 'source_fact_ids', 'provenance')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(col_count, 4, "lineage table should have 4 expected columns");

    // Unique index on wisdom_fact_id must exist
    let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_lineage_wisdom_fact_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(idx_count, 1, "unique index on wisdom_fact_id should exist");
}

#[test]
fn fresh_db_has_lineage_table() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('lineage')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0, "fresh DB should have lineage table");
}

/// Create a v8 schema by initing v9 and removing activity/checkpoint artifacts.
fn init_schema_v8(conn: &Connection) -> Result<()> {
    init_schema(conn)?;
    conn.execute_batch("DROP TABLE IF EXISTS session_checkpoints;")
        .map_err(StorageError::backend)?;
    conn.execute_batch("DROP TABLE IF EXISTS activities;")
        .map_err(StorageError::backend)?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_activities_session;")
        .map_err(StorageError::backend)?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_activities_dedup;")
        .map_err(StorageError::backend)?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_activities_scope_recent;")
        .map_err(StorageError::backend)?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_activities_status;")
        .map_err(StorageError::backend)?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_checkpoints_scope;")
        .map_err(StorageError::backend)?;
    set_config(conn, "schema_version", "8")?;
    Ok(())
}

#[test]
fn migrate_v8_to_v9_adds_activities_and_checkpoints() {
    let conn = open_memory().unwrap();
    init_schema_v8(&conn).unwrap();

    // Tables must NOT exist before migration
    let tables_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('activities', 'session_checkpoints')",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(
        tables_before, 0,
        "activities/session_checkpoints should not exist before migration"
    );

    migrate(&conn, None).unwrap();

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );

    // Both tables must exist after migration
    let tables_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('activities', 'session_checkpoints')",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(
        tables_after, 2,
        "activities and session_checkpoints should exist after migration"
    );

    // Verify activities columns
    let act_cols: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('activities')
                 WHERE name IN ('id', 'session_id', 'tool_name', 'args_hash', 'args',
                                'result_summary', 'outcome_class', 'status',
                                'occurrence_count', 'first_seen', 'last_seen',
                                'scope_id', 'promoted_fact_id')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        act_cols, 13,
        "activities table should have 13 expected columns"
    );

    // Verify session_checkpoints columns
    let cp_cols: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('session_checkpoints')
                 WHERE name IN ('session_id', 'scope_path', 'summary',
                                'last_activity_id', 'checkpoint_at', 'metadata')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cp_cols, 6,
        "session_checkpoints table should have 6 expected columns"
    );

    // Verify indexes (5 new)
    let idx_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name IN ('idx_activities_session', 'idx_activities_dedup',
                              'idx_activities_scope_recent', 'idx_activities_status',
                              'idx_checkpoints_scope')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        idx_count, 5,
        "all 5 activity/checkpoint indexes should exist"
    );
}

#[test]
fn fresh_db_has_activities_and_checkpoints() {
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();

    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('activities', 'session_checkpoints')",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 2,
        "fresh DB should have activities and session_checkpoints tables"
    );
}

// --- v9→v10 migration tests (idx_activities_dedup convergence) ---

/// Return the ordered indexed-column names of an index via `pragma_index_info`.
fn index_columns(conn: &Connection, index_name: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
        .unwrap();
    stmt.query_map([index_name], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}

/// Create a v9 schema as it was *originally shipped* — with the buggy 4-column
/// `idx_activities_dedup` index (missing `scope_id`) produced by the fresh-DB
/// path before the v9→v10 corrective migration. Used to exercise convergence.
fn init_schema_v9_buggy_dedup_index(conn: &Connection) -> Result<()> {
    init_schema(conn)?;
    // Replace the (now-fixed 5-col) index with the original buggy 4-col form
    // to simulate a fresh-v9 database created before the corrective migration.
    conn.execute_batch("DROP INDEX IF EXISTS idx_activities_dedup;")
        .map_err(StorageError::backend)?;
    conn.execute_batch(
        "CREATE INDEX idx_activities_dedup
                ON activities(session_id, tool_name, args_hash, outcome_class);",
    )
    .map_err(StorageError::backend)?;
    set_config(conn, "schema_version", "9")?;
    Ok(())
}

#[test]
fn fresh_and_migrated_dedup_index_have_identical_columns() {
    // Fresh DB at latest version.
    let fresh = open_memory().unwrap();
    init_schema(&fresh).unwrap();
    let fresh_cols = index_columns(&fresh, "idx_activities_dedup");

    // v8 → v9 → v10 migrated DB.
    let migrated = open_memory().unwrap();
    init_schema_v8(&migrated).unwrap();
    migrate(&migrated, None).unwrap();
    let migrated_cols = index_columns(&migrated, "idx_activities_dedup");

    // Both must include scope_id and be identical (5 columns).
    let expected = vec![
        "session_id".to_string(),
        "tool_name".to_string(),
        "args_hash".to_string(),
        "outcome_class".to_string(),
        "scope_id".to_string(),
    ];
    assert_eq!(
        fresh_cols, expected,
        "fresh-DB idx_activities_dedup must include scope_id (5 cols)"
    );
    assert_eq!(
        migrated_cols, expected,
        "migrated idx_activities_dedup must include scope_id (5 cols)"
    );
    assert_eq!(
        fresh_cols, migrated_cols,
        "fresh and migrated idx_activities_dedup must be identical"
    );
}

// --- v12→v13 embedding_spaces convergence + read-only registry read ---

/// Whitespace-normalized `sqlite_master.sql` for a named object (the same
/// normalization `deterministic_schema_dump` uses), so the hand-copied fresh-init
/// and frozen-migration DDL literals are compared by structure, not formatting.
fn normalized_object_sql(conn: &Connection, name: &str) -> String {
    let raw: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn fresh_vs_migrated_embedding_spaces_converge() {
    // Fresh-init DDL (TABLES_DDL) and the frozen migration snapshot must produce a
    // byte-identical table + partial index, so a v12→v13 migrated store and a fresh
    // v13 store are structurally interchangeable.
    let fresh = open_memory().unwrap();
    init_schema(&fresh).unwrap();

    // simulate_v12 drops the fresh table, so migrate() recreates it via the
    // frozen migrate_v12_to_v13 DDL — exactly the path we want to compare.
    let migrated = open_memory().unwrap();
    simulate_v12(&migrated);
    migrate(&migrated, None).unwrap();

    for obj in ["embedding_spaces", "idx_embedding_spaces_one_active"] {
        assert_eq!(
            normalized_object_sql(&fresh, obj),
            normalized_object_sql(&migrated, obj),
            "fresh-init and migrated {obj} DDL must converge"
        );
    }
    // The single-active partial index really is partial (WHERE status = 'active').
    assert!(
        normalized_object_sql(&fresh, "idx_embedding_spaces_one_active")
            .contains("WHERE status = 'active'"),
        "the one-active index must be partial"
    );
}

// --- v13 → v14 (fact_vectors, #623) ---

/// Roll a fresh DB back to a simulated v13 state: drop the v14 `fact_vectors` table +
/// its index and reset `schema_version` to 13.
fn simulate_v13(conn: &Connection) {
    init_schema(conn).unwrap();
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_fact_vectors_space;
             DROP TABLE IF EXISTS fact_vectors;",
    )
    .unwrap();
    set_config(conn, "schema_version", "13").unwrap();
}

#[test]
fn migrate_v13_to_v14_creates_empty_fact_vectors() {
    // Purely additive: the migration creates fact_vectors (empty) and bumps the
    // version. The active vectors stay in facts.embedding (no data move).
    let conn = open_memory().unwrap();
    simulate_v13(&conn);
    // fact_vectors does not exist at simulated v13.
    let exists_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='fact_vectors'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(exists_before, 0, "fact_vectors must not exist at v13");

    migrate(&conn, None).unwrap();

    assert_eq!(
        get_config(&conn, "schema_version").unwrap().as_deref(),
        Some("14")
    );
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM fact_vectors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "fact_vectors is created empty (no data move)");
}

#[test]
fn migrate_v13_to_v14_idempotent() {
    let conn = open_memory().unwrap();
    simulate_v13(&conn);
    migrate(&conn, None).unwrap();
    // Re-running migrate from v14 is a no-op.
    migrate(&conn, None).unwrap();
    assert_eq!(
        get_config(&conn, "schema_version").unwrap().as_deref(),
        Some("14")
    );
}

#[test]
fn fresh_vs_migrated_fact_vectors_converge() {
    // Fresh-init DDL and the frozen v13→v14 migration snapshot must produce a
    // byte-identical fact_vectors table + index.
    let fresh = open_memory().unwrap();
    init_schema(&fresh).unwrap();

    let migrated = open_memory().unwrap();
    simulate_v13(&migrated);
    migrate(&migrated, None).unwrap();

    for obj in ["fact_vectors", "idx_fact_vectors_space"] {
        assert_eq!(
            normalized_object_sql(&fresh, obj),
            normalized_object_sql(&migrated, obj),
            "fresh-init and migrated {obj} DDL must converge"
        );
    }
}

#[test]
fn read_only_open_reads_registry() {
    // load() is a pure SELECT, so a read-only open of a v13 store reads the active
    // identity (no migration, no table creation).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ro.db");
    let path_str = path.to_str().unwrap();
    let fp = crate::types::EmbeddingFingerprint::new("model-a", "tei", 8);
    {
        let conn = open_connection(path_str).unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();
        crate::store::embedding_meta::store(&conn, &fp).unwrap();
    }
    let ro = open_connection_read_only(path_str).unwrap();
    validate_schema_version(&ro).expect("a v13 store validates read-only");
    assert_eq!(
        crate::store::embedding_meta::load(&ro).unwrap(),
        Some(fp),
        "read-only open reads the registry identity"
    );
}

#[test]
fn migrate_v9_to_v10_converges_buggy_dedup_index() {
    let conn = open_memory().unwrap();
    init_schema_v9_buggy_dedup_index(&conn).unwrap();

    // Before migration: the index is the buggy 4-column form (no scope_id).
    let before = index_columns(&conn, "idx_activities_dedup");
    assert_eq!(
        before,
        vec![
            "session_id".to_string(),
            "tool_name".to_string(),
            "args_hash".to_string(),
            "outcome_class".to_string(),
        ],
        "precondition: fresh-v9 DB has buggy 4-column dedup index"
    );

    migrate(&conn, None).unwrap();

    assert_eq!(
        get_config(&conn, "schema_version").unwrap(),
        Some(CURRENT_SCHEMA_VERSION.to_string())
    );

    // After migration: the index converged to the 5-column form (incl scope_id).
    let after = index_columns(&conn, "idx_activities_dedup");
    assert_eq!(
        after,
        vec![
            "session_id".to_string(),
            "tool_name".to_string(),
            "args_hash".to_string(),
            "outcome_class".to_string(),
            "scope_id".to_string(),
        ],
        "v9→v10 migration must rebuild idx_activities_dedup with scope_id"
    );
}

#[test]
fn reopening_v10_db_leaves_dedup_index_unchanged() {
    // A fresh DB is already at v10 with the correct index; re-running the
    // migration chain on reopen (a no-op, since version == CURRENT) must not
    // change or drop the index.
    let conn = open_memory().unwrap();
    init_schema(&conn).unwrap();
    let before = index_columns(&conn, "idx_activities_dedup");
    migrate(&conn, None).unwrap();
    let after = index_columns(&conn, "idx_activities_dedup");
    assert_eq!(
        before, after,
        "re-running migrations on a fresh v10 DB must leave the dedup index unchanged"
    );
    assert_eq!(after.len(), 5, "dedup index must have 5 columns");
}
