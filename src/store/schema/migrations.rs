//! Per-version migration functions for the `SQLite` schema.
//!
//! Each function applies the DDL delta that advances the schema from version N
//! to N+1. Functions are registered in the `MIGRATIONS` slice in `mod.rs` and
//! are invoked in order by [`super::migrate`].
//!
//! **Invariant**: SQL text in these functions is a *frozen snapshot* of the
//! schema at that version — never reference the live DDL constants from
//! `mod.rs`, which may evolve in future versions.

use rusqlite::Connection;

use crate::error::{MigrationError, Result};

pub(super) fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
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

/// Recreate the FTS5 virtual table and its sync triggers for the v3 schema
/// (inlined frozen snapshot), repopulating from the rebuilt facts table.
pub(super) fn recreate_facts_fts_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
            content,
            content='facts',
            content_rowid='id',
            tokenize='porter unicode61'
        );
        INSERT INTO facts_fts(rowid, content) SELECT id, content FROM facts;",
    )?;
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
    Ok(())
}

/// Rebuild tables to add `REFERENCES scopes(id)` on `scope_id` columns.
///
/// `ALTER TABLE` cannot add FK constraints in `SQLite`, so this migration
/// recreates each table with the full column definition. Requires
/// `PRAGMA foreign_keys = OFF` (handled by the migration framework).
///
/// Rebuild order respects FK dependencies: events → facts → edges → summaries.
pub(super) fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
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

    // 6+7. Recreate the FTS5 virtual table and its sync triggers.
    recreate_facts_fts_v3(conn)?;

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
pub(super) fn migrate_v3_to_v4(conn: &Connection) -> Result<()> {
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
pub(super) fn migrate_v4_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch("ALTER TABLE events ADD COLUMN event_revision INTEGER NOT NULL DEFAULT 1;")?;
    Ok(())
}

/// Add `surfaced_at` column for tracking when due facts are first returned to consumers.
pub(super) fn migrate_v5_to_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch("ALTER TABLE facts ADD COLUMN surfaced_at TEXT;")?;
    Ok(())
}

/// Add `archive_manifest` table to track `.pak` archive files.
///
/// The table is created unconditionally (not gated on the `archive` feature)
/// so that schema version is consistent regardless of feature flags. The Rust
/// code that reads/writes this table is gated behind `#[cfg(feature = "archive")]`.
pub(super) fn migrate_v6_to_v7(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS archive_manifest (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            pak_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            fact_count INTEGER NOT NULL,
            edge_count INTEGER NOT NULL,
            fact_id_min INTEGER NOT NULL,
            fact_id_max INTEGER NOT NULL,
            t_created_min TEXT NOT NULL,
            t_created_max TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            blake3_hash TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_archive_manifest_path
            ON archive_manifest(pak_path);",
    )?;
    Ok(())
}

pub(super) fn migrate_v7_to_v8(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS lineage (
            lineage_id INTEGER PRIMARY KEY AUTOINCREMENT,
            wisdom_fact_id INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
            source_fact_ids TEXT NOT NULL CHECK(json_valid(source_fact_ids)),
            provenance TEXT NOT NULL CHECK(json_valid(provenance))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_lineage_wisdom_fact_id
            ON lineage(wisdom_fact_id);",
    )?;
    Ok(())
}

pub(super) fn migrate_v8_to_v9(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS activities (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id       TEXT    NOT NULL,
            tool_name        TEXT    NOT NULL,
            args_hash        TEXT    NOT NULL,
            args             TEXT    NOT NULL DEFAULT '{}' CHECK(json_valid(args)),
            result_summary   TEXT,
            outcome_class    TEXT    NOT NULL DEFAULT 'success',
            status           TEXT    NOT NULL DEFAULT 'recorded'
                             CHECK(status IN ('recorded', 'deduplicated', 'ignored', 'promoted')),
            occurrence_count INTEGER NOT NULL DEFAULT 1,
            first_seen       TEXT    NOT NULL,
            last_seen        TEXT    NOT NULL,
            scope_id         INTEGER NOT NULL DEFAULT 1 REFERENCES scopes(id),
            promoted_fact_id INTEGER REFERENCES facts(id)
        );

        CREATE INDEX IF NOT EXISTS idx_activities_session
            ON activities(session_id);
        CREATE INDEX IF NOT EXISTS idx_activities_dedup
            ON activities(session_id, tool_name, args_hash, outcome_class, scope_id);
        CREATE INDEX IF NOT EXISTS idx_activities_scope_recent
            ON activities(scope_id, last_seen DESC);
        CREATE INDEX IF NOT EXISTS idx_activities_status
            ON activities(status);

        CREATE TABLE IF NOT EXISTS session_checkpoints (
            session_id       TEXT PRIMARY KEY,
            scope_path       TEXT,
            summary          TEXT,
            last_activity_id INTEGER REFERENCES activities(id) ON DELETE SET NULL,
            checkpoint_at    TEXT NOT NULL,
            metadata         TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata))
        );

        CREATE INDEX IF NOT EXISTS idx_checkpoints_scope
            ON session_checkpoints(scope_path);",
    )?;
    Ok(())
}

/// Converge the `idx_activities_dedup` index to its 5-column form.
///
/// The original v9 schema shipped with a mismatch: the fresh-DB DDL created
/// `idx_activities_dedup` with 4 columns (`session_id, tool_name, args_hash,
/// outcome_class`), while `migrate_v8_to_v9` created it with 5 (appending
/// `scope_id`). The dedup query filters all 5 columns, so fresh-v9 databases
/// got a less-selective index than migrated ones.
///
/// This corrective migration unconditionally drops and recreates the index in
/// the canonical 5-column form. `DROP INDEX IF EXISTS` makes it idempotent and
/// safe regardless of which 4- or 5-column variant a v9 database already has.
pub(super) fn migrate_v9_to_v10(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_activities_dedup;
         CREATE INDEX idx_activities_dedup
             ON activities(session_id, tool_name, args_hash, outcome_class, scope_id);",
    )?;
    Ok(())
}

/// Add an index on `facts.t_created` for recency/horizon sweeps.
///
/// A bulk backfill (autonomous-agent-project#53) plus any query filtering or
/// ordering by `t_created` (recency scans, memarch #42's horizon sweep) would
/// otherwise full-scan the table. The fresh-init path adds the same index via
/// `INDEXES_DDL`, so fresh and migrated databases converge.
pub(super) fn migrate_v10_to_v11(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_facts_created ON facts(t_created);")?;
    Ok(())
}

/// Replace the bare `embed_dim` config key with the `embedding_meta` identity
/// tuple (issue #613, ADR 0015).
///
/// No DDL change — the identity is a `config` row written on the first embedding
/// write, not a column. No data migration: the engine had no users at the v11→v12
/// boundary, so any pre-existing `embed_dim` is **dropped** rather than upgraded
/// (we lack `model`/`provider` to synthesize a full, *correct* tuple — a fabricated
/// `model:"unknown"` would be a wrong identity that #614 would then hard-reject on
/// every subsequent write). The identity is re-established on the next embedding
/// write. The `DELETE` is idempotent (a no-op on a fresh v12 DB that never had the
/// key), and runs inside the migration framework's per-step transaction.
pub(super) fn migrate_v11_to_v12(conn: &Connection) -> Result<()> {
    conn.execute_batch("DELETE FROM config WHERE key = 'embed_dim';")?;
    Ok(())
}

/// Decode target for the v12 `embedding_meta` config value.
///
/// Deliberately a **migration-local** struct, not `crate::types::EmbeddingFingerprint`:
/// a future change to the live type's serde shape must not silently alter what this
/// frozen v12→v13 step means (the module's frozen-snapshot invariant). It mirrors the
/// v12-era field set #613 persisted under the `embedding_meta` key.
#[derive(serde::Deserialize)]
struct V12Fingerprint {
    model: String,
    provider: String,
    dim: u64,
    #[serde(default)]
    matryoshka_base_dim: Option<u64>,
    element_type: String,
}

/// Generalize the single `embedding_meta` config value into the `embedding_spaces`
/// registry table (issue #622, ADR 0015 §1 — the degenerate single-space case).
///
/// Unlike v11→v12 (which *dropped* `embed_dim` for lack of a full tuple), the v12
/// `embedding_meta` value is a complete, correct identity, so this migration is
/// **lossless**: it parses that value — via the same serde path `embedding_meta::load`
/// used, so a corrupt value fails identically rather than becoming a fabricated row —
/// and writes it as the single `active` row named `'default'`, then retires the legacy
/// config key. A store that never embedded has no value: the table is created empty and
/// the identity is established lazily on the next embedding write (preserves #613). A
/// corrupt value is a hard error that rolls the step back (version stays 12), never a
/// fabricated identity. Idempotent: the table uses `IF NOT EXISTS` and the framework
/// gates the step on `version < target`.
///
/// Frozen DDL snapshot — never reference the live `TABLES_DDL` constant in `mod.rs`.
pub(super) fn migrate_v12_to_v13(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embedding_spaces (
            name                TEXT    PRIMARY KEY,
            model               TEXT    NOT NULL,
            provider            TEXT    NOT NULL,
            dim                 INTEGER NOT NULL,
            matryoshka_base_dim INTEGER,
            element_type        TEXT    NOT NULL DEFAULT 'float32',
            status              TEXT    NOT NULL DEFAULT 'active'
                                CHECK(status IN ('active', 'populating', 'deprecated')),
            created_at          TEXT    NOT NULL DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_embedding_spaces_one_active
            ON embedding_spaces(status) WHERE status = 'active';",
    )?;

    // Fold a present legacy identity into one active 'default' row. The table was just
    // created empty in this same step, so a plain INSERT cannot conflict (the version
    // gate makes the step run-once) — no ON CONFLICT needed.
    if let Some(raw) = super::get_config(conn, "embedding_meta")? {
        let fp: V12Fingerprint = serde_json::from_str(&raw).map_err(|e| {
            MigrationError::Incompatible(format!("corrupt embedding_meta during v12->v13: {e}"))
        })?;
        conn.execute(
            "INSERT INTO embedding_spaces
                 (name, model, provider, dim, matryoshka_base_dim, element_type, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active')",
            rusqlite::params![
                "default",
                fp.model,
                fp.provider,
                i64::try_from(fp.dim).map_err(|_| MigrationError::Incompatible(
                    "embedding_meta.dim exceeds i64".into()
                ))?,
                fp.matryoshka_base_dim
                    .map(
                        |d| i64::try_from(d).map_err(|_| MigrationError::Incompatible(
                            "embedding_meta.matryoshka_base_dim exceeds i64".into()
                        ))
                    )
                    .transpose()?,
                fp.element_type,
            ],
        )?;
        conn.execute_batch("DELETE FROM config WHERE key = 'embedding_meta';")?;
    }
    Ok(())
}
