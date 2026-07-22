//! Frozen DDL constants for the latest schema version.
//!
//! These string literals are the fresh-database DDL executed by
//! [`init_schema`](super::init_schema). They describe the schema at
//! [`CURRENT_SCHEMA_VERSION`](super::CURRENT_SCHEMA_VERSION); the
//! per-version migration deltas live in [`migrations`](super::migrations)
//! and carry their own frozen DDL snapshots.

pub(super) const TABLES_DDL: &str = "
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

CREATE TABLE IF NOT EXISTS archive_manifest (
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
    ON archive_manifest(pak_path);

CREATE TABLE IF NOT EXISTS lineage (
    lineage_id INTEGER PRIMARY KEY AUTOINCREMENT,
    wisdom_fact_id INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    source_fact_ids TEXT NOT NULL CHECK(json_valid(source_fact_ids)),
    provenance TEXT NOT NULL CHECK(json_valid(provenance))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_lineage_wisdom_fact_id
    ON lineage(wisdom_fact_id);

CREATE TABLE IF NOT EXISTS activities (
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
    ON session_checkpoints(scope_path);

CREATE TABLE IF NOT EXISTS embedding_spaces (
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
    ON embedding_spaces(status) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS fact_vectors (
    fact_id   INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    space_id  TEXT    NOT NULL REFERENCES embedding_spaces(name) ON DELETE CASCADE,
    embedding BLOB    NOT NULL,
    PRIMARY KEY (fact_id, space_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_fact_vectors_space ON fact_vectors(space_id);
";

pub(super) const SCOPES_DDL: &str = "
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

pub(super) const FTS5_DDL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    content,
    content='facts',
    content_rowid='id',
    tokenize='porter unicode61'
);
";

pub(super) const TRIGGERS_DDL: &str = "
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

pub(super) const INDEXES_DDL: &str = "
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
CREATE INDEX IF NOT EXISTS idx_facts_created ON facts(t_created);
CREATE INDEX IF NOT EXISTS idx_events_origin_seq ON events(origin_node_id, sequence_id);
";
