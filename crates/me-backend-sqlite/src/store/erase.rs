//! Erasure (#1023): the store's one hard delete, as one transaction's statements.
//!
//! [`erase`] is the function #1025, #1026 and #1029 extend with the tables they add:
//! select what the target owns, then delete children before parents, each adding its count
//! to `deleted`. Every id set is bound as one JSON array, so no erasure meets SQLite's
//! bound-variable cap.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde_json::json;

use me_types::error::{ConflictError, MemoryError, Result, StorageError};
use me_types::limits::check_json_size;
use me_types::types::{EraseReport, EraseTarget, is_dns_label, machine_bucket};

use super::UpcasterRegistry;
use super::edges::EdgeStore;
use super::facts::FactStore;
use super::tombstones::append_maintenance;

fn ids(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql).map_err(StorageError::backend)?;
    stmt.query_map(params, |r| r.get(0))
        .map_err(StorageError::backend)?
        .collect::<rusqlite::Result<Vec<i64>>>()
        .map_err(|e| StorageError::backend(e).into())
}

fn json_ids(ids: &[i64]) -> Result<String> {
    serde_json::to_string(ids).map_err(|e| StorageError::backend(e).into())
}

fn exec(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
    conn.execute(sql, params)
        .map_err(|e| StorageError::backend(e).into())
}

/// What an erasure takes: the bucket's path and subtree (by machine), the machine's events,
/// and every fact it deletes, wisdom closure included.
struct Selection {
    bucket: Option<String>,
    subtree: Vec<i64>,
    events: Vec<i64>,
    taken: Vec<i64>,
}

/// Refuse a target no erasure can mean, before anything is touched.
pub(crate) fn validate(target: &EraseTarget) -> Result<()> {
    match target {
        EraseTarget::Machine(machine) if !is_dns_label(machine.as_bytes()) => {
            Err(MemoryError::Conflict(ConflictError::PolicyParameter(
                format!("a machine is a lowercase DNS label: {machine:?}"),
            )))
        }
        _ => Ok(()),
    }
}

fn select(conn: &Connection, target: &EraseTarget) -> Result<Selection> {
    // The bucket subtree, the machine's events and the facts seeding the erasure.
    let (bucket, subtree, events, seed) = match target {
        EraseTarget::Fact(id) => (
            None,
            Vec::new(),
            Vec::new(),
            ids(conn, "SELECT id FROM facts WHERE id = ?1", params![id])?,
        ),
        EraseTarget::Machine(machine) => {
            let bucket = machine_bucket(machine);
            let subtree = ids(
                conn,
                "WITH RECURSIVE sub(id) AS (
                     SELECT id FROM scopes WHERE parent_id = 1 AND label = ?1
                     UNION SELECT s.id FROM scopes s JOIN sub ON s.parent_id = sub.id)
                 SELECT id FROM sub ORDER BY id",
                params![bucket],
            )?;
            let subtree_json = json_ids(&subtree)?;
            let events = ids(
                conn,
                "SELECT id FROM events
                 WHERE scope_id IN (SELECT value FROM json_each(?1))
                    OR CASE WHEN json_valid(payload)
                            THEN json_extract(payload, '$.machine') END = ?2
                 ORDER BY id",
                params![subtree_json, machine],
            )?;
            let seed = ids(
                conn,
                "SELECT id FROM facts
                 WHERE scope_id IN (SELECT value FROM json_each(?1))
                    OR json_extract(metadata, '$.machine') = ?2
                    OR source_event_id IN (SELECT value FROM json_each(?3))
                 ORDER BY id",
                params![subtree_json, machine, json_ids(&events)?],
            )?;
            (Some(bucket), subtree, events, seed)
        }
    };
    // Every wisdom fact derived from a taken fact, transitively.
    let taken = ids(
        conn,
        "WITH RECURSIVE taken(id) AS (
             SELECT value FROM json_each(?1)
             UNION
             SELECT l.wisdom_fact_id FROM lineage l, json_each(l.source_fact_ids) j, taken t
             WHERE j.value = t.id)
         SELECT id FROM taken ORDER BY id",
        params![json_ids(&seed)?],
    )?;
    Ok(Selection {
        bucket,
        subtree,
        events,
        taken,
    })
}

/// Delete what `sel` takes, children first, and count each kind.
fn delete(conn: &Connection, dim: usize, sel: &Selection) -> Result<BTreeMap<String, usize>> {
    let taken_json = json_ids(&sel.taken)?;
    // A tombstone keeps its hash: the block outlives its fact.
    exec(
        conn,
        "UPDATE fact_tombstones SET reason = NULL, prior_t_expired = NULL, prior_t_invalid = NULL
         WHERE fact_id IN (SELECT value FROM json_each(?1))",
        params![taken_json],
    )?;
    let summaries = exec(
        conn,
        "DELETE FROM summaries WHERE EXISTS (
             SELECT 1 FROM json_each(CASE WHEN json_valid(summaries.source_fact_ids)
                                          THEN summaries.source_fact_ids ELSE '[]' END) j
             WHERE j.value IN (SELECT value FROM json_each(?1)))",
        params![taken_json],
    )?;
    let edges = EdgeStore::new(conn).hard_delete_incident_to_facts(&sel.taken)?;
    let activities = exec(
        conn,
        "DELETE FROM activities
         WHERE promoted_fact_id IN (SELECT value FROM json_each(?1))
            OR scope_id IN (SELECT value FROM json_each(?2))",
        params![taken_json, json_ids(&sel.subtree)?],
    )?;
    let checkpoints = match &sel.bucket {
        Some(bucket) => exec(
            conn,
            "DELETE FROM session_checkpoints
             WHERE scope_path = ?1 OR substr(scope_path, 1, length(?1) + 1) = ?1 || '/'",
            params![bucket],
        )?,
        None => 0,
    };
    super::embed_queue::forget_errors_of(conn, &sel.taken)?;
    let facts = FactStore::new(conn, dim).hard_delete_ids(&sel.taken)?;
    let events = exec(
        conn,
        "DELETE FROM events WHERE id IN (SELECT value FROM json_each(?1))",
        params![json_ids(&sel.events)?],
    )?;
    // Drop the deleted documents' tokens from the older full-text segments.
    exec(
        conn,
        "INSERT INTO facts_fts(facts_fts) VALUES ('optimize')",
        [],
    )?;
    Ok([
        ("activities", activities),
        ("checkpoints", checkpoints),
        ("edges", edges),
        ("events", events),
        ("facts", facts),
        ("summaries", summaries),
    ]
    .into_iter()
    .map(|(kind, n)| (kind.to_owned(), n))
    .collect())
}

/// Erase `target` per [`Retraction::erase`](me_storage::Retraction::erase), inside the
/// caller's transaction, without the post-commit purge. Returns the report with
/// `purged == false`.
///
/// # Errors
///
/// `Conflict(PolicyParameter)` for an invalid machine name, `Conflict(PayloadTooLarge)`
/// for an oversized record, `MemoryError::Storage` on SQL failure.
pub fn erase(
    conn: &Connection,
    registry: &UpcasterRegistry,
    target: &EraseTarget,
    dim: usize,
    now: DateTime<Utc>,
) -> Result<EraseReport> {
    validate(target)?;
    let sel = select(conn, target)?;
    let taken_json = json_ids(&sel.taken)?;
    // What the erasure leaves behind, read before any edge goes.
    let orphaned = ids(
        conn,
        "SELECT DISTINCT e.target_fact_id FROM edges e JOIN facts f ON f.id = e.target_fact_id
         WHERE e.source_fact_id IN (SELECT value FROM json_each(?1))
           AND e.target_fact_id NOT IN (SELECT value FROM json_each(?1))
           AND e.relation_type IN ('supersedes', 'contradicts')
           AND f.t_expired IS NOT NULL
         ORDER BY e.target_fact_id",
        params![taken_json],
    )?;
    let copies = ids(
        conn,
        "SELECT id FROM facts
         WHERE id NOT IN (SELECT value FROM json_each(?1))
           AND content IN (SELECT content FROM facts
                           WHERE id IN (SELECT value FROM json_each(?1)))
         ORDER BY id",
        params![taken_json],
    )?;
    // The #1020 link: each erased fact whose source event survives.
    let mut stmt = conn
        .prepare(
            "SELECT id, source_event_id FROM facts
             WHERE id IN (SELECT value FROM json_each(?1)) AND source_event_id IS NOT NULL
               AND source_event_id NOT IN (SELECT value FROM json_each(?2))
             ORDER BY id",
        )
        .map_err(StorageError::backend)?;
    let linked = stmt
        .query_map(params![taken_json, json_ids(&sel.events)?], |r| {
            Ok(json!({ "fact": r.get::<_, i64>(0)?, "source_event": r.get::<_, i64>(1)? }))
        })
        .map_err(StorageError::backend)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::backend)?;

    let deleted = delete(conn, dim, &sel)?;
    if deleted.values().any(|n| *n > 0) {
        let payload = json!({
            "op": "erase",
            "target": target,
            "linked": linked,
            "orphaned": orphaned.len(),
            "copies": copies.len(),
            "deleted": deleted,
        });
        check_json_size(&payload, "erase record")?;
        append_maintenance(conn, registry, payload, now)?;
    }
    Ok(EraseReport {
        target: target.clone(),
        deleted,
        erased_facts: sel.taken,
        orphaned,
        copies,
        purged: false,
        purge_error: None,
    })
}
