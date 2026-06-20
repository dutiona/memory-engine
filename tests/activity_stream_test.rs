//! Integration test: `record_activity` → `checkpoint_session` → `load_context` cycle.

use chrono::Utc;
use memory_engine::{ActivityFilterConfig, ActivityStatus, MemoryEngine, RecordActivityRequest};

/// Minimal embedder for testing (returns zero-vector of fixed dim).
struct ZeroEmbedder(usize);

impl memory_engine::EmbeddingProvider for ZeroEmbedder {
    fn embed(&self, _text: &str) -> memory_engine::error::Result<Vec<f32>> {
        Ok(vec![0.0; self.0])
    }
    fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
        memory_engine::EmbeddingFingerprint::new("mock", "test", self.0)
    }
}

/// Embedder that always fails — exercises the promotion-failure fallback path
/// in `record_activity` (the `Err(e)` arm that logs a warning and degrades the
/// status to `Recorded` rather than propagating or leaving an orphan fact).
struct FailingEmbedder(usize);

impl memory_engine::EmbeddingProvider for FailingEmbedder {
    fn embed(&self, _text: &str) -> memory_engine::error::Result<Vec<f32>> {
        Err(memory_engine::error::MemoryError::Internal(
            "forced embedding failure".into(),
        ))
    }
    fn fingerprint(&self) -> memory_engine::EmbeddingFingerprint {
        memory_engine::EmbeddingFingerprint::new("mock", "test", self.0)
    }
}

const DIM: usize = 4;

fn engine() -> MemoryEngine {
    MemoryEngine::builder(DIM).build().unwrap()
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
    let config = ActivityFilterConfig::new(300, ["format".to_string()], []);

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
    let config = ActivityFilterConfig::new(300, [], ["commit".to_string()]);

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
fn record_activity_promote_failure_falls_back_to_recorded() {
    // When a promote-matched activity's embedding fails, promotion must degrade
    // gracefully: the activity is still recorded, no fact id is surfaced, and —
    // critically — no orphan fact is left in the store (the failed `add_fact`
    // must not commit a partial fact).
    let engine = engine();
    let embedder = FailingEmbedder(DIM);
    let config = ActivityFilterConfig::new(300, [], ["commit".to_string()]);

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

    // Activity itself was persisted (the failure is only in the promotion step).
    assert!(result.activity_id.is_some());
    assert!(!result.was_deduplicated);
    // Promotion failed → fall back to Recorded with no promoted fact id.
    assert_eq!(result.status, ActivityStatus::Recorded);
    assert!(result.promoted_fact_id.is_none());

    // No orphan fact: the failed promotion left zero facts in the store.
    let stats = engine.statistics().unwrap();
    assert_eq!(
        stats.facts.total, 0,
        "promotion failure must not orphan a fact"
    );
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
