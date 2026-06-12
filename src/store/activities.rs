//! Store operations for activity records.

use rusqlite::{Connection, params};

use crate::error::{MemoryError, Result};
use crate::types::{Activity, ActivityStatus, NewActivity};

use super::parse_timestamp;

/// Store facade for the `activities` table.
pub struct ActivityStore<'a> {
    conn: &'a Connection,
}

impl<'a> ActivityStore<'a> {
    pub const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a new activity or deduplicate against an existing one.
    ///
    /// Dedup key: `(session_id, tool_name, args_hash, outcome_class)` within
    /// `dedup_window_secs` of the most recent matching activity.
    ///
    /// Returns `(id, was_deduplicated)`.
    pub fn insert_or_dedup(
        &self,
        activity: &NewActivity,
        dedup_window_secs: i64,
    ) -> Result<(i64, bool)> {
        let ts = activity.timestamp.to_rfc3339();

        // Check for existing activity within the dedup window.
        let existing: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, occurrence_count FROM activities
                 WHERE session_id = ?1
                   AND tool_name = ?2
                   AND args_hash = ?3
                   AND outcome_class = ?4
                   AND scope_id = ?5
                   AND status IN ('recorded', 'deduplicated', 'promoted')
                   AND julianday(?6) - julianday(last_seen) < ?7 / 86400.0
                 ORDER BY last_seen DESC
                 LIMIT 1",
                params![
                    activity.session_id,
                    activity.tool_name,
                    activity.args_hash,
                    activity.outcome_class,
                    activity.scope_id,
                    ts,
                    dedup_window_secs,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(MemoryError::Database)?;

        if let Some((existing_id, count)) = existing {
            self.conn
                .execute(
                    "UPDATE activities
                     SET occurrence_count = ?1,
                         last_seen = ?2,
                         status = CASE WHEN status = 'promoted' THEN 'promoted' ELSE 'deduplicated' END,
                         result_summary = COALESCE(?3, result_summary)
                     WHERE id = ?4",
                    params![count + 1, ts, activity.result_summary, existing_id],
                )
                .map_err(MemoryError::Database)?;
            return Ok((existing_id, true));
        }

        // Insert new activity.
        self.conn
            .execute(
                "INSERT INTO activities
                 (session_id, tool_name, args_hash, args, result_summary,
                  outcome_class, status, occurrence_count, first_seen, last_seen, scope_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'recorded', 1, ?7, ?7, ?8)",
                params![
                    activity.session_id,
                    activity.tool_name,
                    activity.args_hash,
                    activity.args.to_string(),
                    activity.result_summary,
                    activity.outcome_class,
                    ts,
                    activity.scope_id,
                ],
            )
            .map_err(MemoryError::Database)?;

        let id = self.conn.last_insert_rowid();
        Ok((id, false))
    }

    /// Get an activity by id.
    pub fn get(&self, id: i64) -> Result<Activity> {
        self.conn
            .query_row(
                "SELECT id, session_id, tool_name, args_hash, args, result_summary,
                        outcome_class, status, occurrence_count, first_seen, last_seen,
                        scope_id, promoted_fact_id
                 FROM activities WHERE id = ?1",
                params![id],
                row_to_activity,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    MemoryError::NotFound(format!("activity {id}"))
                }
                other => MemoryError::Database(other),
            })
    }

    /// List activities for a session, most recent first.
    pub fn list_by_session(&self, session_id: &str, limit: Option<usize>) -> Result<Vec<Activity>> {
        let sql = match limit {
            Some(n) => format!(
                "SELECT id, session_id, tool_name, args_hash, args, result_summary,
                        outcome_class, status, occurrence_count, first_seen, last_seen,
                        scope_id, promoted_fact_id
                 FROM activities
                 WHERE session_id = ?1
                 ORDER BY last_seen DESC
                 LIMIT {n}"
            ),
            None => "SELECT id, session_id, tool_name, args_hash, args, result_summary,
                            outcome_class, status, occurrence_count, first_seen, last_seen,
                            scope_id, promoted_fact_id
                     FROM activities
                     WHERE session_id = ?1
                     ORDER BY last_seen DESC"
                .to_string(),
        };
        let mut stmt = self.conn.prepare(&sql).map_err(MemoryError::Database)?;
        let rows = stmt
            .query_map(params![session_id], row_to_activity)
            .map_err(MemoryError::Database)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(MemoryError::Database)
    }

    /// List recent activities for a set of scope IDs, most recent first.
    ///
    /// Uses `idx_activities_scope_recent` for efficient lookup.
    pub fn list_recent_by_scope(&self, scope_ids: &[i64], limit: usize) -> Result<Vec<Activity>> {
        if scope_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(scope_ids).map_err(|e| {
            MemoryError::Database(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, session_id, tool_name, args_hash, args, result_summary,
                        outcome_class, status, occurrence_count, first_seen, last_seen,
                        scope_id, promoted_fact_id
                 FROM activities
                 WHERE scope_id IN (SELECT value FROM json_each(?1))
                 ORDER BY last_seen DESC
                 LIMIT ?2",
            )
            .map_err(MemoryError::Database)?;
        let rows = stmt
            .query_map(params![ids_json, limit], row_to_activity)
            .map_err(MemoryError::Database)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(MemoryError::Database)
    }

    /// Update the status of an activity and optionally link to a promoted fact.
    pub fn update_status(
        &self,
        id: i64,
        status: ActivityStatus,
        promoted_fact_id: Option<i64>,
    ) -> Result<()> {
        let rows = self
            .conn
            .execute(
                "UPDATE activities SET status = ?1, promoted_fact_id = ?2 WHERE id = ?3",
                params![status.to_string(), promoted_fact_id, id],
            )
            .map_err(MemoryError::Database)?;
        if rows == 0 {
            return Err(MemoryError::NotFound(format!("activity {id}")));
        }
        Ok(())
    }

    /// Count activities in a session.
    pub fn count_by_session(&self, session_id: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM activities WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(MemoryError::Database)
    }
}

fn row_to_activity(row: &rusqlite::Row<'_>) -> rusqlite::Result<Activity> {
    let status_str: String = row.get(7)?;
    let status = status_str.parse::<ActivityStatus>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    let args_str: String = row.get(4)?;
    let args: serde_json::Value = serde_json::from_str(&args_str).unwrap_or_default();
    let first_seen_str: String = row.get(9)?;
    let last_seen_str: String = row.get(10)?;

    Ok(Activity {
        id: row.get(0)?,
        session_id: row.get(1)?,
        tool_name: row.get(2)?,
        args_hash: row.get(3)?,
        args,
        result_summary: row.get(5)?,
        outcome_class: row.get(6)?,
        status,
        occurrence_count: row.get(8)?,
        first_seen: parse_timestamp(&first_seen_str)?,
        last_seen: parse_timestamp(&last_seen_str)?,
        scope_id: row.get(11)?,
        promoted_fact_id: row.get(12)?,
    })
}

/// Re-export for use by `rusqlite::Connection::query_row`.
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::init_schema;
    use chrono::Utc;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn insert_new_activity() {
        let conn = setup();
        let store = ActivityStore::new(&conn);
        let activity = NewActivity {
            session_id: "sess-1".into(),
            tool_name: "Read".into(),
            args_hash: "abc123def456abc123def456abc123de".into(),
            args: serde_json::json!({"path": "/foo/bar.rs"}),
            result_summary: Some("200 lines".into()),
            outcome_class: "success".into(),
            timestamp: Utc::now(),
            scope_id: 1,
        };
        let (id, deduped) = store.insert_or_dedup(&activity, 300).unwrap();
        assert!(!deduped);
        assert!(id > 0);

        let fetched = store.get(id).unwrap();
        assert_eq!(fetched.tool_name, "Read");
        assert_eq!(fetched.occurrence_count, 1);
        assert_eq!(fetched.status, ActivityStatus::Recorded);
    }

    #[test]
    fn dedup_within_window() {
        let conn = setup();
        let store = ActivityStore::new(&conn);
        let activity = NewActivity {
            session_id: "sess-1".into(),
            tool_name: "Read".into(),
            args_hash: "abc123def456abc123def456abc123de".into(),
            args: serde_json::json!({"path": "/foo/bar.rs"}),
            result_summary: Some("200 lines".into()),
            outcome_class: "success".into(),
            timestamp: Utc::now(),
            scope_id: 1,
        };

        let (id1, deduped1) = store.insert_or_dedup(&activity, 300).unwrap();
        assert!(!deduped1);

        let (id2, deduped2) = store.insert_or_dedup(&activity, 300).unwrap();
        assert!(deduped2);
        assert_eq!(id1, id2);

        let fetched = store.get(id1).unwrap();
        assert_eq!(fetched.occurrence_count, 2);
        assert_eq!(fetched.status, ActivityStatus::Deduplicated);
    }

    #[test]
    fn different_outcome_class_not_deduped() {
        let conn = setup();
        let store = ActivityStore::new(&conn);
        let success = NewActivity {
            session_id: "sess-1".into(),
            tool_name: "Bash".into(),
            args_hash: "abc123def456abc123def456abc123de".into(),
            args: serde_json::json!({"cmd": "cargo test"}),
            result_summary: Some("ok".into()),
            outcome_class: "success".into(),
            timestamp: Utc::now(),
            scope_id: 1,
        };
        let failure = NewActivity {
            outcome_class: "error".into(),
            result_summary: Some("FAILED".into()),
            ..success.clone()
        };

        let (id1, _) = store.insert_or_dedup(&success, 300).unwrap();
        let (id2, deduped) = store.insert_or_dedup(&failure, 300).unwrap();
        assert!(!deduped);
        assert_ne!(id1, id2);
    }

    #[test]
    fn list_by_session() {
        let conn = setup();
        let store = ActivityStore::new(&conn);
        for i in 0..5 {
            let a = NewActivity {
                session_id: "sess-1".into(),
                tool_name: format!("Tool{i}"),
                args_hash: format!("hash{i:032}"),
                args: serde_json::json!({}),
                result_summary: None,
                outcome_class: "success".into(),
                timestamp: Utc::now(),
                scope_id: 1,
            };
            store.insert_or_dedup(&a, 300).unwrap();
        }
        let list = store.list_by_session("sess-1", Some(3)).unwrap();
        assert_eq!(list.len(), 3);

        let count = store.count_by_session("sess-1").unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn list_recent_by_scope() {
        let conn = setup();
        let store = ActivityStore::new(&conn);
        for i in 0..3 {
            let a = NewActivity {
                session_id: "sess-1".into(),
                tool_name: format!("Tool{i}"),
                args_hash: format!("hash{i:032}"),
                args: serde_json::json!({}),
                result_summary: None,
                outcome_class: "success".into(),
                timestamp: Utc::now(),
                scope_id: 1,
            };
            store.insert_or_dedup(&a, 300).unwrap();
        }
        let list = store.list_recent_by_scope(&[1], 10).unwrap();
        assert_eq!(list.len(), 3);

        let empty = store.list_recent_by_scope(&[], 10).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn update_status() {
        let conn = setup();
        let store = ActivityStore::new(&conn);
        let a = NewActivity {
            session_id: "sess-1".into(),
            tool_name: "Bash".into(),
            args_hash: "abc123def456abc123def456abc123de".into(),
            args: serde_json::json!({"cmd": "git commit"}),
            result_summary: Some("committed".into()),
            outcome_class: "success".into(),
            timestamp: Utc::now(),
            scope_id: 1,
        };
        let (id, _) = store.insert_or_dedup(&a, 300).unwrap();

        // Test without a promoted_fact_id (fact FK would require a real fact).
        store
            .update_status(id, ActivityStatus::Promoted, None)
            .unwrap();
        let fetched = store.get(id).unwrap();
        assert_eq!(fetched.status, ActivityStatus::Promoted);
        assert_eq!(fetched.promoted_fact_id, None);
    }
}
