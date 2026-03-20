use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::error::Result;
use crate::graph::MemoryGraph;
use crate::scope::tree::ScopeTree;
use crate::store::facts::FactStore;
use crate::types::Fact;

use super::types::{
    ExpiredReason, FactExplanation, FactHistory, FactHistoryEntry, FactProvenance, FactState,
    GraphContext, HistoryEventKind,
};

/// Explain why a fact is in its current state.
///
/// # Errors
///
/// Returns [`MemoryError::NotFound`] if the fact does not exist, or
/// [`MemoryError::Database`] on SQL failure.
pub fn explain_fact(
    conn: &Connection,
    graph: &MemoryGraph,
    scope_tree: &ScopeTree,
    embed_dim: usize,
    fact_id: i64,
) -> Result<FactExplanation> {
    let store = FactStore::new(conn, embed_dim);
    let fact = store.get(fact_id)?;
    let now = Utc::now();

    let state = determine_state(&fact, now);
    let provenance = build_provenance(&fact);
    let graph_context = build_graph_context(graph, fact_id);
    let scope_path = scope_tree
        .path_for_id(fact.scope_id)
        .unwrap_or_else(|| format!("scope:{}", fact.scope_id));

    Ok(FactExplanation {
        fact_id,
        state,
        provenance,
        graph_context,
        scope_path,
    })
}

fn determine_state(fact: &Fact, now: DateTime<Utc>) -> FactState {
    // Priority: Expired > Invalidated > Pinned > Due > Active
    if fact.t_expired.is_some() {
        return FactState::Expired {
            reason: ExpiredReason::Unknown,
        };
    }
    if let Some(t_invalid) = fact.t_invalid {
        if t_invalid <= now {
            return FactState::Invalidated { t_invalid };
        }
    }
    if fact.is_pinned {
        return FactState::Pinned;
    }
    if let Some(t_valid) = fact.t_valid {
        if t_valid <= now && (fact.t_invalid.is_none() || fact.t_invalid.unwrap() > now) {
            return FactState::Due {
                t_valid,
                surfaced_at: fact.surfaced_at,
            };
        }
    }
    FactState::Active
}

const fn build_provenance(fact: &Fact) -> FactProvenance {
    FactProvenance {
        source_event_id: fact.source_event_id,
        source_event: None,
        importance: fact.importance,
        importance_score: fact.importance_score,
        is_pinned: fact.is_pinned,
        access_count: fact.access_count,
    }
}

fn build_graph_context(graph: &MemoryGraph, fact_id: i64) -> GraphContext {
    let degree = graph.degree(fact_id);
    // Use connected_component to get ALL neighbors (in + out), consistent with degree.
    // `neighbors()` only returns outgoing, which would be inconsistent with degree.
    let mut neighbor_ids: Vec<i64> = if graph.has_node(fact_id) {
        let component = graph.connected_component(fact_id);
        component.into_iter().filter(|&id| id != fact_id).collect()
    } else {
        Vec::new()
    };
    neighbor_ids.sort_unstable();
    let component_size = neighbor_ids.len() + usize::from(graph.has_node(fact_id));
    GraphContext {
        degree,
        neighbor_ids,
        component_size,
    }
}

/// Reconstruct the temporal history of a fact from its bi-temporal timestamps.
///
/// Returns a sorted timeline of lifecycle events (`Created`, `BecameValid`, etc.)
/// computed from the fact's `t_created`, `t_valid`, `t_invalid`, and `t_expired` fields.
///
/// # Errors
///
/// Returns [`MemoryError::NotFound`] if the fact does not exist, or
/// [`MemoryError::Database`] on SQL failure.
pub fn fact_history(conn: &Connection, embed_dim: usize, fact_id: i64) -> Result<FactHistory> {
    let store = FactStore::new(conn, embed_dim);
    let fact = store.get(fact_id)?;

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

    Ok(FactHistory { fact_id, timeline })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactOptions, FactType};
    use chrono::Duration;

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
    }

    #[test]
    fn explain_active_fact() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let id = engine
            .add_fact(
                "test fact",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                None,
                None,
            )
            .unwrap();
        let explanation = engine.explain_fact(id).unwrap();
        assert_eq!(explanation.fact_id, id);
        assert!(matches!(explanation.state, FactState::Active));
        assert_eq!(explanation.scope_path, "/"); // root scope
    }

    #[test]
    fn explain_pinned_fact() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                "pinned",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                Some(&opts),
                None,
            )
            .unwrap();
        let explanation = engine.explain_fact(id).unwrap();
        assert!(matches!(explanation.state, FactState::Pinned));
        assert!(explanation.provenance.is_pinned);
    }

    #[test]
    fn explain_due_fact() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let opts = AddFactOptions {
            t_valid: Some(Utc::now() - Duration::hours(1)),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                "due fact",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                Some(&opts),
                None,
            )
            .unwrap();
        let explanation = engine.explain_fact(id).unwrap();
        assert!(matches!(explanation.state, FactState::Due { .. }));
    }

    #[test]
    fn explain_not_found() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let err = engine.explain_fact(999).unwrap_err();
        assert!(matches!(err, crate::error::MemoryError::NotFound(_)));
    }

    #[test]
    fn fact_history_all_timestamps() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let opts = AddFactOptions {
            t_valid: Some(Utc::now() - Duration::hours(2)),
            t_invalid: Some(Utc::now() + Duration::hours(1)),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                "temporal",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                Some(&opts),
                None,
            )
            .unwrap();
        let history = engine.fact_history(id).unwrap();
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

    #[test]
    fn fact_history_minimal() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let id = engine
            .add_fact(
                "simple",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                None,
                None,
            )
            .unwrap();
        let history = engine.fact_history(id).unwrap();
        assert_eq!(history.timeline.len(), 1);
        assert!(matches!(
            history.timeline[0].kind,
            HistoryEventKind::Created
        ));
    }
}
