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
    let has_node = graph.has_node(fact_id);
    // Immediate (distance-1) in+out neighbors — consistent with `degree`. This was
    // `connected_component` (the whole *transitive* component minus self), which #901
    // flagged as inconsistent with `degree`: a fact of degree 2 sitting in a larger
    // component reported every transitively-reachable fact as a "neighbor".
    let mut neighbor_ids = graph.neighbors_undirected(fact_id);
    neighbor_ids.sort_unstable();
    // `component_size` stays the separate, broader transitive-component metric (the
    // whole weakly-connected component including the fact itself).
    let component_size = if has_node {
        graph.connected_component(fact_id).len()
    } else {
        0
    };
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
    use crate::graph::{EdgeData, MemoryGraph};
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactOptions, AddFactRequest, EventType, FactType, NewEvent};
    use chrono::Duration;
    use std::sync::Arc;

    const DIM: usize = 4;

    /// A neutral, *active* baseline fact: no expiry, no valid-time, not pinned.
    ///
    /// Tests for [`determine_state`] / [`fact_history_from_fact`] mutate exactly the
    /// one or two timestamp/flag fields under test on top of this baseline, so each
    /// assertion isolates a single classifier branch. `determine_state` and
    /// `fact_history_from_fact` are pure over a `Fact`, so a hand-built struct is
    /// the most direct, least-coupled way to drive every branch — including the
    /// past-`t_invalid` and `t_expired` states the engine's `add_fact` entry points
    /// cannot set.
    fn baseline_fact(now: DateTime<Utc>) -> Fact {
        Fact {
            id: 1,
            content: "baseline".into(),
            content_hash: "0".repeat(32),
            embedding: vec![0.0; DIM],
            fact_type: FactType::Semantic,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: now,
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.5,
            surfaced_at: None,
        }
    }

    // --- #314 / #458: determine_state branch + priority coverage -----------

    /// #314: a fact with a *past* `t_invalid` (and no `t_expired`, not pinned)
    /// classifies as `Invalidated`, and the exact `t_invalid` instant is threaded
    /// through unchanged. The asserted instant is deliberately offset from both
    /// `now` and `t_created` so a wrong field (e.g. echoing `now`) would fail.
    #[test]
    fn determine_state_invalidated_branch() {
        let now = Utc::now();
        let t_invalid = now - Duration::hours(3);
        let mut fact = baseline_fact(now);
        fact.t_invalid = Some(t_invalid);

        let state = determine_state(&fact, now);
        assert_eq!(state, FactState::Invalidated { t_invalid });
    }

    /// #314: a `t_invalid` that is strictly in the *future* must NOT invalidate —
    /// `is_temporally_due` keeps the fact `Due` (it also has a past `t_valid`).
    ///
    /// Note: a strictly-future `t_invalid` behaves identically under `<` and `<=`,
    /// so this case alone does NOT discriminate the `t_invalid <= now` boundary
    /// (the flip is silently accepted). The exact-boundary case
    /// [`determine_state_t_invalid_at_now_is_invalidated`] pins the `<=` vs `<`
    /// distinction; this one only covers the "clearly future ⇒ still Due" branch.
    #[test]
    fn determine_state_future_t_invalid_is_not_invalidated() {
        let now = Utc::now();
        let mut fact = baseline_fact(now);
        fact.t_valid = Some(now - Duration::hours(1));
        fact.t_invalid = Some(now + Duration::hours(1));

        let state = determine_state(&fact, now);
        assert!(
            matches!(state, FactState::Due { .. }),
            "future t_invalid must stay Due, got {state:?}"
        );
    }

    /// #314: the exact `t_invalid == now` boundary classifies as `Invalidated`,
    /// pinning the `t_invalid <= now` comparison against a `<=` → `<` flip.
    ///
    /// This is the discriminating case the strictly-future test above cannot reach:
    /// under the live `<=` the fact is `Invalidated { t_invalid: now }`; under a
    /// mutated `<` the `t_invalid` branch is skipped and — with `t_valid` left
    /// `None`, so `is_temporally_due` is false — the fact falls through to `Active`.
    /// Asserting the exact `Invalidated { t_invalid }` (not just the variant) also
    /// guards the threaded instant.
    #[test]
    fn determine_state_t_invalid_at_now_is_invalidated() {
        let now = Utc::now();
        let mut fact = baseline_fact(now);
        // `t_valid` stays None: a `<=` → `<` flip would route to Active (not Due),
        // making the boundary failure unambiguous.
        fact.t_invalid = Some(now);

        let state = determine_state(&fact, now);
        assert_eq!(state, FactState::Invalidated { t_invalid: now });
    }

    /// #314: `t_expired` set without a `quarantine` metadata marker → `Unknown`.
    #[test]
    fn determine_state_expired_unknown_reason() {
        let now = Utc::now();
        let mut fact = baseline_fact(now);
        fact.t_expired = Some(now - Duration::hours(1));

        let state = determine_state(&fact, now);
        assert_eq!(
            state,
            FactState::Expired {
                reason: ExpiredReason::Unknown
            }
        );
    }

    /// #314: `t_expired` set WITH a `quarantine` metadata marker → `Quarantined`.
    /// Paired with the `Unknown` case above so a constant-return bug (always one
    /// reason) is caught by the asymmetry.
    #[test]
    fn determine_state_expired_quarantined_reason() {
        let now = Utc::now();
        let mut fact = baseline_fact(now);
        fact.t_expired = Some(now - Duration::hours(1));
        fact.metadata = serde_json::json!({ "quarantine": "dream-cycle" });

        let state = determine_state(&fact, now);
        assert_eq!(
            state,
            FactState::Expired {
                reason: ExpiredReason::Quarantined
            }
        );
    }

    /// #458: priority `Pinned > Due` — a pinned fact whose `t_valid` is in the past
    /// (so it would otherwise be `Due`) classifies as `Pinned`. Catches a chain
    /// re-order that put the `is_temporally_due` check before `is_pinned`.
    #[test]
    fn determine_state_pinned_beats_due() {
        let now = Utc::now();
        let mut fact = baseline_fact(now);
        fact.is_pinned = true;
        fact.t_valid = Some(now - Duration::hours(1));

        let state = determine_state(&fact, now);
        assert_eq!(state, FactState::Pinned);
    }

    /// #458: priority `Expired > Pinned` — an expired *and* pinned fact classifies
    /// as `Expired`, not `Pinned`. Catches a chain re-order that ran `is_pinned`
    /// before the `t_expired` check.
    #[test]
    fn determine_state_expired_beats_pinned() {
        let now = Utc::now();
        let mut fact = baseline_fact(now);
        fact.t_expired = Some(now - Duration::hours(1));
        fact.is_pinned = true;

        let state = determine_state(&fact, now);
        assert_eq!(
            state,
            FactState::Expired {
                reason: ExpiredReason::Unknown
            }
        );
    }

    /// #458: priority `Expired > Invalidated` — a fact that is both expired and
    /// past-`t_invalid` classifies as `Expired`. Catches a chain re-order that put
    /// the `t_invalid` check first.
    #[test]
    fn determine_state_expired_beats_invalidated() {
        let now = Utc::now();
        let mut fact = baseline_fact(now);
        fact.t_expired = Some(now - Duration::hours(1));
        fact.t_invalid = Some(now - Duration::hours(2));

        let state = determine_state(&fact, now);
        assert_eq!(
            state,
            FactState::Expired {
                reason: ExpiredReason::Unknown
            }
        );
    }

    /// #458: priority `Invalidated > Pinned` — a pinned fact whose `t_invalid` is in
    /// the past classifies as `Invalidated`, not `Pinned`. Catches a chain re-order
    /// that put `is_pinned` before the `t_invalid` check.
    #[test]
    fn determine_state_invalidated_beats_pinned() {
        let now = Utc::now();
        let t_invalid = now - Duration::hours(2);
        let mut fact = baseline_fact(now);
        fact.t_invalid = Some(t_invalid);
        fact.is_pinned = true;

        let state = determine_state(&fact, now);
        assert_eq!(state, FactState::Invalidated { t_invalid });
    }

    // --- #459: build_graph_context with a connected fact (degree > 0) ------

    /// #459: a fact with >= 1 edge yields a non-trivial graph context — the
    /// existing tests only ever exercise degree-0 isolated facts. Builds a known
    /// star (center `10` linked to `20`, `30`, `40`) so degree, the *sorted*
    /// neighbour set, and the `component_size == neighbors + 1` formula are all
    /// asserted against distinct, hand-computed values.
    #[test]
    fn build_graph_context_with_neighbors() {
        let mut graph = MemoryGraph::new();
        let edge = |edge_id| EdgeData {
            edge_id,
            relation_type: "supports".into(),
            weight: 1.0,
        };
        // Insert targets out of order so a missing sort would surface.
        graph.add_edge(10, 40, edge(1)); // outgoing
        graph.add_edge(10, 20, edge(2)); // outgoing
        graph.add_edge(30, 10, edge(3)); // incoming — degree counts both directions

        let ctx = build_graph_context(&graph, 10);

        assert_eq!(ctx.degree, 3, "2 outgoing + 1 incoming");
        assert_eq!(
            ctx.neighbor_ids,
            vec![20, 30, 40],
            "all in/out neighbours, ascending"
        );
        assert_eq!(
            ctx.component_size,
            ctx.neighbor_ids.len() + 1,
            "neighbours + the fact itself"
        );
        assert_eq!(ctx.component_size, 4);
    }

    /// #901 (fix): `neighbor_ids` is the **immediate (distance-1) in+out neighbour
    /// set**, consistent with `degree` — NOT the whole weakly-connected component.
    /// `component_size` remains the separate, broader transitive-component metric.
    ///
    /// The star fixture in [`build_graph_context_with_neighbors`] cannot discriminate
    /// this: in a star every component member is also a distance-1 neighbour, so the
    /// two coincide. Here a deliberately **transitive** chain `A(10) -> B(20) -> C(30)`
    /// queried from `A` separates them: `C` is distance-2 from `A`, so it must appear
    /// in `component_size` (the transitive metric) but NOT in `neighbor_ids` (the
    /// immediate metric). This is the regression witness for the #901 fix — before it,
    /// `neighbor_ids` walked the whole component and wrongly included `C(30)`.
    #[test]
    fn build_graph_context_neighbor_ids_are_immediate_not_transitive() {
        let mut graph = MemoryGraph::new();
        let edge = |edge_id| EdgeData {
            edge_id,
            relation_type: "supports".into(),
            weight: 1.0,
        };
        // Transitive chain: A(10) -> B(20) -> C(30). C is distance-2 from A.
        graph.add_edge(10, 20, edge(1));
        graph.add_edge(20, 30, edge(2));

        let ctx = build_graph_context(&graph, 10);

        // `degree` is distance-1 in+out: A has only the single outgoing `A->B`.
        assert_eq!(ctx.degree, 1, "A has exactly one distance-1 edge (A->B)");

        // `neighbor_ids` is now the IMMEDIATE neighbour set — B(20) only. The
        // transitive distance-2 node C(30) must NOT appear (that was the #901 bug).
        assert_eq!(
            ctx.neighbor_ids,
            vec![20],
            "immediate (distance-1) neighbours only — B(20), not transitive C(30)"
        );
        assert!(
            !ctx.neighbor_ids.contains(&30),
            "transitive distance-2 node C(30) must NOT be an immediate neighbour (#901)"
        );

        // Consistency restored: the immediate neighbour set size equals `degree`
        // (both distance-1) — the divergence #901 flagged is gone.
        assert_eq!(
            ctx.neighbor_ids.len(),
            ctx.degree,
            "immediate neighbour count matches the distance-1 degree (#901 fixed)"
        );

        // `component_size` stays the broader transitive metric: A + B + C = 3, and it
        // is genuinely larger than the immediate neighbourhood (`neighbor_ids` + self).
        assert_eq!(ctx.component_size, 3, "whole transitive component A,B,C");
        assert!(
            ctx.component_size > ctx.neighbor_ids.len() + 1,
            "component_size (transitive) is broader than the immediate neighbourhood"
        );
    }

    /// #459: a node present in the graph but with no edges has degree 0, no
    /// neighbours, and a component of just itself — the asymmetric counterpart to
    /// the connected case above (guards `component_size` against a stray +1).
    #[test]
    fn build_graph_context_isolated_present_node() {
        let mut graph = MemoryGraph::new();
        // Give the graph some unrelated structure, then probe a disconnected node.
        graph.add_edge(
            1,
            2,
            EdgeData {
                edge_id: 1,
                relation_type: "r".into(),
                weight: 1.0,
            },
        );
        // Materialize an isolated-but-present node 99 via the public API: add a
        // self-loop, then remove its edges — `remove_edges_by_fact` drops the edge
        // but keeps the node (documented post-condition). `MemoryGraph::ensure_node`
        // itself is `pub(crate)` inside `me-index` (Wave 2 #816 / S2), so this
        // facade test can no longer reach it directly across the crate boundary.
        graph.add_edge(
            99,
            99,
            EdgeData {
                edge_id: 2,
                relation_type: "self".into(),
                weight: 1.0,
            },
        );
        graph.remove_edges_by_fact(99);

        let ctx = build_graph_context(&graph, 99);
        assert_eq!(ctx.degree, 0);
        assert!(ctx.neighbor_ids.is_empty());
        assert_eq!(ctx.component_size, 1);
    }

    // --- #460: fact_history Expired timeline entry -------------------------

    /// #460: a fact with `t_expired` set yields a timeline that **includes** the
    /// `Expired` event — the existing `fact_history_*` tests never set `t_expired`,
    /// so this branch was uncovered. Realistic, chronological timestamps here.
    #[test]
    fn fact_history_includes_expired_entry() {
        let now = Utc::now();
        let t_created = now - Duration::hours(10);
        let t_valid = now - Duration::hours(8);
        let t_invalid = now - Duration::hours(4);
        let t_expired = now - Duration::hours(1);

        let mut fact = baseline_fact(now);
        fact.t_created = t_created;
        fact.t_valid = Some(t_valid);
        fact.t_invalid = Some(t_invalid);
        fact.t_expired = Some(t_expired);

        let history = fact_history_from_fact(42, &fact);
        assert_eq!(history.fact_id, 42);
        assert_eq!(history.timeline.len(), 4);

        // The Expired entry is present and carries `t_expired` verbatim — a missing
        // `if let Some(t_expired)` push, or echoing the wrong timestamp, fails here.
        let expired_entry = history
            .timeline
            .iter()
            .find(|e| matches!(e.kind, HistoryEventKind::Expired))
            .expect("Expired entry present");
        assert_eq!(expired_entry.timestamp, t_expired);
    }

    /// #460: the timeline is sorted **ascending** by timestamp regardless of the
    /// fixed push order (`Created → BecameValid → BecameInvalid → Expired`). To make
    /// the `sort_by_key` load-bearing — and not silently satisfied because the push
    /// order already happens to be chronological — `t_created` is deliberately the
    /// *latest* instant, so a dropped sort would leave `Created` at index 0 and the
    /// kind-vector assertion would fail.
    #[test]
    fn fact_history_sort_is_load_bearing() {
        let base = Utc::now() - Duration::hours(20);
        // Chronological order: valid < invalid < expired < created (created last).
        let t_valid = base + Duration::hours(1);
        let t_invalid = base + Duration::hours(2);
        let t_expired = base + Duration::hours(3);
        let t_created = base + Duration::hours(9);

        let mut fact = baseline_fact(Utc::now());
        fact.t_created = t_created;
        fact.t_valid = Some(t_valid);
        fact.t_invalid = Some(t_invalid);
        fact.t_expired = Some(t_expired);

        let history = fact_history_from_fact(7, &fact);
        assert_eq!(history.timeline.len(), 4);

        // Sorted by timestamp, `Created` (latest) must land LAST — the inverse of
        // the push order, so a dropped sort flips this assertion.
        let kinds: Vec<&HistoryEventKind> = history.timeline.iter().map(|e| &e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                &HistoryEventKind::BecameValid,
                &HistoryEventKind::BecameInvalid,
                &HistoryEventKind::Expired,
                &HistoryEventKind::Created,
            ],
            "timeline must be sorted ascending by timestamp, not push order"
        );

        // Timestamps are non-decreasing.
        assert!(
            history
                .timeline
                .windows(2)
                .all(|w| w[0].timestamp <= w[1].timestamp),
            "timeline must be sorted ascending"
        );
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
                Arc::new(crate::test_utils::MockEmbedder::fixed4()),
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
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
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
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
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
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
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
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
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
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
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
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
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
                std::sync::Arc::new(crate::test_utils::MockEmbedder::fixed4())
                    as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        let explanation = engine.explain_fact(id).await.unwrap();
        insta::assert_yaml_snapshot!(explanation);
    }
}
