//! Integration test for `MemoryEngine::list_recent_insights` (#225).
//!
//! Public-API only: add facts carrying the insight marker across a nested scope
//! subtree and assert subtree-scoped, newest-first, limited, active-only retrieval.

#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::{
    AddFactOptions, AddFactRequest, EmbeddingFingerprint, EmbeddingProvider, FactType,
    INSIGHT_MARKER_KEY, MemoryEngine, MemoryError,
};

const DIM: usize = 4;

struct FixedEmbed;
impl EmbeddingProvider for FixedEmbed {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", 4)
    }
}

/// Add a fact, optionally insight-marked, at `scope`, with an explicit `t_created`
/// (controls recency ordering).
fn add(engine: &MemoryEngine, content: &str, scope: &str, marked: bool, t_created: &str) -> i64 {
    let metadata = if marked {
        Some(serde_json::json!({ INSIGHT_MARKER_KEY: { "flushed_at": t_created } }))
    } else {
        Some(serde_json::json!({ "source": "manual" }))
    };
    let req = AddFactRequest {
        content: content.into(),
        fact_type: FactType::Semantic,
        source_event_id: None,
        scope: Some(scope.into()),
        opts: Some(AddFactOptions {
            metadata,
            t_created: Some(t_created.parse().unwrap()),
            ..Default::default()
        }),
    };
    engine.add_fact(&req, &FixedEmbed, None).unwrap()
}

#[test]
fn list_recent_insights_subtree_newest_first_limited_active_only() {
    let engine = MemoryEngine::builder(DIM).build().unwrap();

    // Marked insights at the project node and a child node (subtree).
    let at_root = add(
        &engine,
        "insight at p",
        "project:p",
        true,
        "2024-01-01T00:00:00Z",
    );
    let at_child = add(
        &engine,
        "insight at p/sub",
        "project:p/sub",
        true,
        "2024-06-01T00:00:00Z",
    );
    // An unmarked fact in-scope → excluded.
    add(
        &engine,
        "plain fact",
        "project:p",
        false,
        "2024-07-01T00:00:00Z",
    );
    // A marked insight in a DIFFERENT project → excluded by subtree scoping.
    add(
        &engine,
        "other project",
        "project:other",
        true,
        "2024-08-01T00:00:00Z",
    );

    // Subtree of project:p, newest-first → child (Jun) before root (Jan); excludes plain + other.
    let got: Vec<i64> = engine
        .list_recent_insights("project:p", 10)
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(
        got,
        vec![at_child, at_root],
        "subtree, marked-only, newest-first"
    );

    // limit truncates to newest.
    let top1: Vec<i64> = engine
        .list_recent_insights("project:p", 1)
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(top1, vec![at_child]);

    // Exact child scope returns only the child's insight.
    let child: Vec<i64> = engine
        .list_recent_insights("project:p/sub", 10)
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(child, vec![at_child]);

    // Unknown project → empty (not an error).
    assert!(
        engine
            .list_recent_insights("project:does-not-exist", 10)
            .unwrap()
            .is_empty()
    );
}
