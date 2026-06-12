use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use crate::error::{MemoryError, Result};
use crate::store::upcaster::UpcasterRegistry;
use crate::types::{Event, EventType, NewEvent};

/// Filter for querying events.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub event_type: Option<EventType>,
    pub source: Option<String>,
    pub limit: Option<usize>,
    pub id_min: Option<i64>,
    pub id_max: Option<i64>,
    pub order_by_id: bool,
}

/// Store for the append-only event log.
pub struct EventStore<'a> {
    conn: &'a Connection,
    registry: &'a UpcasterRegistry,
}

pub const fn event_type_to_str(et: &EventType) -> &'static str {
    match et {
        EventType::Interaction => "Interaction",
        EventType::ToolCall => "ToolCall",
        EventType::MemoryOp => "MemoryOp",
        EventType::SystemEvent => "SystemEvent",
        EventType::OutcomeSignal => "OutcomeSignal",
    }
}

fn str_to_event_type(s: &str) -> Result<EventType> {
    match s {
        "Interaction" => Ok(EventType::Interaction),
        "ToolCall" => Ok(EventType::ToolCall),
        "MemoryOp" => Ok(EventType::MemoryOp),
        "SystemEvent" => Ok(EventType::SystemEvent),
        "OutcomeSignal" => Ok(EventType::OutcomeSignal),
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

    let created_at_str: Option<String> = row.get("created_at")?;
    let created_at = created_at_str
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
        })
        .transpose()?;

    let event_revision: i64 = row.get("event_revision")?;

    Ok(Event {
        id: row.get("id")?,
        timestamp,
        event_type,
        payload,
        source: row.get("source")?,
        session_id: row.get("session_id")?,
        scope_id: row.get("scope_id")?,
        origin_node_id: row.get("origin_node_id")?,
        sequence_id: row.get("sequence_id")?,
        created_at,
        event_revision: u16::try_from(event_revision).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                10, // event_revision column index
                rusqlite::types::Type::Integer,
                format!("event_revision {event_revision} out of u16 range").into(),
            )
        })?,
    })
}

impl<'a> EventStore<'a> {
    /// Create a new `EventStore` borrowing the given connection and upcaster registry.
    #[must_use]
    pub const fn new(conn: &'a Connection, registry: &'a UpcasterRegistry) -> Self {
        Self { conn, registry }
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

        let created_at_str = event.created_at.map(|dt| dt.to_rfc3339());

        self.conn.execute(
            "INSERT INTO events (timestamp, event_type, payload, source, session_id, scope_id,
                origin_node_id, sequence_id, created_at, event_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                timestamp_str,
                event_type_str,
                payload_str,
                event.source,
                event.session_id,
                event.scope_id,
                event.origin_node_id,
                event.sequence_id,
                created_at_str,
                i64::from(self.registry.latest_revision(event_type_str)),
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
            "SELECT id, timestamp, event_type, payload, source, session_id, scope_id,
                    origin_node_id, sequence_id, created_at, event_revision
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
            "SELECT id, timestamp, event_type, payload, source, session_id, scope_id,
                    origin_node_id, sequence_id, created_at, event_revision FROM events",
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

    /// Iterate all events row-by-row, calling `f` for each.
    ///
    /// Unlike [`Self::list`], this never allocates a `Vec` — each event is
    /// deserialized, passed to the callback, and dropped before the next
    /// row is read.  Suitable for streaming serialization of large databases.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure, or propagates any
    /// error returned by `f`.
    pub fn for_each<F>(&self, mut f: F) -> Result<()>
    where
        F: FnMut(Event) -> Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, event_type, payload, source, session_id, scope_id,
                    origin_node_id, sequence_id, created_at, event_revision
             FROM events ORDER BY timestamp ASC",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let event = row_to_event(row)?;
            f(event)?;
        }
        Ok(())
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

    /// Get an event by id with upcasted payload.
    ///
    /// Unlike [`Self::get`], this applies the upcaster chain to transform
    /// the payload from its stored revision to the latest revision.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the id doesn't exist.
    /// Returns `MemoryError::Migration` if upcasting fails.
    pub fn get_upcasted(&self, id: i64) -> Result<Event> {
        let event = self.get(id)?;
        self.apply_upcasting(event)
    }

    /// List events matching the filter with upcasted payloads.
    ///
    /// Unlike [`Self::list`], this applies the upcaster chain to transform
    /// each event's payload from its stored revision to the latest revision.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    /// Returns `MemoryError::Migration` if upcasting fails.
    pub fn list_upcasted(&self, filter: &EventFilter) -> Result<Vec<Event>> {
        let events = self.list(filter)?;
        events
            .into_iter()
            .map(|e| self.apply_upcasting(e))
            .collect()
    }

    /// Apply the upcaster chain to an event's payload.
    fn apply_upcasting(&self, mut event: Event) -> Result<Event> {
        let event_type_str = event_type_to_str(&event.event_type);
        let (new_payload, new_rev) =
            self.registry
                .upcast(event_type_str, event.event_revision, event.payload)?;
        event.payload = new_payload;
        event.event_revision = new_rev;
        Ok(event)
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
    if let Some(ref until) = filter.until {
        clauses.push(format!("timestamp <= ?{idx}"));
        values.push(Box::new(until.to_rfc3339()));
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
    if let Some(ref source) = filter.source {
        clauses.push(format!("source = ?{idx}"));
        values.push(Box::new(source.clone()));
        idx += 1;
    }
    if let Some(id_min) = filter.id_min {
        clauses.push(format!("id >= ?{idx}"));
        values.push(Box::new(id_min));
        idx += 1;
    }
    if let Some(id_max) = filter.id_max {
        clauses.push(format!("id <= ?{idx}"));
        values.push(Box::new(id_max));
        idx += 1;
    }

    let mut sql = base.to_string();
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    if filter.order_by_id {
        sql.push_str(" ORDER BY id ASC");
    } else {
        sql.push_str(" ORDER BY timestamp ASC");
    }

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
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        }
    }

    #[test]
    fn insert_returns_id() {
        let conn = setup();
        let registry = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &registry);
        let id = store.insert(&make_event("test", None)).unwrap();
        assert_eq!(id, 1);
        let id2 = store.insert(&make_event("test", None)).unwrap();
        assert_eq!(id2, 2);
    }

    #[test]
    fn get_round_trip() {
        let conn = setup();
        let registry = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &registry);
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
        let registry = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &registry);
        let err = store.get(999).unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));
    }

    #[test]
    fn list_with_session_id_filter() {
        let conn = setup();
        let registry = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &registry);
        store.insert(&make_event("a", Some("sess-1"))).unwrap();
        store.insert(&make_event("b", Some("sess-2"))).unwrap();
        store.insert(&make_event("c", Some("sess-1"))).unwrap();

        let filter = EventFilter {
            session_id: Some("sess-1".into()),
            ..EventFilter::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|e| e.session_id == Some("sess-1".into()))
        );
    }

    #[test]
    fn count_matches_list() {
        let conn = setup();
        let registry = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &registry);
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
        let registry = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &registry);
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
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };
        let id = store.insert(&event).unwrap();
        let retrieved = store.get(id).unwrap();
        assert_eq!(retrieved.payload, payload);
        assert_eq!(retrieved.event_type, EventType::ToolCall);
    }

    #[test]
    fn get_returns_raw_event() {
        let conn = setup();
        // Insert with empty registry → stamped at revision 1
        let empty_reg = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &empty_reg);
        let id = store.insert(&make_event("test", None)).unwrap();

        // Now read with a registry that has upcasters
        let mut registry = UpcasterRegistry::new();
        registry.register("Interaction", 1, |mut v| {
            v["upcasted"] = serde_json::json!(true);
            Ok(v)
        });
        let store2 = EventStore::new(&conn, &registry);

        // Raw get() does NOT apply upcasting — returns stored payload
        let raw = store2.get(id).unwrap();
        assert!(raw.payload.get("upcasted").is_none());
        assert_eq!(raw.event_revision, 1); // stored at revision 1
    }

    #[test]
    fn get_upcasted_transforms_payload() {
        let conn = setup();
        // Insert with empty registry → stored at revision 1
        let empty_reg = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &empty_reg);
        let id = store.insert(&make_event("test", None)).unwrap();

        // Read with upcaster registry
        let mut registry = UpcasterRegistry::new();
        registry.register("Interaction", 1, |mut v| {
            v["upcasted"] = serde_json::json!(true);
            Ok(v)
        });
        let store2 = EventStore::new(&conn, &registry);

        // get_upcasted() applies the chain: revision 1 → 2
        let upcasted = store2.get_upcasted(id).unwrap();
        assert_eq!(upcasted.payload["upcasted"], true);
        assert_eq!(upcasted.event_revision, 2);
    }

    #[test]
    fn list_upcasted_transforms_all() {
        let conn = setup();
        // Insert with empty registry → stored at revision 1
        let empty_reg = UpcasterRegistry::new();
        let store = EventStore::new(&conn, &empty_reg);
        store.insert(&make_event("a", None)).unwrap();
        store.insert(&make_event("b", None)).unwrap();

        // Read with upcaster registry
        let mut registry = UpcasterRegistry::new();
        registry.register("Interaction", 1, |mut v| {
            v["version"] = serde_json::json!("v2");
            Ok(v)
        });
        let store2 = EventStore::new(&conn, &registry);

        let raw = store2.list(&EventFilter::default()).unwrap();
        assert!(raw.iter().all(|e| e.payload.get("version").is_none()));

        let upcasted = store2.list_upcasted(&EventFilter::default()).unwrap();
        assert_eq!(upcasted.len(), 2);
        assert!(upcasted.iter().all(|e| e.payload["version"] == "v2"));
        assert!(upcasted.iter().all(|e| e.event_revision == 2));
    }

    #[test]
    fn insert_stamps_latest_revision() {
        let conn = setup();
        let mut registry = UpcasterRegistry::new();
        // Interaction has upcasters 1→2 and 2→3, so latest = 3
        registry.register("Interaction", 1, Ok);
        registry.register("Interaction", 2, Ok);
        let store = EventStore::new(&conn, &registry);

        let id = store.insert(&make_event("test", None)).unwrap();
        let event = store.get(id).unwrap();
        assert_eq!(event.event_revision, 3); // stamped at latest
    }

    #[test]
    fn serde_event_revision_compat() {
        // Event JSON without event_revision deserializes with default 1
        let json = r#"{
            "id": 1,
            "timestamp": "2024-01-01T00:00:00Z",
            "event_type": "Interaction",
            "payload": {},
            "source": "test",
            "session_id": null,
            "scope_id": 1,
            "origin_node_id": "local",
            "sequence_id": 0,
            "created_at": null
        }"#;
        let event: crate::types::Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_revision, 1);

        // With explicit event_revision
        let json_v3 = r#"{
            "id": 1,
            "timestamp": "2024-01-01T00:00:00Z",
            "event_type": "Interaction",
            "payload": {},
            "source": "test",
            "session_id": null,
            "scope_id": 1,
            "origin_node_id": "local",
            "sequence_id": 0,
            "created_at": null,
            "event_revision": 3
        }"#;
        let event_v3: crate::types::Event = serde_json::from_str(json_v3).unwrap();
        assert_eq!(event_v3.event_revision, 3);
    }
}
