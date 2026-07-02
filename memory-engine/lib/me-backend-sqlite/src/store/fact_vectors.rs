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

use me_types::error::StorageError;
use rusqlite::{Connection, params};

use crate::store::embedding_spaces::{self, SpaceStatus};
use crate::store::serialize_embedding;
use me_types::error::{MemoryError, Result};
use me_types::types::PromoteOutcome;

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
/// Returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage) on query
/// failure, or [`MemoryError::Internal`](me_types::error::MemoryError::Internal) if
/// `limit` overflows `i64`.
pub fn next_backfill_window(
    conn: &Connection,
    space_id: &str,
    after_id: i64,
    limit: usize,
) -> Result<Vec<(i64, String)>> {
    let limit = i64::try_from(limit)
        .map_err(|_| MemoryError::Internal("backfill window limit exceeds i64".into()))?;
    let mut stmt = conn
        .prepare(
            "SELECT f.id, f.content
           FROM facts f
           LEFT JOIN fact_vectors v ON v.fact_id = f.id AND v.space_id = ?1
          WHERE v.fact_id IS NULL AND f.id > ?2
          ORDER BY f.id
          LIMIT ?3",
        )
        .map_err(StorageError::backend)?;
    let rows = stmt
        .query_map(params![space_id, after_id, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(StorageError::backend)?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::backend)?)
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
/// Returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage) on a
/// write failure — including a foreign-key violation if `space_id` is not a
/// registered space or a `fact_id` does not exist (FK enforcement is ON).
pub fn write_backfill_batch(
    conn: &Connection,
    space_id: &str,
    rows: &[(i64, Vec<f32>)],
) -> Result<usize> {
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    let mut inserted = 0usize;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO fact_vectors (fact_id, space_id, embedding)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(fact_id, space_id) DO NOTHING",
            )
            .map_err(StorageError::backend)?;
        for (fact_id, embedding) in rows {
            let blob = serialize_embedding(embedding);
            inserted += stmt
                .execute(params![fact_id, space_id, blob])
                .map_err(StorageError::backend)?;
        }
    }
    tx.commit().map_err(StorageError::backend)?;
    Ok(inserted)
}

/// Count facts that still lack a vector in `space_id`.
///
/// The same anti-join as
/// [`next_backfill_window`], reduced to a count. `0` means the space is fully
/// backfilled: the promote completeness gate (#623 D6). Covers every fact.
///
/// # Errors
///
/// Returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage) on query
/// failure, or [`MemoryError::Internal`](me_types::error::MemoryError::Internal) if
/// the stored count is negative (impossible).
pub fn count_unbackfilled(conn: &Connection, space_id: &str) -> Result<usize> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*)
           FROM facts f
           LEFT JOIN fact_vectors v ON v.fact_id = f.id AND v.space_id = ?1
          WHERE v.fact_id IS NULL",
            params![space_id],
            |row| row.get(0),
        )
        .map_err(StorageError::backend)?;
    usize::try_from(n).map_err(|_| MemoryError::Internal("negative unbackfilled count".into()))
}

/// Atomically promote the `populating` space to active (#623 D6) — the O(N) copy-swap.
///
/// **One transaction**, never decomposed (the #631-incident lesson:
/// any `?` before `commit` drops the transaction and rusqlite rolls back, so a
/// mid-promote failure can never leave partial state).
///
/// Steps, in order:
/// 0. **Resolve** the active + populating spaces (no dim guard — a promote is
///    dimension-agnostic at the storage layer since the copy-swap is a blob-level
///    `UPDATE`; #742 allows `populating.dim != active.dim`. The width invariant is
///    the engine-side `backfill_space` per-vector check against the populating
///    space's declared dim, and a different-dim promote fences the engine handle
///    until it reopens at the new dim).
/// 1. **Completeness gate INSIDE the tx** (no TOCTOU) — every fact must already
///    have a populating vector. A non-zero count aborts (a straggler arrived
///    after the engine's pre-tx catch-up).
/// 2. **Retain** the old active vectors into `fact_vectors[old]` for rollback.
/// 3. **Copy-swap** the populating vectors into `facts.embedding` (the active
///    serving store). The gate guarantees the subquery is never `NULL`
///    (`facts.embedding` is `NOT NULL`) and covers every fact → the active space
///    stays homogeneous.
/// 4. **Status flip** — demote the old active, then activate the populating row
///    (demote-then-activate, so the single-active partial index is never violated).
///    This flip *is* the identity flip: `embedding_meta::load` reads the active
///    row's fingerprint.
/// 5. **Cleanup** — delete `fact_vectors[populating]` (now redundant with
///    `facts.embedding`; the active space's vectors never live in `fact_vectors`).
///
/// Returns a [`PromoteOutcome`] carrying the swapped-fact count, the deprecated
/// old space's name, and the new active fingerprint. `stragglers_caught` is `0`
/// here (the engine sets it from its catch-up); `rebuild_index` is always `true`.
///
/// # Errors
///
/// Returns [`MemoryError::Internal`] if there is no active space, the populating
/// space is absent or not `populating`, or the completeness gate fails, or
/// [`MemoryError::Storage`] on a write failure (which rolls the transaction back).
pub fn promote_space(conn: &Connection, populating: &str) -> Result<PromoteOutcome> {
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;

    // (0) Resolve both spaces. NO dim guard: a promote is dimension-agnostic at the
    // storage layer (the copy-swap below is a blob-level UPDATE), so #742 allows the
    // populating space's dim to differ from the active one. The width invariant is
    // enforced engine-side by `backfill_space`'s per-vector check against the
    // populating space's declared dim (the sole backstop once this guard is gone),
    // and a different-dim promote leaves the engine handle fenced until it reopens at
    // the new dim. The completeness gate (step 1) still guarantees every fact has a
    // populating vector, so the total copy-swap never nulls `facts.embedding`.
    let active = embedding_spaces::find_active(&tx)?.ok_or_else(|| {
        MemoryError::Internal("promote: no active embedding space to swap over".into())
    })?;
    let new = embedding_spaces::find_by_name(&tx, populating)?.ok_or_else(|| {
        MemoryError::Internal(format!(
            "promote: populating space {populating:?} not found"
        ))
    })?;
    if new.status != SpaceStatus::Populating {
        return Err(MemoryError::Internal(format!(
            "promote: space {populating:?} is {:?}, expected populating",
            new.status
        )));
    }

    // (1) Completeness gate INSIDE the tx (no TOCTOU).
    let missing = count_unbackfilled(&tx, populating)?;
    if missing != 0 {
        return Err(MemoryError::Internal(format!(
            "promote: {missing} fact(s) un-backfilled in {populating:?} — backfill before promote"
        )));
    }

    // (2) Retain the old active vectors (keyed by the old space) for rollback.
    // No `ON CONFLICT`: the active space — by the `fact_vectors`-holds-only-
    // non-active invariant — has no rows here, so a PK collision is impossible;
    // were one to occur it would signal a corrupted invariant and should error,
    // not silently skip. (It also dodges the SQLite `INSERT … SELECT … ON CONFLICT`
    // parser ambiguity.)
    tx.execute(
        "INSERT INTO fact_vectors (fact_id, space_id, embedding)
         SELECT id, ?1, embedding FROM facts",
        params![active.name],
    )
    .map_err(StorageError::backend)?;

    // (3) Copy-swap the populating vectors into the active serving store.
    let promoted = tx
        .execute(
            "UPDATE facts SET embedding =
            (SELECT embedding FROM fact_vectors WHERE fact_id = facts.id AND space_id = ?1)",
            params![populating],
        )
        .map_err(StorageError::backend)?;

    // (4) Demote-then-activate (the partial-unique index never sees two actives).
    embedding_spaces::deprecate(&tx, &active.name)?;
    embedding_spaces::activate(&tx, populating)?;

    // (5) Drop the now-redundant populating vectors.
    tx.execute(
        "DELETE FROM fact_vectors WHERE space_id = ?1",
        params![populating],
    )
    .map_err(StorageError::backend)?;

    tx.commit().map_err(StorageError::backend)?;

    Ok(PromoteOutcome {
        promoted,
        deprecated_space: active.name,
        new_fingerprint: new.fingerprint,
        stragglers_caught: 0,
        rebuild_index: true,
    })
}

/// Stream every `fact_vectors` row (all non-active spaces) for the dump.
///
/// Yields
/// `(fact_id, space_id, embedding)` one row at a time — O(1) peak memory, matching
/// the streaming snapshot writer. Ordered by `(space_id, fact_id)` for a stable
/// dump.
///
/// Deserializes each blob at **its own space's recorded dimension** (#742): after
/// a different-dimension reconstruction the active space is D′ while a retained
/// `deprecated` space is still D-wide, so a single engine `embed_dim` cannot decode
/// every row. The per-space dims are read once from `embedding_spaces`; `embed_dim`
/// is only a fallback for a row whose space is somehow absent (the FK makes that
/// impossible). This preserves the deprecated space's rollback vectors across a
/// dump/restore of a reconstructed store (no data loss).
///
/// # Errors
///
/// Returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage) on query
/// failure, [`MemoryError::EmbeddingDimension`](me_types::error::MemoryError::EmbeddingDimension)
/// if a stored blob is not its space's dim, or any error the callback returns.
pub fn for_each<F>(conn: &Connection, embed_dim: usize, mut cb: F) -> Result<()>
where
    F: FnMut(i64, String, Vec<f32>) -> Result<()>,
{
    // Per-space dims: a non-active space may differ from the active `embed_dim`
    // after a #742 different-dim reconstruction.
    let space_dims: std::collections::HashMap<String, usize> = embedding_spaces::list_spaces(conn)?
        .into_iter()
        .map(|s| (s.name, s.fingerprint.dim))
        .collect();
    let mut stmt = conn
        .prepare("SELECT fact_id, space_id, embedding FROM fact_vectors ORDER BY space_id, fact_id")
        .map_err(StorageError::backend)?;
    let mut rows = stmt.query([]).map_err(StorageError::backend)?;
    while let Some(row) = rows.next().map_err(StorageError::backend)? {
        let fact_id: i64 = row.get(0).map_err(StorageError::backend)?;
        let space_id: String = row.get(1).map_err(StorageError::backend)?;
        let blob: Vec<u8> = row.get(2).map_err(StorageError::backend)?;
        let dim = space_dims.get(&space_id).copied().unwrap_or(embed_dim);
        let embedding = crate::store::deserialize_embedding(&blob, dim)?;
        cb(fact_id, space_id, embedding)?;
    }
    Ok(())
}

/// Count the vectors stored for `space_id`. Test-only inspection helper.
///
/// The
/// promote and dump paths read `fact_vectors` with their own queries; gated so it
/// never reaches the lib target as dead code. Un-gate if a production caller needs
/// a per-space count.
///
/// # Errors
///
/// Returns [`MemoryError::Storage`](me_types::error::MemoryError::Storage) on query
/// failure, or [`MemoryError::Internal`](me_types::error::MemoryError::Internal) if
/// the stored count is negative (impossible).
#[cfg(test)]
pub fn count_vectors(conn: &Connection, space_id: &str) -> Result<usize> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM fact_vectors WHERE space_id = ?1",
            params![space_id],
            |row| row.get(0),
        )
        .map_err(StorageError::backend)?;
    usize::try_from(n).map_err(|_| MemoryError::Internal("negative vector count".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::deserialize_embedding;
    use crate::store::embedding_spaces::{
        EmbeddingSpace, SpaceStatus, find_active, find_by_name, insert_active, insert_populating,
    };
    use crate::store::facts::FactStore;
    use crate::store::schema::{init_schema, open_memory};
    use me_types::types::{EmbeddingFingerprint, FactType, NewFact};

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

    /// Register the active space `default` (model `model`, dim `DIM`).
    fn set_active(conn: &Connection, model: &str) {
        insert_active(
            conn,
            &EmbeddingSpace::default_active(EmbeddingFingerprint::new(model, "tei", DIM)),
        )
        .expect("insert active space");
    }

    /// Backfill `ids` in `space` with the constant vector `[val; DIM]`.
    fn backfill_all(conn: &Connection, space: &str, ids: &[i64], val: f32) {
        let rows: Vec<(i64, Vec<f32>)> = ids.iter().map(|&id| (id, vec![val; DIM])).collect();
        write_backfill_batch(conn, space, &rows).expect("backfill");
    }

    /// Read a fact's stored *active* embedding (`facts.embedding`) back as a vector.
    fn read_embedding(conn: &Connection, id: i64) -> Vec<f32> {
        let blob: Vec<u8> = conn
            .query_row(
                "SELECT embedding FROM facts WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .expect("read embedding blob");
        deserialize_embedding(&blob, DIM).expect("deserialize")
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
        assert!(
            matches!(
                err,
                MemoryError::Storage(me_types::error::StorageError::Backend(_))
            ),
            "got {err:?}"
        );
    }

    // --- promote (#623 T3) ---

    #[test]
    fn promote_copy_swaps_and_flips_identity() {
        let conn = fresh_conn();
        set_active(&conn, "model-a"); // old active = "default" / model-a
        add_populating_space(&conn); // shadow = model-b, same dim
        let ids: Vec<i64> = (0..3)
            .map(|i| insert_fact(&conn, &format!("f{i}")))
            .collect();
        // Facts start with [0;DIM]; backfill the shadow with the distinct [1;DIM].
        backfill_all(&conn, SPACE, &ids, 1.0);

        let outcome = promote_space(&conn, SPACE).expect("promote");
        assert_eq!(outcome.promoted, 3);
        assert_eq!(outcome.deprecated_space, "default");
        assert_eq!(outcome.new_fingerprint.model, "model-b");
        assert!(outcome.rebuild_index);
        assert_eq!(outcome.stragglers_caught, 0);

        // The identity flipped: the shadow is now the single active space.
        let active = find_active(&conn).expect("find").expect("active");
        assert_eq!(active.name, SPACE);
        assert_eq!(active.fingerprint.model, "model-b");

        // facts.embedding now serves the new vectors.
        for &id in &ids {
            assert_eq!(read_embedding(&conn, id), vec![1.0_f32; DIM]);
        }
        // Cleanup: the populating rows are gone; the OLD vectors are retained
        // (keyed by the deprecated space) for an instant rollback.
        assert_eq!(count_vectors(&conn, SPACE).expect("shadow"), 0);
        assert_eq!(
            count_vectors(&conn, "default").expect("retained"),
            3,
            "old vectors retained for rollback"
        );
        assert_eq!(
            find_by_name(&conn, "default")
                .expect("find")
                .expect("present")
                .status,
            SpaceStatus::Deprecated
        );
    }

    #[test]
    fn promote_refuses_incomplete_populating() {
        let conn = fresh_conn();
        set_active(&conn, "model-a");
        add_populating_space(&conn);
        let ids: Vec<i64> = (0..3)
            .map(|i| insert_fact(&conn, &format!("f{i}")))
            .collect();
        backfill_all(&conn, SPACE, &ids[0..1], 1.0); // only 1 of 3 backfilled

        let err = promote_space(&conn, SPACE).expect_err("incomplete");
        assert!(matches!(err, MemoryError::Internal(_)), "got {err:?}");
        // No mutation: still one active = default, facts.embedding untouched.
        assert_eq!(
            find_active(&conn).expect("find").expect("active").name,
            "default"
        );
        assert_eq!(read_embedding(&conn, ids[0]), vec![0.0_f32; DIM]);
    }

    #[test]
    fn promote_allows_different_dim() {
        // #742 (inverts promote_rejects_different_dim): the storage promote is
        // dimension-agnostic — it copy-swaps DIM*2-wide vectors into facts.embedding,
        // flips identity, and retains the old DIM-wide vectors for rollback.
        let conn = fresh_conn();
        set_active(&conn, "model-a"); // active @ DIM; facts seeded @ DIM
        let a = insert_fact(&conn, "alpha");
        let b = insert_fact(&conn, "beta");
        insert_populating(
            &conn,
            &EmbeddingSpace {
                name: "wide".to_string(),
                fingerprint: EmbeddingFingerprint::new("model-wide", "tei", DIM * 2),
                status: SpaceStatus::Populating,
            },
        )
        .expect("insert wide");
        // Backfill the wide space with DIM*2-wide vectors.
        let wide_rows: Vec<(i64, Vec<f32>)> = [a, b]
            .iter()
            .map(|&id| (id, vec![0.9_f32; DIM * 2]))
            .collect();
        write_backfill_batch(&conn, "wide", &wide_rows).expect("backfill wide");

        let outcome = promote_space(&conn, "wide").expect("promote allows different dim");
        assert_eq!(outcome.promoted, 2);
        assert_eq!(outcome.new_fingerprint.dim, DIM * 2);

        // facts.embedding is now DIM*2-wide (the new space's vectors).
        for id in [a, b] {
            let blob: Vec<u8> = conn
                .query_row(
                    "SELECT embedding FROM facts WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .expect("blob");
            assert_eq!(blob.len(), DIM * 2 * 4, "served vector is now D*2-wide");
            assert_eq!(
                deserialize_embedding(&blob, DIM * 2).expect("deserialize"),
                vec![0.9_f32; DIM * 2]
            );
        }
        // Identity flipped to the wide space; the old "default" is retained @ DIM
        // for rollback, the populating rows cleaned up.
        let active = find_active(&conn).expect("find").expect("active");
        assert_eq!(active.name, "wide");
        assert_eq!(active.fingerprint.dim, DIM * 2);
        assert_eq!(
            count_vectors(&conn, "default").expect("retained"),
            2,
            "old DIM-wide vectors retained for rollback"
        );
        assert_eq!(
            count_vectors(&conn, "wide").expect("cleanup"),
            0,
            "populating rows deleted post-promote"
        );
    }

    #[test]
    fn promote_rolls_back_on_mid_tx_error() {
        let conn = fresh_conn();
        set_active(&conn, "model-a");
        add_populating_space(&conn);
        let ids: Vec<i64> = (0..2)
            .map(|i| insert_fact(&conn, &format!("f{i}")))
            .collect();
        backfill_all(&conn, SPACE, &ids, 1.0);

        // Inject a mid-tx failure on the copy-swap (step 3 updates facts.embedding).
        conn.execute_batch(
            "CREATE TRIGGER fail_swap BEFORE UPDATE OF embedding ON facts \
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .expect("install trigger");

        let err = promote_space(&conn, SPACE).expect_err("mid-tx failure");
        assert!(
            matches!(
                err,
                MemoryError::Storage(me_types::error::StorageError::Backend(_))
            ),
            "got {err:?}"
        );

        conn.execute_batch("DROP TRIGGER fail_swap;")
            .expect("drop trigger");

        // Full rollback (single-tx design): nothing moved.
        assert_eq!(
            find_active(&conn).expect("find").expect("active").name,
            "default",
            "old active intact"
        );
        for &id in &ids {
            assert_eq!(
                read_embedding(&conn, id),
                vec![0.0_f32; DIM],
                "embedding old"
            );
        }
        assert_eq!(
            count_vectors(&conn, SPACE).expect("shadow"),
            2,
            "populating vectors NOT deleted (step 5 rolled back)"
        );
        assert_eq!(
            count_vectors(&conn, "default").expect("retained"),
            0,
            "no old vectors retained (step 2 rolled back)"
        );
        assert_eq!(
            find_by_name(&conn, SPACE)
                .expect("find")
                .expect("present")
                .status,
            SpaceStatus::Populating,
            "shadow not activated"
        );
    }

    #[test]
    fn backfill_is_invisible_to_active_read_path() {
        // The pivot's core property: backfill writes ONLY fact_vectors, never
        // facts.embedding — so reads see the old vectors until promote commits.
        let conn = fresh_conn();
        set_active(&conn, "model-a");
        add_populating_space(&conn);
        let ids: Vec<i64> = (0..2)
            .map(|i| insert_fact(&conn, &format!("f{i}")))
            .collect();
        backfill_all(&conn, SPACE, &ids, 1.0);

        for &id in &ids {
            assert_eq!(read_embedding(&conn, id), vec![0.0_f32; DIM]);
        }
        assert_eq!(count_vectors(&conn, SPACE).expect("shadow"), 2);
    }

    #[test]
    fn promote_errors_when_populating_space_absent() {
        let conn = fresh_conn();
        set_active(&conn, "model-a");
        let err = promote_space(&conn, "ghost").expect_err("absent");
        assert!(matches!(err, MemoryError::Internal(_)), "got {err:?}");
    }

    #[test]
    fn for_each_decodes_each_space_at_its_own_dim() {
        // #742 (R6): a dump of a reconstructed store has a deprecated space at the
        // OLD dim while the active space (engine embed_dim) is the NEW dim. `for_each`
        // must decode each row at its space's own recorded dim — not the engine dim —
        // so a dump at the new dim still preserves the deprecated rollback vectors.
        let conn = fresh_conn();
        // Active space at the NEW (wide) dim.
        insert_active(
            &conn,
            &EmbeddingSpace::default_active(EmbeddingFingerprint::new("new", "tei", DIM * 2)),
        )
        .expect("active wide");
        // A deprecated space at the OLD dim, holding old-dim retained vectors.
        insert_active(
            &conn,
            &EmbeddingSpace {
                name: "old".to_string(),
                fingerprint: EmbeddingFingerprint::new("old", "tei", DIM),
                status: SpaceStatus::Deprecated,
            },
        )
        .expect("deprecated old");
        let a = insert_fact(&conn, "alpha");
        let b = insert_fact(&conn, "beta");
        write_backfill_batch(
            &conn,
            "old",
            &[(a, vec![0.3_f32; DIM]), (b, vec![0.3_f32; DIM])],
        )
        .expect("write old-dim vectors");

        // for_each at the ACTIVE (wide) embed_dim must still decode the old DIM-wide rows.
        let mut seen: Vec<(i64, String, Vec<f32>)> = Vec::new();
        for_each(&conn, DIM * 2, |fid, space, emb| {
            seen.push((fid, space, emb));
            Ok(())
        })
        .expect("for_each across heterogeneous dims");

        assert_eq!(seen.len(), 2);
        for (_, space, emb) in &seen {
            assert_eq!(space, "old");
            assert_eq!(
                emb.len(),
                DIM,
                "decoded at the space's own dim, not the engine dim"
            );
            assert_eq!(emb, &vec![0.3_f32; DIM]);
        }
    }
}
