//! The fresh `PostgreSQL` migration chain (#633).
//!
//! Unlike `SQLite` — which *evolved* through 14 versions (`crate::store::schema`) — the
//! Postgres backend is **born at the v14 logical shape**. So this is ONE fresh chain,
//! not a replay of the `SQLite` per-version migrations, and it renders the schema in
//! idiomatic PG: real FK constraints (no FK-rebuild hack), `GENERATED ALWAYS AS
//! IDENTITY` ids (never reused — the #209 caller-write cursor invariant), `timestamptz`
//! / `jsonb` / `boolean`, a `tsvector` GENERATED column + GIN index (replacing `SQLite`'s
//! FTS5 virtual table + 3 sync triggers), and `vector(N)` pgvector columns.
//!
//! ## Version numbering — logical-equivalence, NOT numeric-equality
//!
//! [`CURRENT_PG_SCHEMA_VERSION`] is **1**, not 14. `SQLite` is at 14 because it passed
//! through 13 prior physical states; PG starts fresh at the v14 *logical* shape, so its
//! physical chain begins at 1. **Do NOT "sync" this number to `SQLite`'s** — when the PG
//! schema later evolves, append a `v1→v2` step here; never rewrite the v1 DDL.
//!
//! ## Transactionality
//!
//! The whole v1 chain runs in ONE transaction — Postgres DDL is transactional (a
//! genuine win over `SQLite`'s per-statement DDL: a mid-chain failure rolls the entire
//! schema back). NOTE for #635: `CREATE INDEX CONCURRENTLY` (a natural choice for the
//! deferred HNSW vector index) **cannot** run inside a transaction — it must be issued
//! outside this chain.

use tokio_postgres::Client;

use crate::error::{MemoryError, MigrationError, Result};

use super::pg_err;

/// The Postgres schema version this build produces. **1** (the fresh v14-logical shape)
/// — see the module docs on logical-equivalence ≠ numeric-equality.
pub const CURRENT_PG_SCHEMA_VERSION: u32 = 1;

/// Cross-backend coarse-compatibility epoch — must equal `crate::store::schema::STORAGE_EPOCH`
/// (the epoch is a *logical* gate, identical across backends).
const STORAGE_EPOCH: u16 = 1;

/// pgvector's maximum column dimension. A `vector(N)` with `N` above this fails at DDL
/// time inside Postgres; we reject it earlier with a typed [`MemoryError::Migration`].
const PGVECTOR_MAX_DIM: usize = 16_000;

/// Run the fresh chain to HEAD. Idempotent: on an at-HEAD database (the `config` table
/// exists and records the current version) this is a no-op.
///
/// # Errors
///
/// Returns [`MemoryError::Migration`] if `embed_dim` is outside pgvector's range, the
/// stored version is newer than supported, or the stored epoch is from the future;
/// [`MemoryError::EmbeddingDimension`] if reopening an existing store at a different
/// dimension than its `vector(N)` columns were built with; [`MemoryError::Storage`] on a
/// backend failure (which rolls the transaction back).
///
/// Takes `&mut Client` (not the pool's `Object`) so the migration logic is decoupled from
/// the connection-pool implementation.
pub async fn migrate(client: &mut Client, embed_dim: usize) -> Result<()> {
    if !(1..=PGVECTOR_MAX_DIM).contains(&embed_dim) {
        return Err(MigrationError::Incompatible(format!(
            "embed_dim {embed_dim} is outside the pgvector range 1..={PGVECTOR_MAX_DIM}"
        ))
        .into());
    }

    // If the `config` table exists, the chain already ran — re-check version/epoch and
    // return (idempotent at HEAD). A fresh database has no `config` table.
    if config_table_exists(client).await? {
        let epoch = read_epoch(client).await?;
        if epoch > STORAGE_EPOCH {
            return Err(MemoryError::UnsupportedEpoch {
                db_epoch: epoch,
                supported_epoch: STORAGE_EPOCH,
            });
        }
        let version = read_version(client).await?;
        if version > CURRENT_PG_SCHEMA_VERSION {
            return Err(MigrationError::SchemaVersionUnsupported {
                found: version,
                supported: CURRENT_PG_SCHEMA_VERSION,
            }
            .into());
        }
        // version == CURRENT (the only other reachable value for a one-step chain) — at
        // HEAD. Guard a reopen at a different dimension: the `vector(N)` columns were
        // baked at the original `embed_dim`, so reopening at a different dim would
        // silently mis-deserialize every vector. Caught here at open (review: gemini +
        // codex both flagged the missing reopen-dim check).
        check_stored_dim(client, embed_dim).await?;
        return Ok(());
    }

    // Fresh database: run the v1 chain atomically.
    let ddl = PG_V1_DDL.replace("{DIM}", &embed_dim.to_string());
    let tx = client.transaction().await.map_err(pg_err)?;
    tx.batch_execute(&ddl).await.map_err(pg_err)?;
    tx.commit().await.map_err(pg_err)?;
    Ok(())
}

/// Read-only schema compatibility check (the read-only open path): validate the
/// `config` table exists, the epoch is compatible, and the version matches — **without**
/// writing. Mirrors `crate::store::schema::validate_schema_version`.
///
/// # Errors
///
/// Returns [`MemoryError::Migration`] if the database is uninitialized, needs migration,
/// or is from a newer version; [`MemoryError::UnsupportedEpoch`] for a future epoch;
/// [`MemoryError::EmbeddingDimension`] if `embed_dim` disagrees with the store's
/// `vector(N)` width; [`MemoryError::Storage`] on a backend failure.
pub async fn validate_schema_version(client: &Client, embed_dim: usize) -> Result<()> {
    if !config_table_exists(client).await? {
        return Err(MigrationError::Incompatible(
            "database has no config table; cannot open read-only on an uninitialized database"
                .to_string(),
        )
        .into());
    }
    let epoch = read_epoch(client).await?;
    if epoch > STORAGE_EPOCH {
        return Err(MemoryError::UnsupportedEpoch {
            db_epoch: epoch,
            supported_epoch: STORAGE_EPOCH,
        });
    }
    let version = read_version(client).await?;
    if version > CURRENT_PG_SCHEMA_VERSION {
        return Err(MigrationError::SchemaVersionUnsupported {
            found: version,
            supported: CURRENT_PG_SCHEMA_VERSION,
        }
        .into());
    }
    if version < CURRENT_PG_SCHEMA_VERSION {
        return Err(MigrationError::SchemaVersionNeedsMigration {
            found: version,
            target: CURRENT_PG_SCHEMA_VERSION,
        }
        .into());
    }
    check_stored_dim(client, embed_dim).await?;
    Ok(())
}

/// Reject a reopen whose `embed_dim` disagrees with the `vector(N)` width baked into the
/// schema at first migrate. Parses the stored dimension from `facts.embedding`'s typmod
/// (`format_type` → e.g. `vector(4)`); a schema without the column (shouldn't happen at
/// HEAD) passes silently.
async fn check_stored_dim(client: &Client, embed_dim: usize) -> Result<()> {
    let row = client
        .query_opt(
            "SELECT format_type(atttypid, atttypmod) FROM pg_attribute \
             WHERE attrelid = 'public.facts'::regclass AND attname = 'embedding'",
            &[],
        )
        .await
        .map_err(pg_err)?;
    let Some(stored) = row.and_then(|r| {
        let formatted: String = r.get(0);
        formatted
            .strip_prefix("vector(")
            .and_then(|s| s.strip_suffix(')'))
            .and_then(|n| n.parse::<usize>().ok())
    }) else {
        return Ok(());
    };
    if stored != embed_dim {
        return Err(MemoryError::EmbeddingDimension {
            expected: stored,
            actual: embed_dim,
        });
    }
    Ok(())
}

/// Read a single `config` value (`None` when absent).
pub async fn get_config(client: &Client, key: &str) -> Result<Option<String>> {
    let row = client
        .query_opt("SELECT value FROM config WHERE key = $1", &[&key])
        .await
        .map_err(pg_err)?;
    Ok(row.map(|r| r.get::<_, String>(0)))
}

/// Upsert a `config` value.
pub async fn set_config(client: &Client, key: &str, value: &str) -> Result<()> {
    client
        .execute(
            "INSERT INTO config (key, value) VALUES ($1, $2) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[&key, &value],
        )
        .await
        .map_err(pg_err)?;
    Ok(())
}

async fn config_table_exists(client: &Client) -> Result<bool> {
    // Schema-qualify `public.config` — a bare `to_regclass('config')` is `search_path`-
    // sensitive, so a session with a non-default `search_path` would not find the table
    // and would wrongly attempt a fresh migration on an already-migrated database.
    let row = client
        .query_one("SELECT to_regclass('public.config') IS NOT NULL", &[])
        .await
        .map_err(pg_err)?;
    Ok(row.get::<_, bool>(0))
}

async fn read_version(client: &Client) -> Result<u32> {
    let raw = get_config(client, "schema_version")
        .await?
        .unwrap_or_else(|| "1".to_string());
    raw.parse::<u32>()
        .map_err(|_| MigrationError::Incompatible(format!("invalid schema_version: {raw}")).into())
}

async fn read_epoch(client: &Client) -> Result<u16> {
    let raw = get_config(client, "storage_epoch")
        .await?
        .unwrap_or_else(|| "1".to_string());
    raw.parse::<u16>()
        .map_err(|_| MigrationError::Incompatible(format!("invalid storage_epoch: {raw}")).into())
}

/// The fresh v14-logical schema, rendered natively for Postgres. `{DIM}` is replaced
/// with the pool's `embed_dim` at migrate time (pgvector needs a concrete dimension).
/// Tables are created in FK-dependency order (`scopes` first, `fact_vectors` last).
const PG_V1_DDL: &str = r"
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE scopes (
    id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    parent_id BIGINT REFERENCES scopes(id),
    label     TEXT   NOT NULL,
    depth     BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX idx_scopes_parent ON scopes(parent_id);
CREATE UNIQUE INDEX idx_scopes_parent_label ON scopes(parent_id, label);
-- Root scope sentinel (id=1). OVERRIDING SYSTEM VALUE forces an explicit value into an
-- IDENTITY column; the sequence is then advanced past 1 so the next auto-id is >= 2.
INSERT INTO scopes (id, parent_id, label, depth)
    OVERRIDING SYSTEM VALUE VALUES (1, NULL, 'root', 0)
    ON CONFLICT (id) DO NOTHING;
ALTER TABLE scopes ALTER COLUMN id RESTART WITH 2;

CREATE TABLE events (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    timestamp      TIMESTAMPTZ NOT NULL,
    event_type     TEXT        NOT NULL,
    payload        JSONB       NOT NULL DEFAULT '{}'::jsonb,
    source         TEXT        NOT NULL,
    session_id     TEXT,
    scope_id       BIGINT      NOT NULL DEFAULT 1 REFERENCES scopes(id),
    origin_node_id TEXT        NOT NULL DEFAULT 'local',
    sequence_id    BIGINT      NOT NULL DEFAULT 0,
    created_at     TIMESTAMPTZ,
    event_revision BIGINT      NOT NULL DEFAULT 1
);

CREATE TABLE facts (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    content          TEXT             NOT NULL,
    content_hash     TEXT             NOT NULL,
    embedding        VECTOR({DIM})    NOT NULL,
    fact_type        TEXT             NOT NULL CHECK (fact_type IN ('episodic', 'semantic', 'procedural')),
    t_created        TIMESTAMPTZ      NOT NULL,
    t_expired        TIMESTAMPTZ,
    t_valid          TIMESTAMPTZ,
    t_invalid        TIMESTAMPTZ,
    source_event_id  BIGINT           REFERENCES events(id),
    importance       DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    access_count     BIGINT           NOT NULL DEFAULT 0,
    last_accessed    TIMESTAMPTZ      NOT NULL,
    metadata         JSONB            NOT NULL DEFAULT '{}'::jsonb,
    scope_id         BIGINT           NOT NULL DEFAULT 1 REFERENCES scopes(id),
    is_pinned        BOOLEAN          NOT NULL DEFAULT false,
    importance_score DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    surfaced_at      TIMESTAMPTZ,
    -- FTS: a generated tsvector column + GIN index replaces SQLite's FTS5 virtual table
    -- and its 3 sync triggers. The two-argument to_tsvector(regconfig, text) form is
    -- IMMUTABLE (required for a generated column); the one-argument form would not be.
    content_tsv      TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', content)) STORED
);

CREATE TABLE edges (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_fact_id BIGINT           NOT NULL REFERENCES facts(id),
    target_fact_id BIGINT           NOT NULL REFERENCES facts(id),
    relation_type  TEXT             NOT NULL,
    weight         DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    t_created      TIMESTAMPTZ      NOT NULL,
    t_expired      TIMESTAMPTZ,
    scope_id       BIGINT           NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE summaries (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    content         TEXT          NOT NULL,
    embedding       VECTOR({DIM}) NOT NULL,
    level           TEXT          NOT NULL CHECK (level IN ('local', 'cluster', 'global')),
    source_fact_ids JSONB         NOT NULL DEFAULT '[]'::jsonb,
    created_at      TIMESTAMPTZ   NOT NULL,
    scope_id        BIGINT        NOT NULL DEFAULT 1 REFERENCES scopes(id)
);

CREATE TABLE config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE archive_manifest (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pak_path      TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL,
    fact_count    BIGINT      NOT NULL,
    edge_count    BIGINT      NOT NULL,
    fact_id_min   BIGINT      NOT NULL,
    fact_id_max   BIGINT      NOT NULL,
    t_created_min TIMESTAMPTZ NOT NULL,
    t_created_max TIMESTAMPTZ NOT NULL,
    size_bytes    BIGINT      NOT NULL,
    blake3_hash   TEXT        NOT NULL
);
CREATE UNIQUE INDEX idx_archive_manifest_path ON archive_manifest(pak_path);

CREATE TABLE lineage (
    lineage_id      BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    wisdom_fact_id  BIGINT NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    source_fact_ids JSONB  NOT NULL,
    provenance      JSONB  NOT NULL
);
CREATE UNIQUE INDEX idx_lineage_wisdom_fact_id ON lineage(wisdom_fact_id);

CREATE TABLE activities (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    session_id       TEXT    NOT NULL,
    tool_name        TEXT    NOT NULL,
    args_hash        TEXT    NOT NULL,
    args             JSONB   NOT NULL DEFAULT '{}'::jsonb,
    result_summary   TEXT,
    outcome_class    TEXT    NOT NULL DEFAULT 'success',
    status           TEXT    NOT NULL DEFAULT 'recorded'
                     CHECK (status IN ('recorded', 'deduplicated', 'ignored', 'promoted')),
    occurrence_count BIGINT  NOT NULL DEFAULT 1,
    first_seen       TIMESTAMPTZ NOT NULL,
    last_seen        TIMESTAMPTZ NOT NULL,
    scope_id         BIGINT  NOT NULL DEFAULT 1 REFERENCES scopes(id),
    promoted_fact_id BIGINT  REFERENCES facts(id)
);
CREATE INDEX idx_activities_session ON activities(session_id);
CREATE INDEX idx_activities_dedup
    ON activities(session_id, tool_name, args_hash, outcome_class, scope_id);
CREATE INDEX idx_activities_scope_recent ON activities(scope_id, last_seen DESC);
CREATE INDEX idx_activities_status ON activities(status);

CREATE TABLE session_checkpoints (
    session_id       TEXT PRIMARY KEY,
    scope_path       TEXT,
    summary          TEXT,
    last_activity_id BIGINT REFERENCES activities(id) ON DELETE SET NULL,
    checkpoint_at    TIMESTAMPTZ NOT NULL,
    metadata         JSONB NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX idx_checkpoints_scope ON session_checkpoints(scope_path);

CREATE TABLE embedding_spaces (
    name                TEXT    PRIMARY KEY,
    model               TEXT    NOT NULL,
    provider            TEXT    NOT NULL,
    dim                 BIGINT  NOT NULL,
    matryoshka_base_dim BIGINT,
    element_type        TEXT    NOT NULL DEFAULT 'float32',
    status              TEXT    NOT NULL DEFAULT 'active'
                        CHECK (status IN ('active', 'populating', 'deprecated')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_embedding_spaces_one_active
    ON embedding_spaces(status) WHERE status = 'active';

CREATE TABLE fact_vectors (
    fact_id   BIGINT        NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    space_id  TEXT          NOT NULL REFERENCES embedding_spaces(name) ON DELETE CASCADE,
    embedding VECTOR({DIM}) NOT NULL,
    PRIMARY KEY (fact_id, space_id)
);
CREATE INDEX idx_fact_vectors_space ON fact_vectors(space_id);

-- Secondary indexes (mirroring SQLite's INDEXES_DDL).
CREATE INDEX idx_events_session ON events(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX idx_events_timestamp ON events(timestamp);
CREATE INDEX idx_facts_expired ON facts(t_expired);
CREATE INDEX idx_facts_type ON facts(fact_type);
CREATE INDEX idx_facts_valid ON facts(t_valid, t_invalid);
CREATE INDEX idx_facts_hash ON facts(content_hash);
CREATE INDEX idx_edges_source ON edges(source_fact_id);
CREATE INDEX idx_edges_target ON edges(target_fact_id);
CREATE INDEX idx_edges_expired ON edges(t_expired);
CREATE INDEX idx_facts_scope ON facts(scope_id);
CREATE INDEX idx_edges_scope ON edges(scope_id);
CREATE INDEX idx_events_scope ON events(scope_id);
CREATE INDEX idx_summaries_scope ON summaries(scope_id);
CREATE INDEX idx_facts_pinned ON facts(is_pinned) WHERE is_pinned;
CREATE INDEX idx_facts_importance_score ON facts(importance_score);
CREATE INDEX idx_facts_t_valid_due ON facts(t_valid) WHERE t_valid IS NOT NULL AND t_expired IS NULL;
CREATE INDEX idx_facts_created ON facts(t_created);
CREATE INDEX idx_events_origin_seq ON events(origin_node_id, sequence_id);
-- FTS lexical index over the generated tsvector column (replaces the FTS5 vtable).
CREATE INDEX idx_facts_content_tsv ON facts USING GIN (content_tsv);

-- Stamp the schema version + epoch (the v14 LOGICAL shape is PG physical version 1).
INSERT INTO config (key, value) VALUES ('schema_version', '1'), ('storage_epoch', '1');
";
