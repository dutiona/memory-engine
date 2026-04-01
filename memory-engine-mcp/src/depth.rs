use memory_engine::inspect_types::{FactExplanation, FactHistory};
use memory_engine::search::hybrid::{QueryDiagnostics, SearchResult};
use memory_engine::types::{Event, Fact};
use memory_engine::ResumeContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Tiered retrieval depth for MCP responses.
///
/// Controls how much detail is included in tool results to manage token budget.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Depth {
    /// ~15 tokens/fact: id, truncated content, importance_score, scope_path.
    Sparse,
    /// ~75 tokens/fact: all fields except embedding and content_hash. Default.
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
            "importance": fact.importance,
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
                "importance": fact.importance,
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
pub fn shape_search_result(result: &SearchResult, depth: Depth, scope_path: Option<&str>) -> Value {
    let mut shaped = shape_fact(&result.fact, depth, scope_path);
    if let Value::Object(ref mut map) = shaped {
        map.insert("score".to_owned(), json!(result.score));
        map.insert(
            "match_type".to_owned(),
            json!(format!("{:?}", result.match_type)),
        );
    }
    shaped
}

/// Shape a [`FactExplanation`] according to the requested depth.
pub fn shape_explanation(explanation: &FactExplanation, depth: Depth) -> Value {
    match depth {
        Depth::Sparse => json!({
            "fact_id": explanation.fact_id,
            "state": format!("{:?}", explanation.state),
            "scope_path": explanation.scope_path,
        }),
        Depth::Standard => json!({
            "fact_id": explanation.fact_id,
            "state": explanation.state,
            "scope_path": explanation.scope_path,
            "provenance": {
                "source_event_id": explanation.provenance.source_event_id,
                "importance": explanation.provenance.importance,
                "importance_score": explanation.provenance.importance_score,
                "is_pinned": explanation.provenance.is_pinned,
                "access_count": explanation.provenance.access_count,
            },
            "graph": {
                "degree": explanation.graph_context.degree,
                "component_size": explanation.graph_context.component_size,
            },
        }),
        Depth::Full => {
            // Full: serialize the entire explanation including source_event.
            serde_json::to_value(explanation).unwrap_or_else(|e| {
                tracing::error!("failed to serialize FactExplanation: {e}");
                json!(null)
            })
        }
    }
}

/// Shape a [`ResumeContext`] according to the requested depth.
///
/// Each tier's facts are shaped independently.
pub fn shape_resume_context(ctx: &ResumeContext, depth: Depth) -> Value {
    let shape_vec = |facts: &[Fact]| -> Vec<Value> {
        facts.iter().map(|f| shape_fact(f, depth, None)).collect()
    };

    json!({
        "pinned": shape_vec(&ctx.pinned),
        "high_importance": shape_vec(&ctx.high_importance),
        "due": shape_vec(&ctx.due),
        "recent": shape_vec(&ctx.recent),
        "kb_stubs": ctx.kb_stubs,
    })
}

/// Shape an [`Event`] according to the requested depth.
///
/// `scope_path` is resolved externally (via `engine.get_scope_path()`) to provide
/// human-readable context, consistent with other shapers.
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
pub fn shape_fact_history(history: &FactHistory, depth: Depth) -> Value {
    match depth {
        Depth::Sparse => json!({
            "fact_id": history.fact_id,
            "event_count": history.timeline.len(),
        }),
        Depth::Standard | Depth::Full => {
            let timeline: Vec<Value> = history
                .timeline
                .iter()
                .map(|entry| {
                    json!({
                        "timestamp": entry.timestamp,
                        "kind": format!("{:?}", entry.kind),
                    })
                })
                .collect();
            json!({
                "fact_id": history.fact_id,
                "timeline": timeline,
            })
        }
    }
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
            importance: 0.8,
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
        use memory_engine::search::hybrid::MatchType;
        let result = SearchResult {
            fact: make_test_fact(),
            score: 0.95,
            match_type: MatchType::Fts,
        };
        let shaped = shape_search_result(&result, Depth::Sparse, None);
        let obj = shaped.as_object().unwrap();
        assert!(obj.contains_key("score"));
        assert!(obj.contains_key("match_type"));
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
        let shaped = shape_fact_history(&history, Depth::Sparse);
        let obj = shaped.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(obj["fact_id"], 42);
        assert_eq!(obj["event_count"], 2);
        assert!(!obj.contains_key("timeline"));
    }

    #[test]
    fn shape_fact_history_standard_includes_timeline() {
        let history = make_test_history();
        let shaped = shape_fact_history(&history, Depth::Standard);
        let obj = shaped.as_object().unwrap();
        assert_eq!(obj["fact_id"], 42);
        let timeline = obj["timeline"].as_array().unwrap();
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0]["kind"], "Created");
        assert_eq!(timeline[1]["kind"], "BecameValid");
    }
}
