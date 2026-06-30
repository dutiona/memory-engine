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

    // kept: distinct from test_utils::new_event — hardcodes session_id=Some("s1")
    // (tests at lines 127/159/191/269/342 filter by "s1") and uses a
    // source-embedding payload {"src": source} rather than the generic {"key":"value"}.
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

    /// A deterministic timestamp `secs` seconds after the Unix epoch.
    ///
    /// `to_event_filter` maps `since`/`until` onto the `timestamp` column, so the
    /// since/until/order tests must pin known timestamps rather than rely on the
    /// `Utc::now()` of [`make_event`] (which is non-deterministic and would make
    /// the `since`/`until` boundary impossible to assert against).
    fn at(secs: i64) -> chrono::DateTime<Utc> {
        chrono::DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    /// A fully-specified event so each test can vary exactly the field it covers
    /// (`timestamp`, `session_id`, `event_type`) — the defaults in [`make_event`]
    /// are otherwise hard-coded to `Interaction` / `Some("s1")` / `Utc::now()`.
    fn custom_event(
        source: &str,
        timestamp: chrono::DateTime<Utc>,
        session_id: Option<&str>,
        event_type: EventType,
    ) -> NewEvent {
        NewEvent {
            timestamp,
            event_type,
            payload: serde_json::json!({"src": source}),
            source: source.into(),
            session_id: session_id.map(Into::into),
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

    /// `since` maps to `timestamp >= since` (inclusive). Insert five events at
    /// distinct, increasing timestamps and filter from the third one's timestamp:
    /// only events 3, 4, 5 must survive. Asymmetric (3 of 5) so a flipped `>=`/`<=`
    /// or an unmapped `since` field is caught.
    #[tokio::test]
    async fn replay_filter_since() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        // ids 1..=5 at t = 100, 200, 300, 400, 500.
        for i in 1..=5 {
            engine
                .storage()
                .insert_event(&custom_event(
                    &format!("e{i}"),
                    at(i64::from(i) * 100),
                    Some("s1"),
                    EventType::Interaction,
                ))
                .await
                .unwrap();
        }
        let filter = ReplayFilter {
            since: Some(at(300)), // event 3's timestamp
            ..Default::default()
        };
        let events = engine.replay_events(&filter).await.unwrap();
        // Inclusive lower bound: events 3, 4, 5 (ids 3, 4, 5).
        assert_eq!(events.len(), 3);
        assert_eq!(
            events.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert!(events.iter().all(|e| e.timestamp >= at(300)));
    }

    /// `until` maps to `timestamp <= until` (inclusive). Mirror of the `since`
    /// test: filtering at event 3's timestamp keeps events 1, 2, 3 — the
    /// complement of the `since` set, so a swap of the two fields is caught.
    #[tokio::test]
    async fn replay_filter_until() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        for i in 1..=5 {
            engine
                .storage()
                .insert_event(&custom_event(
                    &format!("e{i}"),
                    at(i64::from(i) * 100),
                    Some("s1"),
                    EventType::Interaction,
                ))
                .await
                .unwrap();
        }
        let filter = ReplayFilter {
            until: Some(at(300)), // event 3's timestamp
            ..Default::default()
        };
        let events = engine.replay_events(&filter).await.unwrap();
        // Inclusive upper bound: events 1, 2, 3.
        assert_eq!(events.len(), 3);
        assert_eq!(
            events.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(events.iter().all(|e| e.timestamp <= at(300)));
    }

    /// `since` + `until` together bound an inclusive window. Filtering to
    /// [200, 400] keeps exactly the middle three (ids 2, 3, 4), dropping the
    /// first and last — proves both bounds are mapped simultaneously.
    #[tokio::test]
    async fn replay_filter_since_and_until_window() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        for i in 1..=5 {
            engine
                .storage()
                .insert_event(&custom_event(
                    &format!("e{i}"),
                    at(i64::from(i) * 100),
                    Some("s1"),
                    EventType::Interaction,
                ))
                .await
                .unwrap();
        }
        let filter = ReplayFilter {
            since: Some(at(200)),
            until: Some(at(400)),
            ..Default::default()
        };
        let events = engine.replay_events(&filter).await.unwrap();
        assert_eq!(
            events.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
    }

    /// `session_id` maps to an exact-match `session_id = ?`. Insert an
    /// asymmetric mix (3 in `alpha`, 2 in `beta`) so a query that ignored the
    /// field, or matched the wrong session, would return a different count.
    #[tokio::test]
    async fn replay_filter_session_id() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let sessions = ["alpha", "beta", "alpha", "beta", "alpha"];
        for (i, sess) in sessions.iter().enumerate() {
            engine
                .storage()
                .insert_event(&custom_event(
                    &format!("e{i}"),
                    at(i64::try_from(i).unwrap() * 100 + 100),
                    Some(sess),
                    EventType::Interaction,
                ))
                .await
                .unwrap();
        }
        let filter = ReplayFilter {
            session_id: Some("alpha".into()),
            ..Default::default()
        };
        let events = engine.replay_events(&filter).await.unwrap();
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|e| e.session_id.as_deref() == Some("alpha"))
        );
        // And the other session is genuinely filterable (distinct count).
        let beta = engine
            .replay_events(&ReplayFilter {
                session_id: Some("beta".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(beta.len(), 2);
    }

    /// `event_type` maps to an exact-match `event_type = ?`. Insert an
    /// asymmetric mix (3 `Interaction`, 2 `ToolCall`) and filter each — distinct
    /// counts catch a query that dropped the predicate or matched the wrong type.
    #[tokio::test]
    async fn replay_filter_event_type() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let types = [
            EventType::Interaction,
            EventType::ToolCall,
            EventType::Interaction,
            EventType::ToolCall,
            EventType::Interaction,
        ];
        for (i, et) in types.iter().enumerate() {
            engine
                .storage()
                .insert_event(&custom_event(
                    &format!("e{i}"),
                    at(i64::try_from(i).unwrap() * 100 + 100),
                    Some("s1"),
                    et.clone(),
                ))
                .await
                .unwrap();
        }
        let interactions = engine
            .replay_events(&ReplayFilter {
                event_type: Some(EventType::Interaction),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(interactions.len(), 3);
        assert!(
            interactions
                .iter()
                .all(|e| e.event_type == EventType::Interaction)
        );
        let tool_calls = engine
            .replay_events(&ReplayFilter {
                event_type: Some(EventType::ToolCall),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(tool_calls.len(), 2);
        assert!(
            tool_calls
                .iter()
                .all(|e| e.event_type == EventType::ToolCall)
        );
    }

    /// `limit` maps to a SQL `LIMIT`. Five events, `limit: Some(2)` must return
    /// exactly two. With the default `InsertionOrder` (id ASC) the two are the
    /// lowest ids, so we can assert the identity too — a `LIMIT` applied to the
    /// wrong order, or dropped entirely, fails.
    #[tokio::test]
    async fn replay_filter_limit() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        for i in 0..5 {
            engine
                .storage()
                .insert_event(&make_event(&format!("e{i}")))
                .await
                .unwrap();
        }
        let filter = ReplayFilter {
            limit: Some(2),
            ..Default::default()
        };
        let events = engine.replay_events(&filter).await.unwrap();
        assert_eq!(events.len(), 2);
        // InsertionOrder + LIMIT 2 → the first two inserted (ids 1, 2).
        assert_eq!(events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// `ReplayOrder::TimestampOrder` sorts by `timestamp ASC`, not `id ASC`. The
    /// events are inserted with timestamps in the *reverse* of their id order, so
    /// the two orderings diverge: `TimestampOrder` yields ids descending. Asserting
    /// that distinguishes it from the default `InsertionOrder` (ids ascending) — a
    /// flip of the `order_by_id` mapping would fail.
    #[tokio::test]
    async fn replay_order_timestamp() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        // id 1 → t=500, id 2 → t=400, id 3 → t=300, id 4 → t=200, id 5 → t=100.
        for i in 0..5 {
            engine
                .storage()
                .insert_event(&custom_event(
                    &format!("e{i}"),
                    at(500 - i64::from(i) * 100),
                    Some("s1"),
                    EventType::Interaction,
                ))
                .await
                .unwrap();
        }
        let filter = ReplayFilter {
            order: ReplayOrder::TimestampOrder,
            ..Default::default()
        };
        let events = engine.replay_events(&filter).await.unwrap();
        assert_eq!(events.len(), 5);
        // Sorted ascending by timestamp ...
        assert!(events.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
        // ... which here is the reverse of id order (5, 4, 3, 2, 1).
        assert_eq!(
            events.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![5, 4, 3, 2, 1]
        );

        // Contrast: the default InsertionOrder on the same data is id-ascending —
        // proving the two orders genuinely diverge (the test is non-vacuous).
        let insertion = engine
            .replay_events(&ReplayFilter::default())
            .await
            .unwrap();
        assert_eq!(
            insertion.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
    }
}
