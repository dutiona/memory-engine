use memory_engine::ResumeContext;
use memory_engine::inspect_types::{FactExplanation, FactHistory};
use memory_engine::types::{Activity, Event, Fact, ProjectContext, SessionCheckpoint};
use memory_engine::{QueryDiagnostics, SearchResult};
use rmcp::model::ErrorData;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Serialize a value into a [`serde_json::Value`], mapping a serialization
/// failure to an MCP internal error instead of silently degrading the output.
///
/// The shapers below embed enum variants (`MatchType`, `FactState`,
/// `HistoryEventKind`) into the response JSON. Using `serde_json::to_value`
/// rather than `format!("{:?}", _)` keeps the wire shape tied to the type's
/// serde contract (stable across compiler versions, and structurally correct
/// for data-carrying variants such as `FactState::Expired { reason }`). The
/// fallible result is propagated to the caller — a serialize error surfaces as
/// a tool error rather than emitting `null` or an unstable `Debug` string.
fn to_value<T: Serialize>(value: &T) -> Result<Value, ErrorData> {
    serde_json::to_value(value)
        .map_err(|e| ErrorData::internal_error(format!("serialization error: {e}"), None))
}

/// Tiered retrieval depth for MCP responses.
///
/// Controls how much detail is included in tool results to manage token budget.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Depth {
    /// ~15 tokens/fact: id, truncated content, `importance_score`, `scope_path`.
    Sparse,
    /// ~75 tokens/fact: all fields except embedding and `content_hash`. Default.
    #[default]
    Standard,
    /// ~300+ tokens/fact: everything including provenance, event history, graph context.
    Full,
}

/// Maximum content length for sparse depth truncation.
const SPARSE_CONTENT_MAX: usize = 200;

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Find a valid UTF-8 boundary near max.
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Shape a [`Fact`] according to the requested depth.
#[must_use]
pub fn shape_fact(fact: &Fact, depth: Depth, scope_path: Option<&str>) -> Value {
    match depth {
        Depth::Sparse => json!({
            "id": fact.id,
            "content": truncate(&fact.content, SPARSE_CONTENT_MAX),
            "importance_score": fact.importance_score,
            "scope": scope_path.unwrap_or(""),
        }),
        Depth::Standard => json!({
            "id": fact.id,
            "content": fact.content,
            "fact_type": fact.fact_type,
            "base_importance": fact.base_importance,
            "importance_score": fact.importance_score,
            "is_pinned": fact.is_pinned,
            "scope_id": fact.scope_id,
            "scope": scope_path.unwrap_or(""),
            "t_created": fact.t_created,
            "t_expired": fact.t_expired,
            "t_valid": fact.t_valid,
            "t_invalid": fact.t_invalid,
            "source_event_id": fact.source_event_id,
            "access_count": fact.access_count,
            "last_accessed": fact.last_accessed,
            "surfaced_at": fact.surfaced_at,
            "metadata": fact.metadata,
        }),
        Depth::Full => {
            // Full includes everything standard has, plus embedding dimensions.
            json!({
                "id": fact.id,
                "content": fact.content,
                "content_hash": fact.content_hash,
                "fact_type": fact.fact_type,
                "base_importance": fact.base_importance,
                "importance_score": fact.importance_score,
                "is_pinned": fact.is_pinned,
                "scope_id": fact.scope_id,
                "scope": scope_path.unwrap_or(""),
                "t_created": fact.t_created,
                "t_expired": fact.t_expired,
                "t_valid": fact.t_valid,
                "t_invalid": fact.t_invalid,
                "source_event_id": fact.source_event_id,
                "access_count": fact.access_count,
                "last_accessed": fact.last_accessed,
                "surfaced_at": fact.surfaced_at,
                "metadata": fact.metadata,
                "embedding_dim": fact.embedding.len(),
            })
        }
    }
}

/// Shape a [`SearchResult`] according to the requested depth.
///
/// Fallible: `match_type` is serialized via the type's serde contract (not
/// `Debug`), so a serialization failure is propagated rather than masked.
pub fn shape_search_result(
    result: &SearchResult,
    depth: Depth,
    scope_path: Option<&str>,
) -> Result<Value, ErrorData> {
    let mut shaped = shape_fact(&result.fact, depth, scope_path);
    if let Value::Object(ref mut map) = shaped {
        map.insert("score".to_owned(), json!(result.score));
        map.insert("match_type".to_owned(), to_value(&result.match_type)?);
    }
    Ok(shaped)
}

/// Shape a [`FactExplanation`] according to the requested depth.
///
/// Fallible: the `state` field (and, at [`Depth::Full`], the whole explanation)
/// is serialized via the type's serde contract, so a serialization failure is
/// propagated to the caller rather than masked as `null` or an unstable `Debug`
/// string. [`FactState`](memory_engine::inspect_types::FactState) is a
/// data-carrying enum, so `Debug` and serde diverge for non-`Active` variants.
pub fn shape_explanation(explanation: &FactExplanation, depth: Depth) -> Result<Value, ErrorData> {
    match depth {
        Depth::Sparse => Ok(json!({
            "fact_id": explanation.fact_id,
            "state": to_value(&explanation.state)?,
            "scope_path": explanation.scope_path,
        })),
        Depth::Standard => Ok(json!({
            "fact_id": explanation.fact_id,
            "state": to_value(&explanation.state)?,
            "scope_path": explanation.scope_path,
            "provenance": {
                "source_event_id": explanation.provenance.source_event_id,
                "base_importance": explanation.provenance.base_importance,
                "importance_score": explanation.provenance.importance_score,
                "is_pinned": explanation.provenance.is_pinned,
                "access_count": explanation.provenance.access_count,
            },
            "graph": {
                "degree": explanation.graph_context.degree,
                "component_size": explanation.graph_context.component_size,
            },
        })),
        // Full: serialize the entire explanation including source_event. A
        // serialization failure (e.g. a non-finite float in a nested field) is
        // propagated as an MCP internal error — surfacing the fault is preferable
        // to silently emitting `null` for what the caller asked to inspect.
        Depth::Full => to_value(explanation),
    }
}

/// Shape a [`ResumeContext`] according to the requested depth.
///
/// Each tier's facts are shaped independently.
#[must_use]
pub fn shape_resume_context(ctx: &ResumeContext, depth: Depth) -> Value {
    let shape_vec = |facts: &[Fact]| -> Vec<Value> {
        facts.iter().map(|f| shape_fact(f, depth, None)).collect()
    };

    json!({
        "pinned": shape_vec(&ctx.pinned),
        "high_importance": shape_vec(&ctx.high_importance),
        "due": shape_vec(&ctx.due),
        "recent": shape_vec(&ctx.recent),
    })
}

/// Shape an [`Event`] according to the requested depth.
///
/// `scope_path` is resolved externally (via `engine.get_scope_path()`) to provide
/// human-readable context, consistent with other shapers.
#[must_use]
pub fn shape_event(event: &Event, depth: Depth, scope_path: Option<&str>) -> Value {
    match depth {
        Depth::Sparse => json!({
            "id": event.id,
            "event_type": event.event_type,
            "timestamp": event.timestamp,
            "scope": scope_path.unwrap_or(""),
        }),
        Depth::Standard => json!({
            "id": event.id,
            "event_type": event.event_type,
            "timestamp": event.timestamp,
            "source": event.source,
            "session_id": event.session_id,
            "scope": scope_path.unwrap_or(""),
        }),
        Depth::Full => json!({
            "id": event.id,
            "event_type": event.event_type,
            "timestamp": event.timestamp,
            "source": event.source,
            "session_id": event.session_id,
            "scope_id": event.scope_id,
            "scope": scope_path.unwrap_or(""),
            "payload": event.payload,
            "origin_node_id": event.origin_node_id,
            "sequence_id": event.sequence_id,
            "created_at": event.created_at,
            "event_revision": event.event_revision,
        }),
    }
}

/// Shape [`QueryDiagnostics`] according to the requested depth.
///
/// - **Sparse**: minimal — just result count and expired matches.
/// - **Standard / Full**: all diagnostic fields.
#[must_use]
pub fn shape_diagnostics(diagnostics: &QueryDiagnostics, depth: Depth) -> Value {
    match depth {
        Depth::Sparse => json!({
            "results_returned": diagnostics.results_returned,
            "expired_matches": diagnostics.expired_matches,
        }),
        Depth::Standard | Depth::Full => json!({
            "candidates_before_filter": diagnostics.candidates_before_filter,
            "results_returned": diagnostics.results_returned,
            "expired_matches": diagnostics.expired_matches,
            "fts_candidates": diagnostics.fts_candidates,
            "vector_candidates": diagnostics.vector_candidates,
        }),
    }
}

/// Shape a [`FactHistory`] according to the requested depth.
///
/// Fallible: each timeline entry's `kind` is serialized via the type's serde
/// contract (not `Debug`), so a serialization failure is propagated rather than
/// masked.
pub fn shape_fact_history(history: &FactHistory, depth: Depth) -> Result<Value, ErrorData> {
    match depth {
        Depth::Sparse => Ok(json!({
            "fact_id": history.fact_id,
            "event_count": history.timeline.len(),
        })),
        Depth::Standard | Depth::Full => {
            let timeline: Vec<Value> = history
                .timeline
                .iter()
                .map(|entry| {
                    Ok(json!({
                        "timestamp": entry.timestamp,
                        "kind": to_value(&entry.kind)?,
                    }))
                })
                .collect::<Result<_, ErrorData>>()?;
            Ok(json!({
                "fact_id": history.fact_id,
                "timeline": timeline,
            }))
        }
    }
}

/// Shape an [`Activity`] according to the requested depth.
#[must_use]
pub fn shape_activity(activity: &Activity, depth: Depth) -> Value {
    match depth {
        Depth::Sparse => json!({
            "id": activity.id,
            "tool_name": activity.tool_name,
            "status": activity.status.to_string(),
            "last_seen": activity.last_seen,
        }),
        Depth::Standard => json!({
            "id": activity.id,
            "session_id": activity.session_id,
            "tool_name": activity.tool_name,
            "result_summary": activity.result_summary,
            "outcome_class": activity.outcome_class,
            "status": activity.status.to_string(),
            "occurrence_count": activity.occurrence_count,
            "first_seen": activity.first_seen,
            "last_seen": activity.last_seen,
            "scope_id": activity.scope_id,
        }),
        Depth::Full => json!({
            "id": activity.id,
            "session_id": activity.session_id,
            "tool_name": activity.tool_name,
            "args_hash": activity.args_hash,
            "args": activity.args,
            "result_summary": activity.result_summary,
            "outcome_class": activity.outcome_class,
            "status": activity.status.to_string(),
            "occurrence_count": activity.occurrence_count,
            "first_seen": activity.first_seen,
            "last_seen": activity.last_seen,
            "scope_id": activity.scope_id,
            "promoted_fact_id": activity.promoted_fact_id,
        }),
    }
}

/// Shape a [`SessionCheckpoint`] according to the requested depth.
#[must_use]
pub fn shape_checkpoint(checkpoint: &SessionCheckpoint, depth: Depth) -> Value {
    match depth {
        Depth::Sparse => json!({
            "session_id": checkpoint.session_id,
            "scope_path": checkpoint.scope_path,
            "checkpoint_at": checkpoint.checkpoint_at,
        }),
        Depth::Standard | Depth::Full => json!({
            "session_id": checkpoint.session_id,
            "scope_path": checkpoint.scope_path,
            "summary": checkpoint.summary,
            "last_activity_id": checkpoint.last_activity_id,
            "checkpoint_at": checkpoint.checkpoint_at,
            "metadata": checkpoint.metadata,
        }),
    }
}

/// Shape a [`ProjectContext`] according to the requested depth.
#[must_use]
pub fn shape_project_context(ctx: &ProjectContext, depth: Depth) -> Value {
    let activities: Vec<Value> = ctx
        .recent_activities
        .iter()
        .map(|a| shape_activity(a, depth))
        .collect();
    let facts: Vec<Value> = ctx
        .relevant_facts
        .iter()
        .map(|f| shape_fact(f, depth, None))
        .collect();
    let checkpoint = ctx
        .last_checkpoint
        .as_ref()
        .map(|cp| shape_checkpoint(cp, depth));

    json!({
        "scope_path": ctx.scope_path,
        "recent_activities": activities,
        "last_checkpoint": checkpoint,
        "relevant_facts": facts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use memory_engine::types::FactType;

    fn make_test_fact() -> Fact {
        Fact {
            id: 42,
            content:
                "This is a test fact with enough content to test truncation behavior in sparse mode"
                    .into(),
            content_hash: "abc123".into(),
            embedding: vec![0.1; 384],
            fact_type: FactType::Semantic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: Some(10),
            base_importance: 0.8,
            access_count: 5,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({"topic": "test"}),
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.85,
            surfaced_at: None,
        }
    }

    #[test]
    fn sparse_truncates_and_minimal_fields() {
        let fact = make_test_fact();
        let shaped = shape_fact(&fact, Depth::Sparse, Some("/test"));
        let obj = shaped.as_object().unwrap();
        assert_eq!(obj.len(), 4); // id, content, importance_score, scope
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("content"));
        assert!(obj.contains_key("importance_score"));
        assert!(obj.contains_key("scope"));
    }

    #[test]
    fn standard_excludes_embedding_and_hash() {
        let fact = make_test_fact();
        let shaped = shape_fact(&fact, Depth::Standard, None);
        let obj = shaped.as_object().unwrap();
        assert!(!obj.contains_key("embedding"));
        assert!(!obj.contains_key("content_hash"));
        assert!(!obj.contains_key("embedding_dim"));
        assert!(obj.contains_key("content"));
        assert!(obj.contains_key("fact_type"));
        assert!(obj.contains_key("metadata"));
    }

    #[test]
    fn full_includes_embedding_dim_and_hash() {
        let fact = make_test_fact();
        let shaped = shape_fact(&fact, Depth::Full, None);
        let obj = shaped.as_object().unwrap();
        assert!(obj.contains_key("content_hash"));
        assert!(obj.contains_key("embedding_dim"));
        assert_eq!(obj["embedding_dim"], 384);
    }

    #[test]
    fn search_result_adds_score_and_match_type() {
        use memory_engine::MatchType;
        let result = SearchResult {
            fact: make_test_fact(),
            score: 0.95,
            match_type: MatchType::Fts,
        };
        let shaped = shape_search_result(&result, Depth::Sparse, None).unwrap();
        let obj = shaped.as_object().unwrap();
        assert!(obj.contains_key("score"));
        assert!(obj.contains_key("match_type"));
        // serde serialization of the unit variant is a plain string, not Debug.
        assert_eq!(obj["match_type"], "Fts");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "hello 🌍 world";
        let truncated = truncate(s, 8);
        // Should not split the emoji
        assert!(truncated.len() <= 8);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    // --- Event shaping tests ---

    fn make_test_event() -> Event {
        use memory_engine::types::EventType;
        Event {
            id: 7,
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"role": "user", "text": "hello"}),
            source: "test-agent".into(),
            session_id: Some("sess-001".into()),
            scope_id: 2,
            origin_node_id: "node-1".into(),
            sequence_id: 42,
            created_at: Some(Utc::now()),
            event_revision: 1,
        }
    }

    #[test]
    fn shape_event_sparse_has_4_fields() {
        let event = make_test_event();
        let shaped = shape_event(&event, Depth::Sparse, Some("project/test"));
        let obj = shaped.as_object().unwrap();
        assert_eq!(obj.len(), 4);
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("event_type"));
        assert!(obj.contains_key("timestamp"));
        assert!(obj.contains_key("scope"));
        // payload must NOT be present at sparse depth
        assert!(!obj.contains_key("payload"));
    }

    #[test]
    fn shape_event_standard_includes_source_excludes_payload() {
        let event = make_test_event();
        let shaped = shape_event(&event, Depth::Standard, None);
        let obj = shaped.as_object().unwrap();
        assert!(obj.contains_key("source"));
        assert!(obj.contains_key("session_id"));
        assert!(!obj.contains_key("payload"));
        assert!(!obj.contains_key("origin_node_id"));
    }

    #[test]
    fn shape_event_full_includes_payload() {
        let event = make_test_event();
        let shaped = shape_event(&event, Depth::Full, Some("root"));
        let obj = shaped.as_object().unwrap();
        assert!(obj.contains_key("payload"));
        assert!(obj.contains_key("origin_node_id"));
        assert!(obj.contains_key("sequence_id"));
        assert!(obj.contains_key("created_at"));
        assert!(obj.contains_key("event_revision"));
        assert_eq!(obj["scope"], "root");
    }

    // --- FactHistory shaping tests ---

    fn make_test_history() -> FactHistory {
        use memory_engine::inspect_types::{FactHistoryEntry, HistoryEventKind};
        FactHistory {
            fact_id: 42,
            timeline: vec![
                FactHistoryEntry {
                    timestamp: Utc::now(),
                    kind: HistoryEventKind::Created,
                },
                FactHistoryEntry {
                    timestamp: Utc::now(),
                    kind: HistoryEventKind::BecameValid,
                },
            ],
        }
    }

    #[test]
    fn shape_fact_history_sparse_only_count() {
        let history = make_test_history();
        let shaped = shape_fact_history(&history, Depth::Sparse).unwrap();
        let obj = shaped.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(obj["fact_id"], 42);
        assert_eq!(obj["event_count"], 2);
        assert!(!obj.contains_key("timeline"));
    }

    #[test]
    fn shape_fact_history_standard_includes_timeline() {
        let history = make_test_history();
        let shaped = shape_fact_history(&history, Depth::Standard).unwrap();
        let obj = shaped.as_object().unwrap();
        assert_eq!(obj["fact_id"], 42);
        let timeline = obj["timeline"].as_array().unwrap();
        assert_eq!(timeline.len(), 2);
        // serde serialization of the unit variants is a plain string, not Debug.
        assert_eq!(timeline[0]["kind"], "Created");
        assert_eq!(timeline[1]["kind"], "BecameValid");
    }

    // --- Property-based tests (#471) ---

    proptest::proptest! {
        /// `truncate` must return the *longest valid char-boundary prefix* of `s`
        /// that fits within `max` bytes, for *any* input string and any cap in
        /// 0..500. Pinning prefix-maximality — not merely `len <= max` + boundary,
        /// both of which an always-empty / off-by-one mutant would satisfy — is what
        /// kills those mutants. The byte-boundary back-off loop exists precisely to
        /// uphold this contract; slicing on a non-boundary would panic at runtime.
        #[test]
        fn truncate_respects_cap_and_char_boundary(s in ".*", max in 0_usize..500) {
            let out = truncate(&s, max);
            proptest::prop_assert!(out.len() <= max, "len {} > max {}", out.len(), max);
            proptest::prop_assert!(s.starts_with(out), "output is not a prefix of input");
            if s.len() <= max {
                // Fits: returned verbatim, no truncation.
                proptest::prop_assert_eq!(out, &s[..]);
            } else {
                // Truncated: the prefix is maximal — appending the next char of the
                // input would overflow `max`, so we did not back off further than
                // necessary.
                let next = out.len() + s[out.len()..].chars().next().map_or(0, char::len_utf8);
                proptest::prop_assert!(next > max, "backed off further than necessary");
            }
        }
    }

    // --- FactExplanation shaping tests ------------------------------------

    fn make_test_explanation(state: memory_engine::inspect_types::FactState) -> FactExplanation {
        use memory_engine::inspect_types::{FactProvenance, GraphContext};
        FactExplanation {
            fact_id: 42,
            state,
            provenance: FactProvenance {
                source_event_id: Some(10),
                source_event: None,
                base_importance: 0.8,
                importance_score: 0.85,
                is_pinned: false,
                access_count: 5,
            },
            graph_context: GraphContext {
                degree: 2,
                neighbor_ids: vec![1, 2],
                component_size: 4,
            },
            scope_path: "project/test".into(),
        }
    }

    /// Regression guard for the serde-vs-`Debug` serialization of a
    /// **data-carrying** `FactState` variant. All other shaping tests use unit
    /// variants (`Active`/`Fts`/`Created`) where `format!("{:?}", _)` and serde
    /// coincide; this is the one that diverges. The expected shape is the
    /// externally-tagged serde encoding of
    /// `FactState::Expired { reason: ExpiredReason::Forgotten }` —
    /// `{"Expired": {"reason": "Forgotten"}}`, where the inner unit variant
    /// `ExpiredReason::Forgotten` serializes to the string `"Forgotten"`.
    /// Reverting `to_value` to `format!("{:?}", _)` would instead yield the
    /// unstable Debug string `"Expired { reason: Forgotten }"`, failing this test.
    #[test]
    fn shape_explanation_serializes_data_carrying_state_via_serde() {
        use memory_engine::inspect_types::{ExpiredReason, FactState};
        let explanation = make_test_explanation(FactState::Expired {
            reason: ExpiredReason::Forgotten,
        });
        let shaped = shape_explanation(&explanation, Depth::Sparse).unwrap();
        // Externally-tagged enum: variant name is the key, struct fields nest under it.
        assert_eq!(shaped["state"]["Expired"]["reason"], json!("Forgotten"));
        // A `format!("{:?}", _)` regression would make `state` a plain string,
        // so the structured lookup above would be `Value::Null` and the assert
        // would fail. Pin the structural shape explicitly too.
        assert!(shaped["state"].is_object());
        assert!(!shaped["state"].is_string());
    }
}
