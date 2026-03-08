use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use crate::error::{MemoryError, Result};
use crate::types::{Event, EventType, NewEvent};

/// Filter for querying events.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub since: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub event_type: Option<EventType>,
    pub limit: Option<usize>,
}

/// Store for the append-only event log.
pub struct EventStore<'a> {
    conn: &'a Connection,
}

const fn event_type_to_str(et: &EventType) -> &'static str {
    match et {
        EventType::Interaction => "Interaction",
        EventType::ToolCall => "ToolCall",
        EventType::MemoryOp => "MemoryOp",
        EventType::SystemEvent => "SystemEvent",
    }
}

fn str_to_event_type(s: &str) -> Result<EventType> {
    match s {
        "Interaction" => Ok(EventType::Interaction),
        "ToolCall" => Ok(EventType::ToolCall),
        "MemoryOp" => Ok(EventType::MemoryOp),
        "SystemEvent" => Ok(EventType::SystemEvent),
        other => Err(MemoryError::NotFound(format!(
            "unknown event type: {other}"
        ))),
    }
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let timestamp_str: String = row.get("timestamp")?;
    let event_type_str: String = row.get("event_type")?;
    let payload_str: String = row.get("payload")?;

    let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;

    let event_type = str_to_event_type(&event_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let payload: serde_json::Value = serde_json::from_str(&payload_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Event {
        id: row.get("id")?,
        timestamp,
        event_type,
        payload,
        source: row.get("source")?,
        session_id: row.get("session_id")?,
    })
}

impl<'a> EventStore<'a> {
    /// Create a new `EventStore` borrowing the given connection.
    #[must_use]
    pub const fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a new event, returning its auto-assigned id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on insert failure.
    pub fn insert(&self, event: &NewEvent) -> Result<i64> {
        let timestamp_str = event.timestamp.to_rfc3339();
        let event_type_str = event_type_to_str(&event.event_type);
        let payload_str = serde_json::to_string(&event.payload)?;

        self.conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                timestamp_str,
                event_type_str,
                payload_str,
                event.source,
                event.session_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get an event by id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the id doesn't exist.
    pub fn get(&self, id: i64) -> Result<Event> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, event_type, payload, source, session_id
             FROM events WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_event)?;
        match rows.next() {
            Some(Ok(event)) => Ok(event),
            Some(Err(e)) => Err(e.into()),
            None => Err(MemoryError::NotFound(format!("event {id}"))),
        }
    }

    /// List events matching the filter.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list(&self, filter: &EventFilter) -> Result<Vec<Event>> {
        let (sql, values) = build_filter_query(
            "SELECT id, timestamp, event_type, payload, source, session_id FROM events",
            filter,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values), row_to_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Count events matching the filter.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn count(&self, filter: &EventFilter) -> Result<i64> {
        let (sql, values) = build_filter_query("SELECT COUNT(*) FROM events", filter);
        let mut stmt = self.conn.prepare(&sql)?;
        let count = stmt.query_row(rusqlite::params_from_iter(values), |row| row.get(0))?;
        Ok(count)
    }
}

/// Build a filtered SQL query with dynamic WHERE clauses.
/// Returns the SQL string and a Vec of boxed parameter values.
fn build_filter_query(
    base: &str,
    filter: &EventFilter,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    if let Some(ref since) = filter.since {
        clauses.push(format!("timestamp >= ?{idx}"));
        values.push(Box::new(since.to_rfc3339()));
        idx += 1;
    }
    if let Some(ref session_id) = filter.session_id {
        clauses.push(format!("session_id = ?{idx}"));
        values.push(Box::new(session_id.clone()));
        idx += 1;
    }
    if let Some(ref event_type) = filter.event_type {
        clauses.push(format!("event_type = ?{idx}"));
        values.push(Box::new(event_type_to_str(event_type).to_string()));
        idx += 1;
    }

    let mut sql = base.to_string();
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY timestamp ASC");

    if let Some(limit) = filter.limit {
        use std::fmt::Write;
        let _ = write!(sql, " LIMIT ?{idx}");
        values.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));
    }

    (sql, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{init_schema, open_memory};

    fn setup() -> Connection {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn make_event(source: &str, session_id: Option<&str>) -> NewEvent {
        NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"key": "value"}),
            source: source.into(),
            session_id: session_id.map(Into::into),
        }
    }

    #[test]
    fn insert_returns_id() {
        let conn = setup();
        let store = EventStore::new(&conn);
        let id = store.insert(&make_event("test", None)).unwrap();
        assert_eq!(id, 1);
        let id2 = store.insert(&make_event("test", None)).unwrap();
        assert_eq!(id2, 2);
    }

    #[test]
    fn get_round_trip() {
        let conn = setup();
        let store = EventStore::new(&conn);
        let event = make_event("src1", Some("sess-1"));
        let id = store.insert(&event).unwrap();
        let retrieved = store.get(id).unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.source, "src1");
        assert_eq!(retrieved.session_id, Some("sess-1".into()));
        assert_eq!(retrieved.event_type, EventType::Interaction);
    }

    #[test]
    fn get_not_found() {
        let conn = setup();
        let store = EventStore::new(&conn);
        let err = store.get(999).unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));
    }

    #[test]
    fn list_with_session_id_filter() {
        let conn = setup();
        let store = EventStore::new(&conn);
        store.insert(&make_event("a", Some("sess-1"))).unwrap();
        store.insert(&make_event("b", Some("sess-2"))).unwrap();
        store.insert(&make_event("c", Some("sess-1"))).unwrap();

        let filter = EventFilter {
            session_id: Some("sess-1".into()),
            ..EventFilter::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|e| e.session_id == Some("sess-1".into())));
    }

    #[test]
    fn count_matches_list() {
        let conn = setup();
        let store = EventStore::new(&conn);
        store.insert(&make_event("a", Some("sess-1"))).unwrap();
        store.insert(&make_event("b", Some("sess-2"))).unwrap();
        store.insert(&make_event("c", Some("sess-1"))).unwrap();

        let filter = EventFilter {
            session_id: Some("sess-1".into()),
            ..EventFilter::default()
        };
        let count = store.count(&filter).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn json_payload_round_trip() {
        let conn = setup();
        let store = EventStore::new(&conn);
        let payload = serde_json::json!({
            "nested": {"array": [1, 2, 3]},
            "bool": true,
            "null_val": null
        });
        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::ToolCall,
            payload: payload.clone(),
            source: "test".into(),
            session_id: None,
        };
        let id = store.insert(&event).unwrap();
        let retrieved = store.get(id).unwrap();
        assert_eq!(retrieved.payload, payload);
        assert_eq!(retrieved.event_type, EventType::ToolCall);
    }
}
