//! The deferred-embedding queue (#1021): a fact stored before its vector exists holds
//! `embedding = X''` and one `embed_queue` row until the drain fills the vector.
//!
//! Queue-wide state lives in `config` keys, named here once.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::store::facts::FactStore;
use crate::store::schema::{get_config, set_config};
use crate::store::{
    embedding_meta, parse_optional_timestamp, parse_timestamp, serialize_embedding,
};
use me_types::error::{MemoryError, Result, StorageError};
use me_types::types::{
    EmbedQueueStatus, EmbeddingFailure, EmbeddingFingerprint, NewFact, NextEmbedding,
    PendingEmbedding,
};

/// When the queue was first paused by a rejected key; absent when not paused.
pub const PAUSED_SINCE: &str = "embed_queue_paused_since";
/// The latest rejected-key error while paused.
pub const PAUSE_ERROR: &str = "embed_queue_pause_error";
/// The fact whose attempt left [`PAUSE_ERROR`]; erasing it drops that text.
pub const PAUSE_ERROR_FACT: &str = "embed_queue_pause_error_fact_id";
/// When an embed last succeeded.
pub const LAST_SUCCESS_AT: &str = "embed_last_success_at";
/// When an embed last failed, whatever the failure.
pub const LAST_ERROR_AT: &str = "embed_last_error_at";
/// The last embed failure's error.
pub const LAST_ERROR: &str = "embed_last_error";
/// The fact whose attempt left [`LAST_ERROR`]; erasing it drops that text.
pub const LAST_ERROR_FACT: &str = "embed_last_error_fact_id";

/// Insert `fact` without a vector and queue it, due at `now`, in one transaction; a store
/// with no identity yet records `embed_dim` as its dimension.
///
/// # Errors
///
/// Returns `MemoryError::EmbeddingDimension` if `fact.embedding` is not empty, or
/// `MemoryError::Storage` on a write failure; nothing is written on any error.
pub fn insert_deferred(
    conn: &Connection,
    fact: &NewFact,
    embed_dim: usize,
    now: DateTime<Utc>,
) -> Result<i64> {
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    let id = FactStore::new(&tx, embed_dim).insert_without_vector(fact)?;
    tx.execute(
        "INSERT INTO embed_queue (fact_id, enqueued_at, next_attempt_at) VALUES (?1, ?2, ?2)",
        params![id, now.to_rfc3339()],
    )
    .map_err(StorageError::backend)?;
    embedding_meta::record_dim_if_unknown(&tx, embed_dim)?;
    tx.commit().map_err(StorageError::backend)?;
    Ok(id)
}

/// The waiting item the drain takes next: the smallest `(next_attempt_at, fact_id)` among
/// the items not failed, `Due` once its time is `now` or earlier.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on a query failure or an unparseable timestamp.
pub fn next(conn: &Connection, now: DateTime<Utc>) -> Result<NextEmbedding> {
    let row = conn
        .query_row(
            "SELECT q.fact_id, f.content, q.attempts, q.next_attempt_at
               FROM embed_queue q JOIN facts f ON f.id = q.fact_id
              WHERE q.failed_at IS NULL
              ORDER BY q.next_attempt_at, q.fact_id LIMIT 1",
            [],
            |r| {
                Ok((
                    PendingEmbedding {
                        fact_id: r.get(0)?,
                        content: r.get(1)?,
                        attempts: u32::try_from(r.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                    },
                    parse_timestamp(&r.get::<_, String>(3)?)?,
                ))
            },
        )
        .optional()
        .map_err(StorageError::backend)?;
    Ok(match row {
        None => NextEmbedding::Idle,
        Some((item, at)) if at <= now => NextEmbedding::Due(item),
        Some((_, at)) => NextEmbedding::NotBefore(at),
    })
}

/// Fill fact `fact_id`'s vector and drop its queue row, in one transaction; returns
/// whether a vector was written on an active fact (the caller indexes it).
///
/// The vector is never written over another: a fact that already has one, or is gone,
/// only loses its queue row, and then no identity is recorded. Records the success time
/// and clears the pause.
///
/// # Errors
///
/// Returns `MemoryError::EmbeddingDimension` if `embedding` is not `expected_dim` wide,
/// `MemoryError::EmbeddingModelMismatch` if `fingerprint` differs from the recorded
/// identity, or `MemoryError::Storage` on a write failure; nothing is written on any
/// error.
pub fn complete(
    conn: &Connection,
    fact_id: i64,
    embedding: &[f32],
    fingerprint: &EmbeddingFingerprint,
    expected_dim: usize,
    now: DateTime<Utc>,
) -> Result<bool> {
    if embedding.len() != expected_dim {
        return Err(MemoryError::EmbeddingDimension {
            expected: expected_dim,
            actual: embedding.len(),
        });
    }
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    let written_active: Option<bool> = tx
        .query_row(
            "UPDATE facts SET embedding = ?1 WHERE id = ?2 AND length(embedding) = 0
             RETURNING t_expired IS NULL",
            params![serialize_embedding(embedding), fact_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(StorageError::backend)?;
    if written_active.is_some() {
        embedding_meta::record_if_absent(&tx, fingerprint, expected_dim)?;
    }
    tx.execute("DELETE FROM embed_queue WHERE fact_id = ?1", [fact_id])
        .map_err(StorageError::backend)?;
    set_config(&tx, LAST_SUCCESS_AT, &now.to_rfc3339())?;
    clear_pause(&tx)?;
    tx.commit().map_err(StorageError::backend)?;
    Ok(written_active == Some(true))
}

/// Record a failed attempt on fact `fact_id`, in one transaction.
///
/// `Retry` and `Fail` count the attempt and keep `error` on the row; `Fail`, and `Retry` when
/// the embedder answered, clear the pause. `Hold` changes no queue row; paused, it keeps the first pause
/// time and the latest `error`. Every failure records its time, `error` and `fact_id`.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on a write failure; nothing is written on any error.
pub fn record_failure(
    conn: &Connection,
    fact_id: i64,
    error: &str,
    failure: EmbeddingFailure,
    now: DateTime<Utc>,
) -> Result<()> {
    let now = now.to_rfc3339();
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    match failure {
        EmbeddingFailure::Retry { at, answered } => {
            tx.execute(
                "UPDATE embed_queue SET attempts = attempts + 1, next_attempt_at = ?2,
                        last_error = ?3
                  WHERE fact_id = ?1",
                params![fact_id, at.to_rfc3339(), error],
            )
            .map_err(StorageError::backend)?;
            if answered {
                clear_pause(&tx)?;
            }
        }
        EmbeddingFailure::Fail => {
            tx.execute(
                "UPDATE embed_queue SET attempts = attempts + 1, failed_at = ?2, last_error = ?3
                  WHERE fact_id = ?1",
                params![fact_id, now, error],
            )
            .map_err(StorageError::backend)?;
            clear_pause(&tx)?;
        }
        EmbeddingFailure::Hold { paused: true } => {
            tx.execute(
                "INSERT OR IGNORE INTO config (key, value) VALUES (?1, ?2)",
                params![PAUSED_SINCE, now],
            )
            .map_err(StorageError::backend)?;
            set_config(&tx, PAUSE_ERROR, error)?;
            set_config(&tx, PAUSE_ERROR_FACT, &fact_id.to_string())?;
        }
        EmbeddingFailure::Hold { paused: false } => {}
    }
    set_config(&tx, LAST_ERROR_AT, &now)?;
    set_config(&tx, LAST_ERROR, error)?;
    set_config(&tx, LAST_ERROR_FACT, &fact_id.to_string())?;
    tx.commit().map_err(StorageError::backend)?;
    Ok(())
}

/// Drop the queue-wide error texts an attempt on an `erased` fact left (#1023).
///
/// An embedder may echo its input in its error body. The times stay: the failure happened,
/// and a pause still holds, since a rejected key is no fact's doing.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on a query or write failure.
pub fn forget_errors_of(conn: &Connection, erased: &[i64]) -> Result<()> {
    for (text, fact) in [
        (LAST_ERROR, LAST_ERROR_FACT),
        (PAUSE_ERROR, PAUSE_ERROR_FACT),
    ] {
        let left_by = get_config(conn, fact)?.and_then(|id| id.parse::<i64>().ok());
        if left_by.is_some_and(|id| erased.contains(&id)) {
            conn.execute(
                "DELETE FROM config WHERE key IN (?1, ?2)",
                params![text, fact],
            )
            .map_err(StorageError::backend)?;
        }
    }
    Ok(())
}

/// The queue as `status` reports it: counts, the oldest waiting item, and the queue-wide
/// state. Reads only, so it runs on a read-only connection.
///
/// # Errors
///
/// Returns `MemoryError::Storage` on a query failure or an unparseable timestamp.
pub fn read_status(conn: &Connection) -> Result<EmbedQueueStatus> {
    let (waiting, oldest, failed): (i64, Option<String>, i64) = conn
        .query_row(
            "SELECT COUNT(*) FILTER (WHERE failed_at IS NULL),
                    MIN(enqueued_at) FILTER (WHERE failed_at IS NULL),
                    COUNT(*) FILTER (WHERE failed_at IS NOT NULL)
               FROM embed_queue",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(StorageError::backend)?;
    let time = |key: &str| -> Result<Option<DateTime<Utc>>> {
        parse_optional_timestamp(get_config(conn, key)?.as_deref())
            .map_err(|e| StorageError::backend(e).into())
    };
    Ok(EmbedQueueStatus {
        waiting: u64::try_from(waiting).unwrap_or(0),
        oldest_enqueued_at: parse_optional_timestamp(oldest.as_deref())
            .map_err(StorageError::backend)?,
        failed: u64::try_from(failed).unwrap_or(0),
        paused_since: time(PAUSED_SINCE)?,
        pause_error: get_config(conn, PAUSE_ERROR)?,
        last_success_at: time(LAST_SUCCESS_AT)?,
        last_error_at: time(LAST_ERROR_AT)?,
        last_error: get_config(conn, LAST_ERROR)?,
    })
}

fn clear_pause(tx: &Transaction) -> Result<()> {
    tx.execute(
        "DELETE FROM config WHERE key IN (?1, ?2, ?3)",
        params![PAUSED_SINCE, PAUSE_ERROR, PAUSE_ERROR_FACT],
    )
    .map_err(StorageError::backend)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{seed_without_vector, setup_memory_db};

    #[test]
    fn next_takes_the_earliest_due_item_and_skips_a_failed_one() {
        let conn = setup_memory_db();
        let failed = seed_without_vector(&conn, "failed");
        let later = seed_without_vector(&conn, "later");
        let first = seed_without_vector(&conn, "first");
        let tie = seed_without_vector(&conn, "tie");
        let t = |s: &str| format!("2026-01-01T00:00:0{s}+00:00");
        for (id, at) in [
            (failed, t("0")),
            (later, t("5")),
            (first, t("1")),
            (tie, t("1")),
        ] {
            conn.execute(
                "UPDATE embed_queue SET next_attempt_at = ?2 WHERE fact_id = ?1",
                params![id, at],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE embed_queue SET failed_at = ?2 WHERE fact_id = ?1",
            params![failed, t("0")],
        )
        .unwrap();
        let at = |s: &str| parse_timestamp(&t(s)).unwrap();

        let NextEmbedding::Due(item) = next(&conn, at("1")).unwrap() else {
            panic!("an item is due");
        };
        assert_eq!(
            item,
            PendingEmbedding {
                fact_id: first,
                content: "first".into(),
                attempts: 0
            },
            "by time, then by id"
        );
        assert_eq!(
            next(&conn, at("0")).unwrap(),
            NextEmbedding::NotBefore(at("1"))
        );
        conn.execute("DELETE FROM embed_queue WHERE fact_id <> ?1", [failed])
            .unwrap();
        assert_eq!(next(&conn, at("9")).unwrap(), NextEmbedding::Idle);
    }
}
