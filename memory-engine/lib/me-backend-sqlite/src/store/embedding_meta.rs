//! Typed persistence for the canonical embedding identity tuple (ADR 0015).
//!
//! Single-active **facade** over the [`embedding_spaces`](super::embedding_spaces) registry:
//! the Memory layer records exactly one `active` space — the degenerate single case of the
//! Knowledge layer's multi-space `embed_spaces` registry. Wave 2 (#622) moved the identity
//! from a single JSON `config` value into the `embedding_spaces` table; these functions are
//! unchanged in signature and semantics (the single-active *policy* — write-once, the #614
//! mismatch check, the dimension guard — lives here; the row CRUD lives in the registry
//! module). Call sites speak only in `EmbeddingFingerprint` values, never the table.
//!
//! The identity is established **lazily on the first embedding write** (ADR 0015
//! §2): [`record_if_absent`] is called by every embed-then-persist path *as a vector
//! is written*, so the store's identity is never older than — and never recorded
//! without — its first vector. The transactional paths (`add_fact`, `add_facts_batch`,
//! `consolidate`, the bootstrap-session savepoint) record inside the same
//! transaction, gated on a vector actually being committed (#643), so a no-op run
//! leaves the store unstamped. The autocommit-per-file `bootstrap_memory_directory`
//! path is the deliberate exception: it records meta-first (before the first file)
//! because, lacking a wrapping transaction, deferral would expose an orphan-vector
//! crash window — it trades a harmless no-op stamp for crash safety.
//! Mismatch *enforcement* (rejecting a differing model) is **#614**: the
//! [`record_if_absent`] present-branch compares `candidate == stored` and returns
//! [`MemoryError::EmbeddingModelMismatch`] on any difference; [`check_compatible`] is
//! the read-only counterpart used for the eager fail-fast check at consumer startup.
//! The write-path call sites did not change when this landed.

use rusqlite::Connection;

use me_types::error::{MemoryError, Result};
use me_types::types::EmbeddingFingerprint;

/// Load the persisted embedding identity, if one has been recorded.
///
/// Returns `Ok(None)` on a fresh store (nothing embedded yet). Absence is a normal
/// state, not an error: the tuple is established on the first embedding write.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on query failure, or `MemoryError::Internal` if a stored
/// dimension/status in the registry is corrupt (never a silent default, because the stored
/// `dim` drives vector deserialization).
pub fn load(conn: &Connection) -> Result<Option<EmbeddingFingerprint>> {
    Ok(super::embedding_spaces::find_active(conn)?.map(|s| s.fingerprint))
}

/// Unconditionally persist the identity tuple, overwriting any existing value.
///
/// Upserts the single `active` registry row. Used by [`record_if_absent`] (first-write
/// path) and reserved for restore/import, where the tuple is reconstructed from a snapshot.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on write failure or `MemoryError::Internal` if a
/// dimension overflows `i64`.
pub fn store(conn: &Connection, fp: &EmbeddingFingerprint) -> Result<()> {
    super::embedding_spaces::upsert_active_fingerprint(conn, fp)
}

/// Write-once recording of the identity on the first embedding write.
///
/// If no identity is recorded, guards `candidate.dim == expected_dim` then persists
/// `candidate` and returns it. If one is already recorded, returns the **stored**
/// tuple unchanged (the candidate is ignored).
///
/// The `expected_dim` guard lives here — not in the engine seam — so that every
/// path inherits it, including the `consolidate` free function which calls this
/// directly rather than through the engine method. The guard refuses to persist an
/// internally inconsistent tuple (claims dim D while the engine stores dim-E
/// vectors); it is the dimension contract `FactStore::insert` already enforces on
/// the vector, lifted to the identity tuple. It is **not** model-identity
/// enforcement (#614).
///
/// When an identity is already recorded, `candidate` must **equal** it or this returns
/// [`MemoryError::EmbeddingModelMismatch`] (#614) — mixing two models' vectors in one
/// space silently corrupts retrieval. On a match, the stored tuple is returned
/// unchanged. Returning the authoritative stored tuple (not `()`) lets the caller
/// reason about the established identity.
///
/// # Errors
///
/// Returns [`MemoryError::EmbeddingModelMismatch`] if an identity is recorded and
/// `candidate` differs from it; `MemoryError::EmbeddingDimension` if no identity is
/// recorded and `candidate.dim != expected_dim` (nothing is persisted); or any
/// [`load`] / [`store`] error.
pub fn record_if_absent(
    conn: &Connection,
    candidate: &EmbeddingFingerprint,
    expected_dim: usize,
) -> Result<EmbeddingFingerprint> {
    if let Some(stored) = load(conn)? {
        ensure_match(&stored, candidate)?;
        return Ok(stored);
    }
    if candidate.dim != expected_dim {
        return Err(MemoryError::EmbeddingDimension {
            expected: expected_dim,
            actual: candidate.dim,
        });
    }
    store(conn, candidate)?;
    Ok(candidate.clone())
}

/// Verify a candidate identity is compatible with the store's recorded one, without writing.
///
/// Returns `Ok(())` on a fresh store (no identity yet — nothing to disagree
/// with) or when `candidate` equals the stored tuple.
///
/// This is the read-only counterpart to [`record_if_absent`], used for the **eager
/// fail-fast check** at consumer startup (#614, §Design.2): the query path embeds at
/// the consumer and hands the engine a pre-computed vector, so the engine cannot
/// fingerprint-check per query — this check catches a misconfigured provider once,
/// before any query silently returns wrong-vector-space results.
///
/// # Errors
///
/// Returns [`MemoryError::EmbeddingModelMismatch`] if an identity is recorded and
/// `candidate` differs from it, or any [`load`] error.
pub fn check_compatible(conn: &Connection, candidate: &EmbeddingFingerprint) -> Result<()> {
    load(conn)?.map_or_else(|| Ok(()), |stored| ensure_match(&stored, candidate))
}

/// Reject a `candidate` that differs from the `stored` identity (#614).
///
/// Equality is field-by-field over the whole tuple (`EmbeddingFingerprint: Eq`), so a
/// difference in *any* identity field — `model`, `provider`, `dim`,
/// `matryoshka_base_dim`, or `element_type` — is a mismatch.
fn ensure_match(stored: &EmbeddingFingerprint, candidate: &EmbeddingFingerprint) -> Result<()> {
    if stored == candidate {
        return Ok(());
    }
    Err(MemoryError::EmbeddingModelMismatch {
        expected: Box::new(stored.clone()),
        actual: Box::new(candidate.clone()),
    })
}

/// Require that a store has a recorded embedding identity before a **pre-computed-embedding** write.
///
/// A pre-computed-embedding write is one with no live `EmbeddingProvider` to
/// fingerprint — `promote`, or a cycle `AddFact`/`Synthesize` delta.
///
/// Such a write cannot establish the identity itself (#613 has no fingerprint to
/// record; declaring a model for a pre-computed vector is #615), so committing it
/// against an un-stamped store would leave a vector with no `embedding_meta` — the
/// #614 silent-corruption landmine. Reject it instead; the store must first be
/// stamped by a real embedding write (`add_fact`/bootstrap/consolidate).
///
/// # Errors
///
/// Returns `MemoryError::Internal` when no identity is recorded.
pub fn require_present(conn: &Connection) -> Result<()> {
    if load(conn)?.is_none() {
        return Err(MemoryError::Internal(
            "cannot write a pre-computed embedding to a store with no embedding identity; \
             write a fact first"
                .into(),
        ));
    }
    Ok(())
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
    fn load_none_on_fresh() {
        let conn = fresh_conn();
        assert!(load(&conn).expect("load").is_none());
    }

    #[test]
    fn store_then_load_roundtrip() {
        let conn = fresh_conn();
        // with_matryoshka exercises Some(base_dim) + the non-default fields.
        let fp =
            EmbeddingFingerprint::with_matryoshka("Qwen/Qwen3-Embedding-0.6B", "tei", 1024, 1024);
        store(&conn, &fp).expect("store");
        assert_eq!(load(&conn).expect("load"), Some(fp));
    }

    #[test]
    fn record_if_absent_writes_when_absent() {
        let conn = fresh_conn();
        let fp = EmbeddingFingerprint::new("model-a", "tei", 8);
        let returned = record_if_absent(&conn, &fp, 8).expect("record");
        assert_eq!(returned, fp);
        assert_eq!(load(&conn).expect("load"), Some(fp));
    }

    #[test]
    fn record_if_absent_returns_stored_when_candidate_matches() {
        // Idempotent re-record with the SAME identity returns the stored tuple.
        let conn = fresh_conn();
        let a = EmbeddingFingerprint::new("model-a", "tei", 8);
        store(&conn, &a).expect("store a");
        let returned = record_if_absent(&conn, &a, 8).expect("record matching");
        assert_eq!(returned, a);
        assert_eq!(load(&conn).expect("load"), Some(a));
    }

    #[test]
    fn record_if_absent_rejects_model_mismatch() {
        // #614 enforcement: a DIFFERENT identity at the same dim is hard-rejected,
        // and the stored identity is left untouched.
        let conn = fresh_conn();
        let a = EmbeddingFingerprint::new("model-a", "tei", 8);
        let b = EmbeddingFingerprint::new("model-b", "ollama", 8);
        store(&conn, &a).expect("store a");
        let err = record_if_absent(&conn, &b, 8).expect_err("model mismatch must error");
        match err {
            MemoryError::EmbeddingModelMismatch { expected, actual } => {
                assert_eq!(*expected, a, "expected = authoritative stored identity");
                assert_eq!(*actual, b, "actual = differing candidate");
            }
            other => panic!("expected EmbeddingModelMismatch, got {other:?}"),
        }
        assert_eq!(
            load(&conn).expect("load"),
            Some(a),
            "stored identity unchanged"
        );
    }

    #[test]
    fn check_compatible_ok_on_fresh_store() {
        // No identity recorded yet -> nothing to disagree with.
        let conn = fresh_conn();
        let fp = EmbeddingFingerprint::new("model-a", "tei", 8);
        check_compatible(&conn, &fp).expect("fresh store is compatible with anything");
    }

    #[test]
    fn check_compatible_ok_on_match_err_on_differ() {
        let conn = fresh_conn();
        let a = EmbeddingFingerprint::new("model-a", "tei", 8);
        store(&conn, &a).expect("store a");
        // Same identity -> Ok (no write, read-only check).
        check_compatible(&conn, &a).expect("matching identity is compatible");
        // Differ in matryoshka_base_dim only -> still a mismatch (full-tuple equality).
        let mrl = EmbeddingFingerprint::with_matryoshka("model-a", "tei", 8, 16);
        let err = check_compatible(&conn, &mrl).expect_err("differing tuple must error");
        assert!(
            matches!(err, MemoryError::EmbeddingModelMismatch { .. }),
            "expected EmbeddingModelMismatch, got {err:?}"
        );
    }

    #[test]
    fn record_if_absent_rejects_dim_mismatch() {
        let conn = fresh_conn();
        let fp = EmbeddingFingerprint::new("model-a", "tei", 16);
        let err = record_if_absent(&conn, &fp, 8).expect_err("dim mismatch must error");
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension {
                    expected: 8,
                    actual: 16
                }
            ),
            "expected EmbeddingDimension, got {err:?}"
        );
        assert!(
            load(&conn).expect("load").is_none(),
            "nothing persisted on mismatch"
        );
    }

    #[test]
    fn store_overwrites() {
        let conn = fresh_conn();
        let a = EmbeddingFingerprint::new("model-a", "tei", 8);
        let b = EmbeddingFingerprint::new("model-b", "tei", 8);
        store(&conn, &a).expect("store a");
        store(&conn, &b).expect("store b");
        assert_eq!(load(&conn).expect("load"), Some(b));
    }

    #[test]
    fn store_then_load_preserves_matryoshka_and_element_type() {
        // The registry stores the fingerprint as typed columns; prove the nullable
        // matryoshka_base_dim and a non-default element_type round-trip (replaces the
        // JSON round-trip the old single-value encoding relied on).
        let conn = fresh_conn();
        let mut fp = EmbeddingFingerprint::with_matryoshka("m", "tei", 768, 1536);
        fp.element_type = "int8".to_string();
        store(&conn, &fp).expect("store");
        assert_eq!(load(&conn).expect("load"), Some(fp));
    }

    #[test]
    fn load_none_when_only_deprecated_row() {
        // The single-active read contract: a store whose only space is `deprecated`
        // reads like a fresh store (`load` → None). Replaces the old corrupt-config-JSON
        // test — that encoding no longer exists; the migration's corrupt-value rejection
        // is covered in `schema::tests::migrate_v12_to_v13_rejects_corrupt_legacy_value`.
        let conn = fresh_conn();
        conn.execute(
            "INSERT INTO embedding_spaces
                 (name, model, provider, dim, matryoshka_base_dim, element_type, status)
             VALUES ('retired', 'm', 'tei', 8, NULL, 'float32', 'deprecated')",
            [],
        )
        .expect("insert deprecated row");
        assert!(
            load(&conn).expect("load").is_none(),
            "a deprecated-only store has no active identity"
        );
    }
}
