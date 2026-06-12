//! Integration test: `record_activity` → `checkpoint_session` → `load_context` cycle.

use chrono::Utc;
use memory_engine::{ActivityFilterConfig, ActivityStatus, MemoryEngine, RecordActivityRequest};

/// Minimal embedder for testing (returns zero-vector of fixed dim).
struct ZeroEmbedder(usize);

impl memory_engine::EmbeddingProvider for ZeroEmbedder {
    fn embed(&self, _text: &str) -> memory_engine::error::Result<Vec<f32>> {
        Ok(vec![0.0; self.0])
    }
}

const DIM: usize = 4;

fn engine() -> MemoryEngine {
    MemoryEngine::open_memory(DIM).unwrap()
}

#[test]
fn record_activity_stores_and_deduplicates() {
    let engine = engine();
    let config = ActivityFilterConfig::default();

    let req = RecordActivityRequest {
        tool_name: "Read".into(),
        args: serde_json::json!({"path": "/foo/bar.rs"}),
        result: Some("200 lines".into()),
        session_id: "sess-1".into(),
        timestamp: Utc::now(),
        scope_path: None,
        outcome_class: None,
    };

    let r1 = engine.record_activity(&req, None, &config).unwrap();
    assert!(r1.activity_id.is_some());
    assert!(!r1.was_deduplicated);
    assert_eq!(r1.status, ActivityStatus::Recorded);

    // Second call within dedup window should deduplicate.
    let r2 = engine.record_activity(&req, None, &config).unwrap();
    assert!(r2.was_deduplicated);
    assert_eq!(r2.activity_id, r1.activity_id);
    assert_eq!(r2.status, ActivityStatus::Deduplicated);
}

#[test]
fn record_activity_ignore_drops_silently() {
    let engine = engine();
    let config = ActivityFilterConfig {
        ignore_patterns: vec!["format".into()],
        ..Default::default()
    };

    let req = RecordActivityRequest {
        tool_name: "FormatCode".into(),
        args: serde_json::json!({}),
        result: None,
        session_id: "sess-1".into(),
        timestamp: Utc::now(),
        scope_path: None,
        outcome_class: None,
    };

    let result = engine.record_activity(&req, None, &config).unwrap();
    assert!(result.activity_id.is_none());
    assert_eq!(result.status, ActivityStatus::Ignored);
}

#[test]
fn record_activity_promotes_to_fact() {
    let engine = engine();
    let embedder = ZeroEmbedder(DIM);
    let config = ActivityFilterConfig {
        promote_patterns: vec!["commit".into()],
        ..Default::default()
    };

    let req = RecordActivityRequest {
        tool_name: "git_commit".into(),
        args: serde_json::json!({"msg": "feat: add feature"}),
        result: Some("committed abc123".into()),
        session_id: "sess-1".into(),
        timestamp: Utc::now(),
        scope_path: None,
        outcome_class: None,
    };

    let result = engine
        .record_activity(&req, Some(&embedder), &config)
        .unwrap();
    assert_eq!(result.status, ActivityStatus::Promoted);
    assert!(result.promoted_fact_id.is_some());

    // Verify the fact was actually created.
    let fact = engine.get_fact(result.promoted_fact_id.unwrap()).unwrap();
    assert!(fact.content.contains("git_commit"));
    assert!(fact.content.contains("committed abc123"));
}

#[test]
fn checkpoint_and_load_context_cycle() {
    let engine = engine();
    let config = ActivityFilterConfig::default();

    // Create a scope first.
    engine.ensure_scope_path("project:test").unwrap();

    // Record some activities.
    for i in 0..5 {
        let req = RecordActivityRequest {
            tool_name: format!("Tool{i}"),
            args: serde_json::json!({"i": i}),
            result: Some(format!("result {i}")),
            session_id: "sess-1".into(),
            timestamp: Utc::now(),
            scope_path: Some("project:test".into()),
            outcome_class: None,
        };
        engine.record_activity(&req, None, &config).unwrap();
    }

    // Checkpoint.
    engine
        .checkpoint_session("sess-1", Some("project:test"), Some("test session"), None)
        .unwrap();

    // Load context.
    let ctx = engine.load_context("project:test", 10, 5).unwrap();
    assert_eq!(ctx.scope_path, "project:test");
    assert_eq!(ctx.recent_activities.len(), 5);
    assert!(ctx.last_checkpoint.is_some());
    let cp = ctx.last_checkpoint.unwrap();
    assert_eq!(cp.session_id, "sess-1");
    assert_eq!(cp.summary, Some("test session".into()));
}

#[test]
fn load_context_nonexistent_scope_errors() {
    let engine = engine();
    let err = engine.load_context("nonexistent:scope", 10, 5).unwrap_err();
    assert!(matches!(
        err,
        memory_engine::error::MemoryError::NotFound(_)
    ));
}
