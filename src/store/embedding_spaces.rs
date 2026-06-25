//! Embedding-space registry (#622): the multi-row generalization of the single
//! `embedding_meta` identity, mirroring the Knowledge layer's `embed_spaces` schema
//! (ADR 0015 §1).
//!
//! The Memory layer records exactly one `active` row — the degenerate single-space case
//! of KB's multi-space registry. This module owns the `embedding_spaces` table DDL (in
//! `store::schema`), the [`SpaceStatus`] ⇄ TEXT mapping, the [`EmbeddingSpace`] row type,
//! and the row CRUD. The single-active *policy* (write-once, the #614 mismatch check, the
//! dimension guard) lives one layer up in the [`store::embedding_meta`](super::embedding_meta)
//! facade, which delegates here; call sites speak only [`EmbeddingFingerprint`] and never
//! see the table or these types.
//!
//! The exactly-one-active invariant is **structural**: a partial unique index
//! `UNIQUE(status) WHERE status = 'active'` (mirrors KB's `idx_embed_spaces_one_active`).
//! It cannot be bypassed by any writer, now or in a future wave. A violation is remapped
//! by [`map_single_active_violation`] to a diagnosable error rather than an opaque
//! `Database` one.
//!
//! ## Future seams (do NOT implement here — separate issues)
//!
//! - `insert_populating(conn, &EmbeddingSpace)` — **#623**. A second, non-active row a
//!   background reconstruction backfills before promotion.
//! - `promote(conn, name)` / `deprecate(conn, name)` — **#689**. MUST demote the current
//!   `active` row to `deprecated` in the SAME transaction as activating `name`, so the
//!   partial unique index never transiently sees two actives; MUST invoke the #624 HNSW
//!   rebuild hook before the new space serves reads.
//! - Promoting [`SpaceStatus`] / [`EmbeddingSpace`] to public `types.rs` types — **#689**,
//!   when a multi-space API is actually exposed to consumers.

use rusqlite::Connection;

use crate::error::{MemoryError, MigrationError, Result};
use crate::types::EmbeddingFingerprint;

/// Lifecycle status of an embedding space in the registry (#622).
///
/// Mirrors the Knowledge layer's `embed_spaces.status` enum. Exactly one space is
/// [`Active`](SpaceStatus::Active) at any time, enforced structurally by the
/// `idx_embedding_spaces_one_active` partial unique index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpaceStatus {
    /// The live space. Reads and writes target it. Exactly one exists.
    Active,
    /// A shadow space being backfilled (#623). Not yet readable; never returned by the
    /// single-active facade.
    Populating,
    /// A retired space kept for rollback/audit (#689). Not read.
    Deprecated,
}

impl SpaceStatus {
    /// On-disk TEXT spelling. MUST match the table's `CHECK(status IN …)` list.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Populating => "populating",
            Self::Deprecated => "deprecated",
        }
    }

    /// Parse the on-disk TEXT spelling. A value outside the `CHECK` list is corrupt DB
    /// state — a hard error, never a silent default.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration(MigrationError::Incompatible)` for any unrecognized
    /// status string.
    pub fn from_sql(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(Self::Active),
            "populating" => Ok(Self::Populating),
            "deprecated" => Ok(Self::Deprecated),
            other => Err(MigrationError::Incompatible(format!(
                "corrupt embedding_spaces.status: {other:?}"
            ))
            .into()),
        }
    }
}

/// One row of the embedding-space registry (#622).
///
/// Wraps the canonical [`EmbeddingFingerprint`] identity tuple (by composition, so the
/// #614 full-tuple `Eq` contract and the normative serde key-set are preserved) with the
/// layer-internal lifecycle fields. The Memory layer records exactly one `Active` row
/// named [`DEFAULT_NAME`](EmbeddingSpace::DEFAULT_NAME).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingSpace {
    /// Unique space identifier (PK). The degenerate single space is `DEFAULT_NAME`.
    pub name: String,
    /// The canonical identity tuple — the cross-layer-parity fields (ADR 0015).
    pub fingerprint: EmbeddingFingerprint,
    /// Lifecycle status.
    pub status: SpaceStatus,
}

impl EmbeddingSpace {
    /// Name of the degenerate single space created by the v12→v13 migration and by the
    /// first embedding write on a fresh DB.
    pub const DEFAULT_NAME: &'static str = "default";

    /// The degenerate single `active` space carrying `fingerprint`.
    #[cfg(test)]
    pub fn default_active(fingerprint: EmbeddingFingerprint) -> Self {
        Self {
            name: Self::DEFAULT_NAME.to_string(),
            fingerprint,
            status: SpaceStatus::Active,
        }
    }
}

/// Decode a `SELECT name, model, provider, dim, matryoshka_base_dim, element_type, status`
/// row into an [`EmbeddingSpace`]. Guards the `i64 → usize` dimension reads (a negative or
/// overflowing stored dim is corrupt state).
fn row_to_space(row: &rusqlite::Row) -> rusqlite::Result<Result<EmbeddingSpace>> {
    let name: String = row.get(0)?;
    let model: String = row.get(1)?;
    let provider: String = row.get(2)?;
    let dim_raw: i64 = row.get(3)?;
    let base_raw: Option<i64> = row.get(4)?;
    let element_type: String = row.get(5)?;
    let status_raw: String = row.get(6)?;
    // The closure returns rusqlite::Result so query errors propagate; the inner Result
    // carries our domain decode errors (bad dim / bad status) without a panic.
    Ok((|| {
        let dim = usize::try_from(dim_raw).map_err(|_| {
            MemoryError::Internal("embedding_spaces.dim is negative/oversized".into())
        })?;
        let matryoshka_base_dim = match base_raw {
            Some(b) => Some(usize::try_from(b).map_err(|_| {
                MemoryError::Internal(
                    "embedding_spaces.matryoshka_base_dim is negative/oversized".into(),
                )
            })?),
            None => None,
        };
        Ok(EmbeddingSpace {
            name,
            fingerprint: EmbeddingFingerprint {
                model,
                provider,
                dim,
                matryoshka_base_dim,
                element_type,
            },
            status: SpaceStatus::from_sql(&status_raw)?,
        })
    })())
}

const SELECT_COLS: &str =
    "name, model, provider, dim, matryoshka_base_dim, element_type, status FROM embedding_spaces";

/// The single active space, or `None` on a store that has never embedded.
///
/// # Errors
///
/// Returns `MemoryError::Database` on query failure, `MemoryError::Internal` if more than
/// one active row exists (impossible under the partial unique index — fail loud, never
/// pick arbitrarily) or if a stored dimension/status is corrupt.
pub fn find_active(conn: &Connection) -> Result<Option<EmbeddingSpace>> {
    let sql = format!("SELECT {SELECT_COLS} WHERE status = 'active'");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let Some(first) = rows.next()? else {
        return Ok(None);
    };
    let space = row_to_space(first)??;
    if rows.next()?.is_some() {
        return Err(MemoryError::Internal(
            "multiple active embedding spaces — single-active invariant corrupted".into(),
        ));
    }
    Ok(Some(space))
}

/// All registered spaces, oldest first. Today returns 0 or 1 row; #623+ returns more.
///
/// # Errors
///
/// Returns `MemoryError::Database` on query failure, or `MemoryError::Internal`/`Migration`
/// if a stored dimension/status is corrupt.
pub fn list_spaces(conn: &Connection) -> Result<Vec<EmbeddingSpace>> {
    let sql = format!("SELECT {SELECT_COLS} ORDER BY created_at, name");
    let mut stmt = conn.prepare(&sql)?;
    let mut out = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        out.push(row_to_space(row)??);
    }
    Ok(out)
}

/// Insert a registry row. Used by the facade to record the first active space.
///
/// # Errors
///
/// Returns `MemoryError::Internal` (via [`map_single_active_violation`]) if the insert
/// would create a second active space, `MemoryError::Database` on any other write failure,
/// or `MemoryError::Internal` if a dimension overflows `i64`.
pub fn insert_active(conn: &Connection, space: &EmbeddingSpace) -> Result<()> {
    let (dim, base) = dims_to_sql(&space.fingerprint)?;
    conn.execute(
        "INSERT INTO embedding_spaces
             (name, model, provider, dim, matryoshka_base_dim, element_type, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            space.name,
            space.fingerprint.model,
            space.fingerprint.provider,
            dim,
            base,
            space.fingerprint.element_type,
            space.status.as_sql(),
        ],
    )
    .map_err(map_single_active_violation)?;
    Ok(())
}

/// Insert a shadow `populating` space being backfilled (#623 background reconstruction).
///
/// Its status is forced to `populating` (≠ `active`), so it coexists with the current
/// active space without tripping the single-active partial index. The atomic promote
/// (#623) later flips it to `active`; this is the registry-row half of the `#622`
/// reserved seam.
///
/// # Errors
///
/// Returns `MemoryError::Database` on a `name` PK collision or any other write failure, or
/// `MemoryError::Internal` if a dimension overflows `i64`.
pub fn insert_populating(conn: &Connection, space: &EmbeddingSpace) -> Result<()> {
    let (dim, base) = dims_to_sql(&space.fingerprint)?;
    conn.execute(
        "INSERT INTO embedding_spaces
             (name, model, provider, dim, matryoshka_base_dim, element_type, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'populating')",
        rusqlite::params![
            space.name,
            space.fingerprint.model,
            space.fingerprint.provider,
            dim,
            base,
            space.fingerprint.element_type,
        ],
    )
    .map_err(map_single_active_violation)?;
    Ok(())
}

/// Mark a space `deprecated` — the old active space retained for rollback after a promote,
/// or a `populating` space being abandoned (#623 mechanism; #689 drives the UX). Idempotent.
///
/// # Errors
///
/// Returns `MemoryError::Database` on write failure.
pub fn deprecate(conn: &Connection, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE embedding_spaces SET status = 'deprecated' WHERE name = ?1",
        rusqlite::params![name],
    )?;
    Ok(())
}

/// Overwrite the `default` active space's identity (the `store` upsert — restore/import
/// and unconditional re-stamp). Inserts the `default` active row if absent.
///
/// # Errors
///
/// Returns `MemoryError::Database` on write failure or `MemoryError::Internal` if a
/// dimension overflows `i64`.
pub fn upsert_active_fingerprint(conn: &Connection, fp: &EmbeddingFingerprint) -> Result<()> {
    let (dim, base) = dims_to_sql(fp)?;
    conn.execute(
        "INSERT INTO embedding_spaces
             (name, model, provider, dim, matryoshka_base_dim, element_type, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active')
         ON CONFLICT(name) DO UPDATE SET
             model = excluded.model,
             provider = excluded.provider,
             dim = excluded.dim,
             matryoshka_base_dim = excluded.matryoshka_base_dim,
             element_type = excluded.element_type,
             status = 'active'",
        rusqlite::params![
            EmbeddingSpace::DEFAULT_NAME,
            fp.model,
            fp.provider,
            dim,
            base,
            fp.element_type,
        ],
    )?;
    Ok(())
}

/// Map the fingerprint's `usize` dimensions to the SQL `i64`/`Option<i64>` columns,
/// erroring on the impossible overflow rather than truncating (keeps `forbid(unsafe)` +
/// clippy-nursery clean — no `as`).
fn dims_to_sql(fp: &EmbeddingFingerprint) -> Result<(i64, Option<i64>)> {
    let dim = i64::try_from(fp.dim)
        .map_err(|_| MemoryError::Internal("embedding dim exceeds i64".into()))?;
    let base = match fp.matryoshka_base_dim {
        Some(b) => Some(
            i64::try_from(b)
                .map_err(|_| MemoryError::Internal("matryoshka_base_dim exceeds i64".into()))?,
        ),
        None => None,
    };
    Ok((dim, base))
}

/// Remap a single-active partial-unique-index violation to a diagnosable engine error.
/// Any other rusqlite error — including a `name` PRIMARY KEY collision — passes through
/// unchanged (→ `MemoryError::Database`); only the `UNIQUE` extended code is the
/// single-active index firing, so we gate on it rather than the generic constraint code.
fn map_single_active_violation(e: rusqlite::Error) -> MemoryError {
    use rusqlite::ErrorCode;
    // SQLITE_CONSTRAINT_UNIQUE (2067) — the partial unique index on status='active'.
    // A PRIMARY KEY collision on `name` is SQLITE_CONSTRAINT_PRIMARYKEY (1555) and must
    // NOT be mislabeled as a single-active violation.
    if let rusqlite::Error::SqliteFailure(ffi, _) = &e
        && ffi.code == ErrorCode::ConstraintViolation
        && ffi.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    {
        return MemoryError::Internal(
            "embedding_spaces single-active invariant violated: attempted a second active \
             space (#622 partial unique index)"
                .into(),
        );
    }
    MemoryError::Database(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};

    fn fresh_conn() -> Connection {
        let conn = open_memory().expect("open in-memory db");
        init_schema(&conn).expect("init schema");
        conn
    }

    #[test]
    fn find_active_none_on_fresh() {
        let conn = fresh_conn();
        assert!(find_active(&conn).expect("find").is_none());
    }

    #[test]
    fn insert_then_find_active_roundtrip_no_mrl() {
        let conn = fresh_conn();
        let fp = EmbeddingFingerprint::new("model-a", "tei", 8);
        insert_active(&conn, &EmbeddingSpace::default_active(fp.clone())).expect("insert");
        let got = find_active(&conn).expect("find").expect("present");
        assert_eq!(got.name, "default");
        assert_eq!(got.status, SpaceStatus::Active);
        assert_eq!(got.fingerprint, fp);
    }

    #[test]
    fn insert_then_find_active_roundtrip_with_mrl() {
        let conn = fresh_conn();
        let fp =
            EmbeddingFingerprint::with_matryoshka("Qwen/Qwen3-Embedding-0.6B", "tei", 1024, 2048);
        insert_active(&conn, &EmbeddingSpace::default_active(fp.clone())).expect("insert");
        let got = find_active(&conn).expect("find").expect("present");
        assert_eq!(
            got.fingerprint, fp,
            "Some(base_dim) + element_type round-trip"
        );
    }

    #[test]
    fn list_spaces_returns_active() {
        let conn = fresh_conn();
        let fp = EmbeddingFingerprint::new("model-a", "tei", 8);
        insert_active(&conn, &EmbeddingSpace::default_active(fp.clone())).expect("insert");
        let spaces = list_spaces(&conn).expect("list");
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].fingerprint, fp);
        assert_eq!(spaces[0].status, SpaceStatus::Active);
    }

    #[test]
    fn space_status_sql_roundtrip() {
        for s in [
            SpaceStatus::Active,
            SpaceStatus::Populating,
            SpaceStatus::Deprecated,
        ] {
            assert_eq!(SpaceStatus::from_sql(s.as_sql()).unwrap(), s);
        }
        assert!(
            SpaceStatus::from_sql("bogus").is_err(),
            "unknown status is a hard error"
        );
    }

    #[test]
    fn single_active_index_rejects_second_active() {
        // The partial unique index makes a second active row unrepresentable; the error
        // is remapped to a diagnosable Internal (not an opaque Database), and the first
        // row is untouched.
        let conn = fresh_conn();
        let a = EmbeddingFingerprint::new("model-a", "tei", 8);
        insert_active(&conn, &EmbeddingSpace::default_active(a.clone())).expect("first insert");
        let second = EmbeddingSpace {
            name: "shadow".to_string(),
            fingerprint: EmbeddingFingerprint::new("model-b", "tei", 8),
            status: SpaceStatus::Active,
        };
        let err = insert_active(&conn, &second).expect_err("second active must be rejected");
        match err {
            MemoryError::Internal(msg) => {
                assert!(msg.contains("single-active invariant"), "got: {msg}");
            }
            other => panic!("expected Internal single-active error, got {other:?}"),
        }
        assert_eq!(
            find_active(&conn)
                .expect("find")
                .expect("present")
                .fingerprint,
            a,
            "first active row unchanged"
        );
    }

    #[test]
    fn name_pk_collision_is_not_mislabeled_single_active() {
        // A PRIMARY KEY collision on `name` (not the status='active' partial index) must
        // surface as a plain Database error, not the "single-active invariant violated"
        // message — only the UNIQUE extended code is the single-active index firing.
        let conn = fresh_conn();
        // A deprecated 'default' row: re-inserting 'default'/active PK-collides on name
        // without violating the single-active index (no other active row exists).
        insert_active(
            &conn,
            &EmbeddingSpace {
                name: "default".to_string(),
                fingerprint: EmbeddingFingerprint::new("m", "tei", 8),
                status: SpaceStatus::Deprecated,
            },
        )
        .expect("insert deprecated default");
        let err = insert_active(
            &conn,
            &EmbeddingSpace::default_active(EmbeddingFingerprint::new("m", "tei", 8)),
        )
        .expect_err("name PK collision must error");
        assert!(
            matches!(err, MemoryError::Database(_)),
            "PK collision must pass through as Database, got {err:?}"
        );
    }

    #[test]
    fn upsert_overwrites_default_active() {
        let conn = fresh_conn();
        let a = EmbeddingFingerprint::new("model-a", "tei", 8);
        let b = EmbeddingFingerprint::new("model-b", "tei", 8);
        upsert_active_fingerprint(&conn, &a).expect("upsert a");
        upsert_active_fingerprint(&conn, &b).expect("upsert b");
        assert_eq!(
            find_active(&conn)
                .expect("find")
                .expect("present")
                .fingerprint,
            b
        );
        assert_eq!(list_spaces(&conn).expect("list").len(), 1, "still one row");
    }

    #[test]
    fn insert_populating_coexists_with_active() {
        // A populating space is inserted alongside the active one WITHOUT tripping the
        // single-active partial index (populating ≠ active). #623 backfill staging.
        let conn = fresh_conn();
        insert_active(
            &conn,
            &EmbeddingSpace::default_active(EmbeddingFingerprint::new("model-a", "tei", 8)),
        )
        .expect("insert active");
        let shadow = EmbeddingSpace {
            name: "model-b_8".to_string(),
            fingerprint: EmbeddingFingerprint::new("model-b", "tei", 8),
            status: SpaceStatus::Populating,
        };
        insert_populating(&conn, &shadow).expect("insert populating coexists with active");

        // Both rows exist; exactly one is active.
        assert_eq!(list_spaces(&conn).expect("list").len(), 2);
        assert_eq!(
            find_active(&conn)
                .expect("find")
                .expect("present")
                .fingerprint
                .model,
            "model-a"
        );
        // deprecate() flips a space; idempotent.
        deprecate(&conn, "model-b_8").expect("deprecate");
        deprecate(&conn, "model-b_8").expect("deprecate idempotent");
        assert!(
            list_spaces(&conn)
                .expect("list")
                .into_iter()
                .any(|s| s.name == "model-b_8" && s.status == SpaceStatus::Deprecated),
            "model-b_8 is now deprecated"
        );
    }
}
