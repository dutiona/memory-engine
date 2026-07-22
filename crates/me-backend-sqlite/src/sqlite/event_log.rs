//! `impl EventLog for SqliteBackend` — delegates to [`EventStore`] verbatim.
//!
//! The [`UpcasterRegistry`](crate::store::upcaster::UpcasterRegistry) is a backend
//! construction detail (cloned into each closure), never crossing a method boundary.

use me_types::error::StorageError;
use std::sync::Arc;

use async_trait::async_trait;

use super::{SqliteBackend, stream_consumer_dropped};
use crate::store::events::EventStore;
use me_storage::EventLog;
use me_types::error::Result;
use me_types::types::{Event, EventFilter, NewEvent};

#[async_trait]
impl EventLog for SqliteBackend {
    async fn insert_event(&self, event: &NewEvent) -> Result<i64> {
        let event = event.clone();
        let registry = Arc::clone(&self.upcaster_registry);
        self.block_write(move |c| EventStore::new(c, &registry).insert(&event))
            .await
    }

    async fn get_event(&self, id: i64) -> Result<Event> {
        let registry = Arc::clone(&self.upcaster_registry);
        self.block_read(move |c| EventStore::new(c, &registry).get(id))
            .await
    }

    async fn list_events(&self, filter: &EventFilter) -> Result<Vec<Event>> {
        let filter = filter.clone();
        let registry = Arc::clone(&self.upcaster_registry);
        self.block_read(move |c| EventStore::new(c, &registry).list(&filter))
            .await
    }

    async fn count_events(&self, filter: &EventFilter) -> Result<i64> {
        let filter = filter.clone();
        let registry = Arc::clone(&self.upcaster_registry);
        self.block_read(move |c| EventStore::new(c, &registry).count(&filter))
            .await
    }

    async fn for_each_event(&self, f: &mut (dyn FnMut(Event) -> Result<()> + Send)) -> Result<()> {
        let registry = Arc::clone(&self.upcaster_registry);
        self.for_each_streamed(
            move |conn, tx| {
                EventStore::new(conn, &registry)
                    .for_each(|ev| tx.blocking_send(ev).map_err(|_| stream_consumer_dropped()))
            },
            f,
        )
        .await
    }

    async fn get_upcasted_event(&self, id: i64) -> Result<Event> {
        let registry = Arc::clone(&self.upcaster_registry);
        self.block_read(move |c| EventStore::new(c, &registry).get_upcasted(id))
            .await
    }

    async fn list_upcasted_events(&self, filter: &EventFilter) -> Result<Vec<Event>> {
        let filter = filter.clone();
        let registry = Arc::clone(&self.upcaster_registry);
        self.block_read(move |c| EventStore::new(c, &registry).list_upcasted(&filter))
            .await
    }

    // READ — outcome-signal aggregation push-down (verbatim SQL from the pre-cutover
    // `engine/outcome.rs::get_outcome_counts`).
    async fn count_outcome_signals(&self, fact_id: i64) -> Result<me_types::types::OutcomeCounts> {
        self.block_read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT json_extract(payload, '$.outcome') AS outcome, COUNT(*) AS cnt
                 FROM events
                 WHERE event_type = 'OutcomeSignal'
                   AND json_extract(payload, '$.fact_id') = ?1
                 GROUP BY outcome",
                )
                .map_err(StorageError::backend)?;
            let mut counts = me_types::types::OutcomeCounts::default();
            let rows = stmt
                .query_map(rusqlite::params![fact_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })
                .map_err(StorageError::backend)?;
            for row in rows {
                let (outcome_str, cnt) = row.map_err(StorageError::backend)?;
                match outcome_str.as_str() {
                    "Positive" => counts.positive = cnt,
                    "Negative" => counts.negative = cnt,
                    "Neutral" => counts.neutral = cnt,
                    _ => {} // skip unknown variants (forward-compat)
                }
            }
            Ok(counts)
        })
        .await
    }

    // READ — batch outcome-signal aggregation push-down (verbatim SQL from the
    // pre-cutover `engine/outcome.rs::get_outcome_counts_batch`).
    async fn count_outcome_signals_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, me_types::types::OutcomeCounts>> {
        if fact_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let ids_json = serde_json::to_string(fact_ids)?;
        self.block_read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT json_extract(payload, '$.fact_id') AS fid,
                        json_extract(payload, '$.outcome') AS outcome,
                        COUNT(*) AS cnt
                 FROM events
                 WHERE event_type = 'OutcomeSignal'
                   AND json_extract(payload, '$.fact_id') IN (SELECT value FROM json_each(?1))
                 GROUP BY fid, outcome",
                )
                .map_err(StorageError::backend)?;
            let mut by_fact: std::collections::HashMap<i64, me_types::types::OutcomeCounts> =
                std::collections::HashMap::new();
            let rows = stmt
                .query_map(rusqlite::params![ids_json], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u32>(2)?,
                    ))
                })
                .map_err(StorageError::backend)?;
            for row in rows {
                let (fid, outcome_str, cnt) = row.map_err(StorageError::backend)?;
                let entry = by_fact.entry(fid).or_default();
                match outcome_str.as_str() {
                    "Positive" => entry.positive = cnt,
                    "Negative" => entry.negative = cnt,
                    "Neutral" => entry.neutral = cnt,
                    _ => {} // skip unknown variants (forward-compat)
                }
            }
            Ok(by_fact)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::SqliteBackend;
    use crate::pool::ConnectionPool;
    use crate::store::upcaster::UpcasterRegistry;
    use me_storage::EventLog;
    use me_types::error::MemoryError;
    use me_types::types::{Event, EventFilter, EventType, NewEvent};

    fn backend() -> SqliteBackend {
        let pool = ConnectionPool::open_memory(4).unwrap();
        SqliteBackend::from_pool(Arc::new(pool), Arc::new(UpcasterRegistry::new()))
    }

    fn make_event(source: &str, session_id: Option<&str>) -> NewEvent {
        me_types::test_util::new_event(source, session_id)
    }

    #[tokio::test]
    async fn insert_get_round_trip() {
        let be = backend();
        let id = be
            .insert_event(&make_event("s1", Some("sess")))
            .await
            .unwrap();
        assert_eq!(id, 1);
        let ev = be.get_event(id).await.unwrap();
        assert_eq!(ev.source, "s1");
        assert_eq!(ev.session_id, Some("sess".into()));
        assert_eq!(ev.event_type, EventType::Interaction);
    }

    #[tokio::test]
    async fn get_missing_yields_not_found() {
        // H4: a missing id is a semantic NotFound, NOT remapped to Storage(Backend).
        let be = backend();
        let err = be.get_event(999).await.unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn list_and_count_filter_parity() {
        let be = backend();
        be.insert_event(&make_event("a", Some("s1"))).await.unwrap();
        be.insert_event(&make_event("b", Some("s2"))).await.unwrap();
        be.insert_event(&make_event("c", Some("s1"))).await.unwrap();
        let filter = EventFilter {
            session_id: Some("s1".into()),
            ..EventFilter::default()
        };
        let listed = be.list_events(&filter).await.unwrap();
        let count = be.count_events(&filter).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(count, 2);
        assert!(listed.iter().all(|e| e.session_id == Some("s1".into())));
    }

    #[tokio::test]
    async fn for_each_event_collects_all_in_scan_order() {
        let be = backend();
        for s in ["a", "b", "c"] {
            be.insert_event(&make_event(s, None)).await.unwrap();
        }
        let mut seen: Vec<String> = Vec::new();
        be.for_each_event(&mut |ev: Event| {
            seen.push(ev.source);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(seen, vec!["a".to_string(), "b".into(), "c".into()]);
    }

    #[tokio::test]
    async fn for_each_event_callback_error_propagates() {
        let be = backend();
        for s in ["a", "b", "c"] {
            be.insert_event(&make_event(s, None)).await.unwrap();
        }
        let mut n = 0;
        let err = be
            .for_each_event(&mut |_ev: Event| {
                n += 1;
                if n == 2 {
                    return Err(MemoryError::Lineage("stop".into()));
                }
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Lineage(_)), "got {err:?}");
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn upcasted_read_applies_registry() {
        // Insert with an empty registry (stored at revision 1), read through a
        // backend whose registry upcasts 1→2.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ev.db");
        let id = {
            let raw = SqliteBackend::from_pool(
                Arc::new(ConnectionPool::open(&path, 4, 2, None).unwrap()),
                Arc::new(UpcasterRegistry::new()),
            );
            raw.insert_event(&make_event("x", None)).await.unwrap()
        };
        let mut registry = UpcasterRegistry::new();
        registry.register("Interaction", 1, |mut v| {
            v["upcasted"] = serde_json::json!(true);
            Ok(v)
        });
        let be = SqliteBackend::from_pool(
            Arc::new(ConnectionPool::open(&path, 4, 2, None).unwrap()),
            Arc::new(registry),
        );
        let raw = be.get_event(id).await.unwrap();
        assert!(
            raw.payload.get("upcasted").is_none(),
            "raw read is unmodified"
        );
        let up = be.get_upcasted_event(id).await.unwrap();
        assert_eq!(up.payload["upcasted"], true);
        assert_eq!(up.event_revision, 2);
    }

    #[tokio::test]
    async fn insert_on_read_only_backend_yields_read_only() {
        // H7 via a real trait method (write path through try_write).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.db");
        {
            let _rw = ConnectionPool::open(&path, 4, 2, None).unwrap();
        }
        let be = SqliteBackend::from_pool(
            Arc::new(ConnectionPool::open_read_only(&path, 4, 2).unwrap()),
            Arc::new(UpcasterRegistry::new()),
        );
        let err = be.insert_event(&make_event("x", None)).await.unwrap_err();
        assert!(matches!(err, MemoryError::ReadOnly), "got {err:?}");
    }
}
