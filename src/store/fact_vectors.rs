//! `fact_vectors` row store (#623 background reconstruction).
//!
//! Holds per-`(fact, space)` embedding BLOBs for the **non-active** embedding
//! spaces: the `populating` space a background reconstruction is backfilling, and
//! the previous active space retained (as `deprecated`) after a promote so a
//! rollback can restore it. The **active** space's vector stays in
//! `facts.embedding` — the single served source of truth, read by all 17
//! `FACT_COLUMNS` query sites, the brute-force scan, HNSW, and dump. `fact_vectors`
//! is therefore *never* on the read path until the atomic promote (#623 D2/D6)
//! copy-swaps a populating vector into `facts.embedding`.
//!
//! ## Backfill covers EVERY fact (not just live ones)
//!
//! [`next_backfill_window`] and [`count_unbackfilled`] deliberately do **not**
//! filter on `t_expired`: a reconstruction must produce a populating vector for
//! *every* fact row, expired or not. The promote (#623 D6) copy-swaps with a
//! *total* `UPDATE facts SET embedding = (SELECT … space_id = :pop)`. Because
//! `facts.embedding` is `NOT NULL`, a fact lacking a populating vector would
//! either abort the swap (NULL into a `NOT NULL` column) or — if the swap were
//! scoped to "facts that have one" — leave `facts.embedding` a heterogeneous mix
//! of old- and new-space vectors, silently corrupting similarity for any reader
//! (e.g. `explain_fact`, `fact_history`) that does not filter expired facts.
//! Re-embedding expired facts costs a few extra provider calls; a homogeneous
//! active space is the correctness invariant that buys.

use rusqlite::{Connection, params};

use crate::error::{MemoryError, Result};
use crate::store::serialize_embedding;

/// Fetch the next window of facts that still lack a vector in `space_id`
/// (cursorless anti-join), as `(fact_id, content)` pairs.
///
/// The window is `facts LEFT JOIN fact_vectors … WHERE v.fact_id IS NULL AND
/// f.id > after_id ORDER BY f.id LIMIT limit`. The **absent** `fact_vectors` row
/// — not a persisted cursor — is the work signal, so the scan self-corrects after
/// a crash (restart at `after_id = 0`) or a concurrent insert (a fact added
/// mid-reconstruction simply shows up as un-backfilled). `after_id` is an
/// intra-run optimization to skip the already-written prefix; correctness does
/// not depend on it, because `facts.id` is monotonic `AUTOINCREMENT` (a
/// concurrently inserted fact always has `id` greater than any previously seen).
///
/// Returns `content` (the lossless source of truth) so the caller can re-embed it
/// under the new space's provider. Covers every fact, expired or not (see module
/// docs).
///
/// # Errors
///
/// Returns [`MemoryError::Database`](crate::error::MemoryError::Database) on query
/// failure, or [`MemoryError::Internal`](crate::error::MemoryError::Internal) if
/// `limit` overflows `i64`.
pub fn next_backfill_window(
    conn: &Connection,
    space_id: &str,
    after_id: i64,
    limit: usize,
) -> Result<Vec<(i64, String)>> {
    let limit = i64::try_from(limit)
        .map_err(|_| MemoryError::Internal("backfill window limit exceeds i64".into()))?;
    let mut stmt = conn.prepare(
        "SELECT f.id, f.content
           FROM facts f
           LEFT JOIN fact_vectors v ON v.fact_id = f.id AND v.space_id = ?1
          WHERE v.fact_id IS NULL AND f.id > ?2
          ORDER BY f.id
          LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![space_id, after_id, limit], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Idempotently write a batch of `(fact_id, embedding)` rows into `space_id`.
///
/// One `unchecked_transaction` commits the whole batch atomically. Each row
/// inserts with `ON CONFLICT(fact_id, space_id) DO NOTHING`, so a replay after a
/// crash — or a window re-derived by the anti-join — never duplicates or errors.
/// Returns the number of rows **actually inserted** (a conflict counts as 0),
/// which the idempotency gates assert on.
///
/// The embedding dimension is the caller's contract: the engine embeds with the
/// new space's provider, and the same-dim guard runs at promote (#623 D6).
/// `fact_vectors` carries no dim column, so no per-row dimension check happens
/// here.
///
/// # Errors
///
/// Returns [`MemoryError::Database`](crate::error::MemoryError::Database) on a
/// write failure — including a foreign-key violation if `space_id` is not a
/// registered space or a `fact_id` does not exist (FK enforcement is ON).
pub fn write_backfill_batch(
    conn: &Connection,
    space_id: &str,
    rows: &[(i64, Vec<f32>)],
) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0usize;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO fact_vectors (fact_id, space_id, embedding)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(fact_id, space_id) DO NOTHING",
        )?;
        for (fact_id, embedding) in rows {
            let blob = serialize_embedding(embedding);
            inserted += stmt.execute(params![fact_id, space_id, blob])?;
        }
    }
    tx.commit()?;
    Ok(inserted)
}

/// Count facts that still lack a vector in `space_id` — the same anti-join as
/// [`next_backfill_window`], reduced to a count. `0` means the space is fully
/// backfilled: the promote completeness gate (#623 D6). Covers every fact.
///
/// # Errors
///
/// Returns [`MemoryError::Database`](crate::error::MemoryError::Database) on query
/// failure, or [`MemoryError::Internal`](crate::error::MemoryError::Internal) if
/// the stored count is negative (impossible).
pub fn count_unbackfilled(conn: &Connection, space_id: &str) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*)
           FROM facts f
           LEFT JOIN fact_vectors v ON v.fact_id = f.id AND v.space_id = ?1
          WHERE v.fact_id IS NULL",
        params![space_id],
        |row| row.get(0),
    )?;
    usize::try_from(n).map_err(|_| MemoryError::Internal("negative unbackfilled count".into()))
}

/// Count the vectors stored for `space_id`. Test-only inspection helper (the
/// promote and dump paths read `fact_vectors` with their own queries); gated so it
/// never reaches the lib target as dead code. Un-gate if a production caller needs
/// a per-space count.
///
/// # Errors
///
/// Returns [`MemoryError::Database`](crate::error::MemoryError::Database) on query
/// failure, or [`MemoryError::Internal`](crate::error::MemoryError::Internal) if
/// the stored count is negative (impossible).
#[cfg(test)]
pub fn count_vectors(conn: &Connection, space_id: &str) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM fact_vectors WHERE space_id = ?1",
        params![space_id],
        |row| row.get(0),
    )?;
    usize::try_from(n).map_err(|_| MemoryError::Internal("negative vector count".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::embedding_spaces::{EmbeddingSpace, SpaceStatus, insert_populating};
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use crate::types::{EmbeddingFingerprint, FactType, NewFact};

    const DIM: usize = 4;
    const SPACE: &str = "shadow";

    fn fresh_conn() -> Connection {
        let conn = open_memory().expect("open in-memory db");
        init_schema(&conn).expect("init schema");
        conn
    }

    /// Register a `populating` shadow space so the `fact_vectors.space_id` FK is
    /// satisfied (FK enforcement is ON).
    fn add_populating_space(conn: &Connection) {
        insert_populating(
            conn,
            &EmbeddingSpace {
                name: SPACE.to_string(),
                fingerprint: EmbeddingFingerprint::new("model-b", "tei", DIM),
                status: SpaceStatus::Populating,
            },
        )
        .expect("insert populating space");
    }

    fn insert_fact(conn: &Connection, content: &str) -> i64 {
        FactStore::new(conn, DIM)
            .insert(&NewFact::builder(content, vec![0.0_f32; DIM], FactType::Semantic).build())
            .expect("insert fact")
    }

    /// Expire a fact (soft-delete) by stamping `t_expired`.
    fn expire_fact(conn: &Connection, id: i64) {
        conn.execute(
            "UPDATE facts SET t_expired = '2020-01-01T00:00:00Z' WHERE id = ?1",
            params![id],
        )
        .expect("expire fact");
    }

    fn vecs(ids: &[i64]) -> Vec<(i64, Vec<f32>)> {
        ids.iter().map(|&id| (id, vec![0.5_f32; DIM])).collect()
    }

    #[test]
    fn window_returns_all_unbackfilled_in_id_order() {
        let conn = fresh_conn();
        add_populating_space(&conn);
        let a = insert_fact(&conn, "alpha");
        let b = insert_fact(&conn, "beta");
        let c = insert_fact(&conn, "gamma");

        let window = next_backfill_window(&conn, SPACE, 0, 10).expect("window");
        assert_eq!(
            window.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![a, b, c],
            "every fact, id-ordered"
        );
        assert_eq!(window[0].1, "alpha", "content carried for re-embedding");
    }

    #[test]
    fn window_excludes_backfilled_rows() {
        let conn = fresh_conn();
        add_populating_space(&conn);
        let a = insert_fact(&conn, "alpha");
        let b = insert_fact(&conn, "beta");
        let c = insert_fact(&conn, "gamma");

        assert_eq!(
            write_backfill_batch(&conn, SPACE, &vecs(&[a])).expect("write"),
            1
        );
        let window = next_backfill_window(&conn, SPACE, 0, 10).expect("window");
        assert_eq!(
            window.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![b, c],
            "the written fact drops out of the anti-join"
        );
    }

    #[test]
    fn window_respects_after_id_and_limit() {
        let conn = fresh_conn();
        add_populating_space(&conn);
        let ids: Vec<i64> = (0..5)
            .map(|i| insert_fact(&conn, &format!("f{i}")))
            .collect();

        let first = next_backfill_window(&conn, SPACE, 0, 2).expect("first");
        assert_eq!(
            first.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ids[0..2]
        );
        let second = next_backfill_window(&conn, SPACE, ids[1], 2).expect("second");
        assert_eq!(
            second.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ids[2..4]
        );
    }

    #[test]
    fn write_batch_is_idempotent_on_replay() {
        let conn = fresh_conn();
        add_populating_space(&conn);
        let a = insert_fact(&conn, "alpha");
        let b = insert_fact(&conn, "beta");

        assert_eq!(
            write_backfill_batch(&conn, SPACE, &vecs(&[a, b])).expect("first write"),
            2,
            "both inserted"
        );
        assert_eq!(
            write_backfill_batch(&conn, SPACE, &vecs(&[a, b])).expect("replay"),
            0,
            "ON CONFLICT DO NOTHING — replay inserts nothing"
        );
        assert_eq!(
            count_vectors(&conn, SPACE).expect("count"),
            2,
            "no duplicates"
        );
    }

    #[test]
    fn count_unbackfilled_reaches_zero_when_fully_written() {
        let conn = fresh_conn();
        add_populating_space(&conn);
        let ids: Vec<i64> = (0..3)
            .map(|i| insert_fact(&conn, &format!("f{i}")))
            .collect();

        assert_eq!(count_unbackfilled(&conn, SPACE).expect("count"), 3);
        write_backfill_batch(&conn, SPACE, &vecs(&ids)).expect("write");
        assert_eq!(count_unbackfilled(&conn, SPACE).expect("count"), 0);
        assert_eq!(count_vectors(&conn, SPACE).expect("vectors"), 3);
    }

    #[test]
    fn backfill_covers_expired_facts() {
        // The homogeneity/NOT-NULL invariant (module docs): an expired fact MUST
        // still be backfilled, else the total copy-swap at promote would null its
        // embedding or split the active space across two identities.
        let conn = fresh_conn();
        add_populating_space(&conn);
        let live = insert_fact(&conn, "live");
        let gone = insert_fact(&conn, "gone");
        expire_fact(&conn, gone);

        let window = next_backfill_window(&conn, SPACE, 0, 10).expect("window");
        assert_eq!(
            window.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![live, gone],
            "the expired fact is still a backfill target"
        );
        assert_eq!(count_unbackfilled(&conn, SPACE).expect("count"), 2);
    }

    #[test]
    fn write_batch_rejects_unknown_space_via_foreign_key() {
        // FK enforcement is ON: writing into an unregistered space fails rather
        // than silently orphaning rows.
        let conn = fresh_conn();
        let a = insert_fact(&conn, "alpha");
        let err = write_backfill_batch(&conn, "no-such-space", &vecs(&[a])).expect_err("FK");
        assert!(matches!(err, MemoryError::Database(_)), "got {err:?}");
    }
}
