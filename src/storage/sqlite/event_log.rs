//! `impl EventLog for SqliteBackend` — delegates to [`EventStore`] verbatim.
//!
//! The [`UpcasterRegistry`](crate::store::upcaster::UpcasterRegistry) is a backend
//! construction detail (cloned into each closure), never crossing a method boundary.

use std::sync::Arc;

use async_trait::async_trait;

use super::{SqliteBackend, stream_consumer_dropped};
use crate::error::Result;
use crate::storage::EventLog;
use crate::store::events::EventStore;
use crate::types::{Event, EventFilter, NewEvent};

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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::super::SqliteBackend;
    use crate::error::MemoryError;
    use crate::pool::ConnectionPool;
    use crate::storage::EventLog;
    use crate::store::upcaster::UpcasterRegistry;
    use crate::types::{Event, EventFilter, EventType, NewEvent};

    fn backend() -> SqliteBackend {
        let pool = ConnectionPool::open_memory(4).unwrap();
        SqliteBackend::from_pool(Arc::new(pool), Arc::new(UpcasterRegistry::new()))
    }

    fn make_event(source: &str, session_id: Option<&str>) -> NewEvent {
        NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({ "k": "v" }),
            source: source.into(),
            session_id: session_id.map(Into::into),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        }
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
