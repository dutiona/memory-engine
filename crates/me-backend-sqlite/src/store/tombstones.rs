//! The forget retraction's blocks (#1023): the `fact_tombstones` table.
//!
//! One row per forgotten fact, keyed for the gate by the content hash
//! ([`content_hash`](super::facts::content_hash)) and holding no text, so it outlives an
//! erasure of its fact. Every function takes the caller's connection: the writes run inside
//! the caller's transaction.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

use me_types::error::{Result, StorageError};
use me_types::types::{EventType, ForgetOutcome, NewEvent, Tombstone};

use super::events::EventStore;
use super::facts::content_hash;
use super::{UpcasterRegistry, parse_optional_timestamp, parse_timestamp};

/// Whether a tombstone blocks `content`, in any scope.
///
/// # Errors
///
/// `MemoryError::Storage` on SQL failure.
pub fn blocks(conn: &Connection, content: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM fact_tombstones WHERE content_hash = ?1)",
        params![content_hash(content)],
        |r| r.get(0),
    )
    .map_err(|e| StorageError::backend(e).into())
}

/// The tombstone that names fact `fact_id`, if any.
///
/// # Errors
///
/// `MemoryError::Storage` on SQL failure.
pub fn get(conn: &Connection, fact_id: i64) -> Result<Option<Tombstone>> {
    conn.query_row(
        "SELECT fact_id, content_hash, reason, forgotten_at, record_event_id, prior_t_expired,
                prior_t_invalid
         FROM fact_tombstones WHERE fact_id = ?1",
        params![fact_id],
        row_to_tombstone,
    )
    .optional()
    .map_err(|e| StorageError::backend(e).into())
}

/// Every tombstone, in id order (the JSON export).
///
/// # Errors
///
/// `MemoryError::Storage` on SQL failure.
pub fn list_all(conn: &Connection) -> Result<Vec<Tombstone>> {
    let mut stmt = conn
        .prepare(
            "SELECT fact_id, content_hash, reason, forgotten_at, record_event_id, prior_t_expired,
                    prior_t_invalid
             FROM fact_tombstones ORDER BY id",
        )
        .map_err(StorageError::backend)?;
    stmt.query_map([], row_to_tombstone)
        .map_err(StorageError::backend)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| StorageError::backend(e).into())
}

fn row_to_tombstone(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tombstone> {
    let forgotten_at: String = row.get(3)?;
    let prior_t_expired: Option<String> = row.get(5)?;
    let prior_t_invalid: Option<String> = row.get(6)?;
    Ok(Tombstone {
        fact_id: row.get(0)?,
        content_hash: row.get(1)?,
        reason: row.get(2)?,
        forgotten_at: parse_timestamp(&forgotten_at)?,
        record_event_id: row.get(4)?,
        prior_t_expired: parse_optional_timestamp(prior_t_expired.as_deref())?,
        prior_t_invalid: parse_optional_timestamp(prior_t_invalid.as_deref())?,
    })
}

/// Retract fact `id` inside `homes`, with `record` the forget's call record, per
/// [`Retraction::forget_fact`](me_storage::Retraction::forget_fact). Returns the outcome
/// and the ids that left the active set (for the vector index).
///
/// # Errors
///
/// `MemoryError::Storage` on SQL failure.
pub fn forget(
    conn: &Connection,
    id: i64,
    homes: &[i64],
    reason: &str,
    now: DateTime<Utc>,
    record: i64,
) -> Result<(ForgetOutcome, Vec<i64>)> {
    let homes_json = serde_json::to_string(homes).map_err(StorageError::backend)?;
    let content: Option<String> = conn
        .query_row(
            "SELECT content FROM facts
             WHERE id = ?1 AND scope_id IN (SELECT value FROM json_each(?2))",
            params![id, homes_json],
            |r| r.get(0),
        )
        .optional()
        .map_err(StorageError::backend)?;
    let Some(content) = content else {
        return Ok((ForgetOutcome::NotFound, Vec::new()));
    };
    let named: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM fact_tombstones WHERE fact_id = ?1)",
            params![id],
            |r| r.get(0),
        )
        .map_err(StorageError::backend)?;
    if named {
        return Ok((ForgetOutcome::AlreadyForgotten, Vec::new()));
    }
    // The named fact, whatever its state, and its active identical copies in the Homes. The
    // copies match on the text itself: a restore copies a stored hash verbatim.
    let mut stmt = conn
        .prepare(
            "SELECT id, t_expired IS NULL FROM facts
             WHERE id = ?1
                OR (content = ?2 AND t_expired IS NULL
                    AND scope_id IN (SELECT value FROM json_each(?3))
                    AND id NOT IN (SELECT fact_id FROM fact_tombstones WHERE fact_id IS NOT NULL))
             ORDER BY id",
        )
        .map_err(StorageError::backend)?;
    let targets = stmt
        .query_map(params![id, content, homes_json], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
        })
        .map_err(StorageError::backend)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::backend)?;
    let hash = content_hash(&content);
    let now = now.to_rfc3339();
    let mut left_active = Vec::new();
    for (fact, active) in targets {
        conn.execute(
            "INSERT INTO fact_tombstones (fact_id, content_hash, reason, forgotten_at,
                                          record_event_id, prior_t_expired, prior_t_invalid)
             SELECT id, ?2, ?3, ?4, ?5, t_expired, t_invalid FROM facts WHERE id = ?1",
            params![fact, hash, reason, now, record],
        )
        .map_err(StorageError::backend)?;
        conn.execute(
            "UPDATE facts SET t_expired = coalesce(t_expired, ?2),
                              t_invalid = min(coalesce(t_invalid, ?2), ?2)
             WHERE id = ?1",
            params![fact, now],
        )
        .map_err(StorageError::backend)?;
        if active {
            left_active.push(fact);
        }
    }
    Ok((ForgetOutcome::Forgotten, left_active))
}

/// Reverse the forget that named fact `id`, per
/// [`Retraction::unforget_fact`](me_storage::Retraction::unforget_fact). Returns `None`
/// when no tombstone names it, else every restored fact with whether recall shows it again.
///
/// # Errors
///
/// `MemoryError::Storage` on SQL failure.
pub fn unforget(
    conn: &Connection,
    registry: &UpcasterRegistry,
    id: i64,
    now: DateTime<Utc>,
) -> Result<Option<Vec<(i64, bool)>>> {
    let Some(named) = get(conn, id)? else {
        return Ok(None);
    };
    // One forget's rows share its record; a row without one reverses its own fact only. An
    // erased copy (no fact) keeps its block.
    let mut stmt = conn
        .prepare(
            "SELECT fact_id, prior_t_expired, prior_t_invalid FROM fact_tombstones
             WHERE fact_id IS NOT NULL
               AND (fact_id = ?1 OR (?2 IS NOT NULL AND record_event_id = ?2))
             ORDER BY fact_id",
        )
        .map_err(StorageError::backend)?;
    let rows = stmt
        .query_map(params![id, named.record_event_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(StorageError::backend)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::backend)?;
    let mut restored = Vec::with_capacity(rows.len());
    for (fact, prior_expired, prior_invalid) in rows {
        conn.execute(
            "UPDATE facts SET t_expired = ?2, t_invalid = ?3 WHERE id = ?1",
            params![fact, prior_expired, prior_invalid],
        )
        .map_err(StorageError::backend)?;
        conn.execute(
            "DELETE FROM fact_tombstones WHERE fact_id = ?1",
            params![fact],
        )
        .map_err(StorageError::backend)?;
        let invalid_later = parse_optional_timestamp(prior_invalid.as_deref())
            .map_err(StorageError::backend)?
            .is_none_or(|end| end > now);
        restored.push((fact, prior_expired.is_none() && invalid_later));
    }
    // A restored fact sits below dedup's watermark (#1080): rewind so it is compared again.
    if let Some(&(lowest, _)) = restored.first() {
        crate::store::facts::rewind_dedup_watermark(conn, lowest)?;
    }
    append_maintenance(
        conn,
        registry,
        json!({
            "op": "unforget",
            "fact_ids": restored.iter().map(|(fact, _)| *fact).collect::<Vec<_>>(),
            "forget_record": named.record_event_id,
        }),
        now,
    )?;
    Ok(Some(restored))
}

/// Append a maintenance record (`unforget`, `erase`): a `SystemEvent` at the root, source
/// `maintenance`, attributed to no machine (no top-level `machine` key). Returns its id.
///
/// # Errors
///
/// `MemoryError::Storage` on SQL failure.
pub fn append_maintenance(
    conn: &Connection,
    registry: &UpcasterRegistry,
    payload: serde_json::Value,
    now: DateTime<Utc>,
) -> Result<i64> {
    EventStore::new(conn, registry).insert(&NewEvent {
        timestamp: now,
        event_type: EventType::SystemEvent,
        payload,
        source: "maintenance".to_owned(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "local".to_owned(),
        sequence_id: 0,
        created_at: None,
    })
}
