use crate::inspect::types::{ReplayFilter, ReplayOrder};
use crate::store::events::EventFilter;

/// Convert a `ReplayFilter` into an `EventFilter` for the store layer.
#[must_use]
pub fn to_event_filter(filter: &ReplayFilter) -> EventFilter {
    EventFilter {
        since: filter.since,
        until: filter.until,
        session_id: filter.session_id.clone(),
        event_type: filter.event_type.clone(),
        source: None,
        limit: filter.limit,
        id_min: filter.id_range.map(|(min, _)| min),
        id_max: filter.id_range.map(|(_, max)| max),
        order_by_id: matches!(filter.order, ReplayOrder::InsertionOrder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::types::{EventType, NewEvent};
    use chrono::Utc;

    const DIM: usize = 4;

    fn make_event(source: &str) -> NewEvent {
        NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"src": source}),
            source: source.into(),
            session_id: Some("s1".into()),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        }
    }

    #[tokio::test]
    async fn replay_by_id_range() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        // Insert events via the storage port (the empty default registry the old
        // raw `EventStore` used is the backend's own registry).
        for i in 0..5 {
            engine
                .storage()
                .insert_event(&make_event(&format!("e{i}")))
                .await
                .unwrap();
        }
        let filter = ReplayFilter {
            id_range: Some((2, 4)),
            ..Default::default()
        };
        let events = engine.replay_events(&filter).await.unwrap();
        assert_eq!(events.len(), 3); // ids 2, 3, 4
        assert_eq!(events[0].id, 2);
        assert_eq!(events[2].id, 4);
    }

    #[tokio::test]
    async fn replay_default_order_is_insertion() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        for i in 0..3 {
            engine
                .storage()
                .insert_event(&make_event(&format!("e{i}")))
                .await
                .unwrap();
        }
        let filter = ReplayFilter::default();
        let events = engine.replay_events(&filter).await.unwrap();
        // Default order is InsertionOrder (ORDER BY id ASC)
        assert!(events.windows(2).all(|w| w[0].id < w[1].id));
    }
}
