use chrono::{DateTime, Utc};

use crate::types::Fact;

use super::types::{
    ExpiredReason, FactHistory, FactHistoryEntry, FactState, GraphContext, HistoryEventKind,
};

pub fn determine_state(fact: &Fact, now: DateTime<Utc>) -> FactState {
    // Priority: Expired > Invalidated > Pinned > Due > Active
    if fact.t_expired.is_some() {
        // A DreamCycle quarantine leaves a `quarantine` metadata marker, letting us
        // distinguish it from ordinary Ebbinghaus forgetting.
        let reason = if fact.metadata.get("quarantine").is_some() {
            ExpiredReason::Quarantined
        } else {
            ExpiredReason::Unknown
        };
        return FactState::Expired { reason };
    }
    if let Some(t_invalid) = fact.t_invalid
        && t_invalid <= now
    {
        return FactState::Invalidated { t_invalid };
    }
    if fact.is_pinned {
        return FactState::Pinned;
    }
    // Shared valid-time predicate (#477): same `Fact::is_temporally_due` the resume
    // walk and the SQL `list_due` use, so the classifier cannot drift from them.
    // (The earlier `t_invalid <= now` early-return already split off Invalidated, so
    // reaching here with `is_temporally_due` true means a genuinely-due active fact.)
    if let Some(t_valid) = fact.t_valid
        && fact.is_temporally_due(now)
    {
        return FactState::Due {
            t_valid,
            surfaced_at: fact.surfaced_at,
        };
    }
    FactState::Active
}

pub fn build_graph_context(graph: &crate::graph::MemoryGraph, fact_id: i64) -> GraphContext {
    let degree = graph.degree(fact_id);
    // Use connected_component to get ALL neighbors (in + out), consistent with degree.
    // `neighbors()` only returns outgoing, which would be inconsistent with degree.
    let has_node = graph.has_node(fact_id);
    let mut neighbor_ids: Vec<i64> = if has_node {
        let component = graph.connected_component(fact_id);
        component.into_iter().filter(|&id| id != fact_id).collect()
    } else {
        Vec::new()
    };
    neighbor_ids.sort_unstable();
    let component_size = neighbor_ids.len() + usize::from(has_node);
    GraphContext {
        degree,
        neighbor_ids,
        component_size,
    }
}

/// Derive the bi-temporal lifecycle timeline from a single fact's timestamps.
///
/// Returns a sorted timeline of lifecycle events (`Created`, `BecameValid`, etc.)
/// computed from the fact's `t_created`, `t_valid`, `t_invalid`, and `t_expired`
/// fields. The async engine fetches the fact via the port (`get_fact`) and calls
/// this directly — no `&Connection` needed (#631).
#[must_use]
pub fn fact_history_from_fact(fact_id: i64, fact: &Fact) -> FactHistory {
    let mut timeline = Vec::new();
    timeline.push(FactHistoryEntry {
        timestamp: fact.t_created,
        kind: HistoryEventKind::Created,
    });
    if let Some(t_valid) = fact.t_valid {
        timeline.push(FactHistoryEntry {
            timestamp: t_valid,
            kind: HistoryEventKind::BecameValid,
        });
    }
    if let Some(t_invalid) = fact.t_invalid {
        timeline.push(FactHistoryEntry {
            timestamp: t_invalid,
            kind: HistoryEventKind::BecameInvalid,
        });
    }
    if let Some(t_expired) = fact.t_expired {
        timeline.push(FactHistoryEntry {
            timestamp: t_expired,
            kind: HistoryEventKind::Expired,
        });
    }
    timeline.sort_by_key(|e| e.timestamp);

    FactHistory { fact_id, timeline }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactOptions, AddFactRequest, EventType, FactType, NewEvent};
    use chrono::Duration;
    use std::sync::Arc;

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    #[tokio::test]
    async fn explain_active_fact() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: "test fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                Arc::new(FakeEmbed),
                None,
            )
            .await
            .unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        assert_eq!(explanation.fact_id, id);
        assert!(matches!(explanation.state, FactState::Active));
        assert_eq!(explanation.scope_path, "/"); // root scope
    }

    #[tokio::test]
    async fn explain_pinned_fact() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: "pinned".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(opts),
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        assert!(matches!(explanation.state, FactState::Pinned));
        assert!(explanation.provenance.is_pinned);
    }

    #[tokio::test]
    async fn explain_due_fact() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let opts = AddFactOptions {
            t_valid: Some(Utc::now() - Duration::hours(1)),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: "due fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(opts),
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        assert!(matches!(explanation.state, FactState::Due { .. }));
    }

    #[tokio::test]
    async fn explain_not_found() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let err = engine.explain_fact(999).await.unwrap_err();
        assert!(matches!(err, crate::error::MemoryError::NotFound(_)));
    }

    #[tokio::test]
    async fn fact_history_all_timestamps() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let opts = AddFactOptions {
            t_valid: Some(Utc::now() - Duration::hours(2)),
            t_invalid: Some(Utc::now() + Duration::hours(1)),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: "temporal".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: Some(opts),
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let history = engine.fact_history(id).await.unwrap();
        assert_eq!(history.fact_id, id);
        // Created + BecameValid + BecameInvalid = 3 entries (no t_expired)
        assert_eq!(history.timeline.len(), 3);
        assert!(matches!(
            history.timeline[0].kind,
            HistoryEventKind::BecameValid
        ));
        assert!(matches!(
            history.timeline[1].kind,
            HistoryEventKind::Created
        ));
        assert!(matches!(
            history.timeline[2].kind,
            HistoryEventKind::BecameInvalid
        ));
    }

    #[tokio::test]
    async fn fact_history_minimal() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: "simple".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let history = engine.fact_history(id).await.unwrap();
        assert_eq!(history.timeline.len(), 1);
        assert!(matches!(
            history.timeline[0].kind,
            HistoryEventKind::Created
        ));
    }

    #[tokio::test]
    async fn explain_fact_with_source_event() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();

        // Ingest an event to get an event_id
        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"role": "user", "content": "hello"}),
            source: "test".to_string(),
            session_id: Some("sess-1".to_string()),
            scope_id: 1,
            origin_node_id: "node-1".to_string(),
            sequence_id: 1,
            created_at: None,
        };
        let event_id = engine.ingest(&event).await.unwrap();

        // Create a fact linked to that event
        let fact_id = engine
            .add_fact(
                &AddFactRequest {
                    content: "fact from event".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: Some(event_id),
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let explanation = engine.explain_fact(fact_id).await.unwrap();
        assert_eq!(explanation.provenance.source_event_id, Some(event_id));

        let source_event = explanation
            .provenance
            .source_event
            .expect("source_event should be populated");
        assert_eq!(source_event.id, event_id);
        assert!(matches!(source_event.event_type, EventType::Interaction));
    }

    #[tokio::test]
    async fn explain_fact_without_source_event() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let fact_id = engine
            .add_fact(
                &AddFactRequest {
                    content: "standalone fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let explanation = engine.explain_fact(fact_id).await.unwrap();
        assert_eq!(explanation.provenance.source_event_id, None);
        assert!(explanation.provenance.source_event.is_none());
    }

    #[tokio::test]
    async fn snapshot_active_fact_explanation() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        let id = engine
            .add_fact(
                &AddFactRequest {
                    content: "snapshot test fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        insta::assert_yaml_snapshot!(explanation);
    }
}
