//! Typed persistence for the canonical embedding identity tuple (ADR 0015).
//!
//! Single owner of the on-disk encoding: the Memory layer records exactly one
//! [`EmbeddingFingerprint`], the degenerate single case of the Knowledge layer's
//! multi-space `embed_spaces` registry. Wave 2 (#622) generalizes this module
//! (value → table) without changing call sites, which speak only in
//! `EmbeddingFingerprint` values — never the JSON layout or the config key.
//!
//! The identity is established **lazily on the first embedding write** (ADR 0015
//! §2): [`record_if_absent`] is called by every embed-then-persist path before it
//! inserts a vector, so the store's identity is never older than its first vector.
//! Mismatch *enforcement* (rejecting a differing model) is **#614** — it switches
//! the [`record_if_absent`] present-branch from "return stored" to "compare and
//! reject". The call sites do not change when that lands.

use rusqlite::Connection;

use crate::error::{MemoryError, MigrationError, Result};
use crate::store::schema::{get_config, set_config};
use crate::types::EmbeddingFingerprint;

/// Config key under which the JSON-encoded identity tuple is stored.
///
/// Wave 2 (#622) retires this single-value key for an `embedding_spaces` table;
/// until then it is the one and only persisted location of the identity.
pub const EMBEDDING_META_KEY: &str = "embedding_meta";

/// Load the persisted embedding identity, if one has been recorded.
///
/// Returns `Ok(None)` on a fresh store (nothing embedded yet). Absence is a normal
/// state, not an error: the tuple is established on the first embedding write.
///
/// # Errors
///
/// Returns `MemoryError::Database` on query failure, or
/// `MemoryError::Migration(MigrationError::Incompatible)` if the value is present
/// but not valid `EmbeddingFingerprint` JSON — a hard error, never a silent default,
/// because the stored `dim` drives vector deserialization.
pub fn load(conn: &Connection) -> Result<Option<EmbeddingFingerprint>> {
    get_config(conn, EMBEDDING_META_KEY)?.map_or_else(
        || Ok(None),
        |raw| {
            serde_json::from_str(&raw).map(Some).map_err(|e| {
                MigrationError::Incompatible(format!(
                    "corrupt {EMBEDDING_META_KEY} config value: {e}"
                ))
                .into()
            })
        },
    )
}

/// Unconditionally persist the identity tuple, overwriting any existing value.
///
/// This is the upsert writer used by [`record_if_absent`] (first-write path) and
/// reserved for restore/import, where the tuple is reconstructed from a snapshot.
///
/// # Errors
///
/// Returns `MemoryError::Serialization` if encoding fails (cannot happen for a
/// valid fingerprint) or `MemoryError::Database` on write failure.
pub fn store(conn: &Connection, fp: &EmbeddingFingerprint) -> Result<()> {
    let json = serde_json::to_string(fp)?;
    set_config(conn, EMBEDDING_META_KEY, &json)
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
/// **This is the seam #614 extends**: it will switch the "already recorded" branch
/// from *return stored* to *compare `candidate == stored`, else
/// `Err(EmbeddingModelMismatch)`*. Returning the authoritative stored tuple (not
/// `()`) is deliberate so the caller and #614 share one value to reason about.
///
/// # Errors
///
/// Returns `MemoryError::EmbeddingDimension` if `candidate.dim != expected_dim`
/// (nothing is persisted), or any [`load`] / [`store`] error.
pub fn record_if_absent(
    conn: &Connection,
    candidate: &EmbeddingFingerprint,
    expected_dim: usize,
) -> Result<EmbeddingFingerprint> {
    if let Some(stored) = load(conn)? {
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
    fn record_if_absent_returns_stored_when_present() {
        // The #614-seam contract: today "stored wins"; #614 will make a differing
        // candidate an error. This test is *extended*, not rewritten, when #614 lands.
        let conn = fresh_conn();
        let a = EmbeddingFingerprint::new("model-a", "tei", 8);
        let b = EmbeddingFingerprint::new("model-b", "ollama", 8);
        store(&conn, &a).expect("store a");
        let returned = record_if_absent(&conn, &b, 8).expect("record b");
        assert_eq!(returned, a, "stored tuple wins, candidate ignored");
        assert_eq!(load(&conn).expect("load"), Some(a));
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
    fn load_errors_on_corrupt_json() {
        let conn = fresh_conn();
        set_config(&conn, EMBEDDING_META_KEY, "{not json").expect("set raw");
        assert!(load(&conn).is_err(), "corrupt JSON must be a hard error");
    }
}
