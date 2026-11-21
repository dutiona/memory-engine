//! Integration test: `record_activity` → `checkpoint_session` → `load_context` cycle.

#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

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

#[tokio::test]
async fn record_activity_stores_and_deduplicates() {
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
        touched_paths: vec![],
        cwd: None,
    };

    let r1 = engine.record_activity(&req, None, &config).await.unwrap();
    assert!(r1.activity_id.is_some());
    assert!(!r1.was_deduplicated);
    assert_eq!(r1.status, ActivityStatus::Recorded);

    // Second call within dedup window should deduplicate.
    let r2 = engine.record_activity(&req, None, &config).await.unwrap();
    assert!(r2.was_deduplicated);
    assert_eq!(r2.activity_id, r1.activity_id);
    assert_eq!(r2.status, ActivityStatus::Deduplicated);
}

#[tokio::test]
async fn record_activity_ignore_drops_silently() {
    let engine = engine();
    let config = ActivityFilterConfig::new(300, ["format"], [] as [&str; 0]);

    let req = RecordActivityRequest {
        tool_name: "FormatCode".into(),
        args: serde_json::json!({}),
        result: None,
        session_id: "sess-1".into(),
        timestamp: Utc::now(),
        scope_path: None,
        outcome_class: None,
        touched_paths: vec![],
        cwd: None,
    };

    let result = engine.record_activity(&req, None, &config).await.unwrap();
    assert!(result.activity_id.is_none());
    assert_eq!(result.status, ActivityStatus::Ignored);
}

#[tokio::test]
async fn record_activity_promotes_to_fact() {
    let engine = engine();
    let embedder = ZeroEmbedder(DIM);
    let config = ActivityFilterConfig::new(300, [] as [&str; 0], ["commit"]);

    let req = RecordActivityRequest {
        tool_name: "git_commit".into(),
        args: serde_json::json!({"msg": "feat: add feature"}),
        result: Some("committed abc123".into()),
        session_id: "sess-1".into(),
        timestamp: Utc::now(),
        scope_path: None,
        outcome_class: None,
        touched_paths: vec![],
        cwd: None,
    };

    let result = engine
        .record_activity(
            &req,
            Some(std::sync::Arc::new(embedder)
                as std::sync::Arc<dyn memory_engine::EmbeddingProvider>),
            &config,
        )
        .await
        .unwrap();
    assert_eq!(result.status, ActivityStatus::Promoted);
    assert!(result.promoted_fact_id.is_some());

    // Verify the fact was actually created.
    let fact = engine
        .get_fact(result.promoted_fact_id.unwrap())
        .await
        .unwrap();
    assert!(fact.content.contains("git_commit"));
    assert!(fact.content.contains("committed abc123"));
}

#[tokio::test]
async fn record_activity_promote_failure_falls_back_to_recorded() {
    // When a promote-matched activity's embedding fails, promotion must degrade
    // gracefully: the activity is still recorded, no fact id is surfaced, and —
    // critically — no orphan fact is left in the store (the failed `add_fact`
    // must not commit a partial fact).
    let engine = engine();
    let embedder = FailingEmbedder(DIM);
    let config = ActivityFilterConfig::new(300, [] as [&str; 0], ["commit"]);

    let req = RecordActivityRequest {
        tool_name: "git_commit".into(),
        args: serde_json::json!({"msg": "feat: add feature"}),
        result: Some("committed abc123".into()),
        session_id: "sess-1".into(),
        timestamp: Utc::now(),
        scope_path: None,
        outcome_class: None,
        touched_paths: vec![],
        cwd: None,
    };

    let result = engine
        .record_activity(
            &req,
            Some(std::sync::Arc::new(embedder)
                as std::sync::Arc<dyn memory_engine::EmbeddingProvider>),
            &config,
        )
        .await
        .unwrap();

    // Activity itself was persisted (the failure is only in the promotion step).
    assert!(result.activity_id.is_some());
    assert!(!result.was_deduplicated);
    // Promotion failed → fall back to Recorded with no promoted fact id.
    assert_eq!(result.status, ActivityStatus::Recorded);
    assert!(result.promoted_fact_id.is_none());

    // No orphan fact: the failed promotion left zero facts in the store.
    let stats = engine.statistics().await.unwrap();
    assert_eq!(
        stats.facts.total, 0,
        "promotion failure must not orphan a fact"
    );
}

#[tokio::test]
async fn checkpoint_and_load_context_cycle() {
    let engine = engine();
    let config = ActivityFilterConfig::default();

    // Create a scope first.
    engine.ensure_scope_path("project:test").await.unwrap();

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
            touched_paths: vec![],
            cwd: None,
        };
        engine.record_activity(&req, None, &config).await.unwrap();
    }

    // Checkpoint.
    engine
        .checkpoint_session("sess-1", Some("project:test"), Some("test session"), None)
        .await
        .unwrap();

    // Load context.
    let ctx = engine.load_context("project:test", 10, 5).await.unwrap();
    assert_eq!(ctx.scope_path, "project:test");
    assert_eq!(ctx.recent_activities.len(), 5);
    assert!(ctx.last_checkpoint.is_some());
    let cp = ctx.last_checkpoint.unwrap();
    assert_eq!(cp.session_id, "sess-1");
    assert_eq!(cp.summary, Some("test session".into()));
}

#[tokio::test]
async fn load_context_nonexistent_scope_errors() {
    let engine = engine();
    let err = engine
        .load_context("nonexistent:scope", 10, 5)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        memory_engine::error::MemoryError::NotFound(_)
    ));
}

/// A promotion is a derivation: once a forget's tombstone blocks the content, the same
/// activity is recorded, not promoted again (#1023).
#[tokio::test]
async fn a_forgotten_activity_promotion_is_recorded_not_promoted() {
    let engine = engine();
    let embedder = || {
        Some(std::sync::Arc::new(ZeroEmbedder(DIM))
            as std::sync::Arc<dyn memory_engine::EmbeddingProvider>)
    };
    let config = ActivityFilterConfig::new(0, [] as [&str; 0], ["commit"]);
    let req = |session: &str| RecordActivityRequest {
        tool_name: "git_commit".into(),
        args: serde_json::json!({"msg": "feat: add feature"}),
        result: Some("committed abc123".into()),
        session_id: session.into(),
        timestamp: Utc::now(),
        scope_path: None,
        outcome_class: None,
        touched_paths: vec![],
        cwd: None,
    };
    let first = engine
        .record_activity(&req("sess-1"), embedder(), &config)
        .await
        .unwrap();
    let id = first.promoted_fact_id.unwrap();
    let record = memory_engine::types::NewEvent {
        timestamp: Utc::now(),
        event_type: memory_engine::types::EventType::MemoryOp,
        payload: serde_json::json!({}),
        source: "test".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "test".into(),
        sequence_id: 0,
        created_at: None,
    };
    engine
        .forget_fact(id, &["/".into()], "wrong", "unscoped:test", &record)
        .await
        .unwrap();
    let again = engine
        .record_activity(&req("sess-2"), embedder(), &config)
        .await
        .unwrap();
    assert_eq!(again.status, ActivityStatus::Recorded);
    assert_eq!(again.promoted_fact_id, None);
    assert_eq!(engine.list_active_facts(None).await.unwrap().len(), 0);
}

// ── The capture contract (#1024, ADR-0020 §6) ─────────────────────────────────

mod capture {
    use std::sync::Arc;

    use chrono::Utc;
    use memory_engine::types::OutcomeClass;
    use memory_engine::{
        ActivityFilterConfig, ActivityStatus, MemoryEngine, RecordActivityRequest,
    };
    use serde_json::{Value, json};

    use super::{DIM, ZeroEmbedder};

    const SCOPE: &str = "project:test";
    const DOTENV: &str = ".env";

    fn aws() -> String {
        format!("AKIA{}", "IOSFODNN7EXAMPLE")
    }

    fn ghp() -> String {
        format!("ghp_{}", "0123456789abcdefghij".repeat(2))
    }

    fn request(tool: &str, args: Value, result: Option<String>) -> RecordActivityRequest {
        RecordActivityRequest {
            tool_name: tool.into(),
            args,
            result,
            session_id: "s1".into(),
            timestamp: Utc::now(),
            scope_path: Some(SCOPE.into()),
            outcome_class: None,
            touched_paths: vec![],
            cwd: None,
        }
    }

    fn embedder() -> Arc<dyn memory_engine::EmbeddingProvider> {
        Arc::new(ZeroEmbedder(DIM))
    }

    struct Store {
        dir: tempfile::TempDir,
        db: std::path::PathBuf,
        engine: MemoryEngine,
    }

    fn store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let engine = MemoryEngine::builder(DIM).path(&db).build().unwrap();
        Store { dir, db, engine }
    }

    async fn activities(engine: &MemoryEngine) -> Vec<memory_engine::types::Activity> {
        let mut rows = engine
            .load_context(SCOPE, 50, 0)
            .await
            .unwrap()
            .recent_activities;
        rows.sort_by_key(|a| a.id);
        rows
    }

    async fn counts(store: Store) -> (u64, u64) {
        let Store {
            dir,
            db,
            mut engine,
        } = store;
        engine.close().await.unwrap();
        let counts = memory_engine::read_redaction_counts(&db).unwrap();
        drop(dir);
        (counts.redacted, counts.stubbed)
    }

    /// No 8-byte window of `secret` appears in `text`.
    fn holds_no_fragment(text: &str, secret: &str) -> bool {
        secret
            .as_bytes()
            .windows(8)
            .all(|w| !text.as_bytes().windows(8).any(|t| t == w))
    }

    #[tokio::test]
    async fn a_sensitive_path_stubs_the_activity() {
        let s = store();
        let promote = ActivityFilterConfig::new(300, [] as [&str; 0], ["read"]);
        let mut req = request(
            "Read",
            json!({ "file_path": DOTENV, "note": aws() }),
            Some(format!("TOKEN={}", ghp())),
        );
        req.touched_paths = vec![DOTENV.into()];
        req.outcome_class = Some(OutcomeClass::Other(format!("odd {}", aws())));
        let result = s
            .engine
            .record_activity(&req, Some(embedder()), &promote)
            .await
            .unwrap();
        assert_eq!(
            (result.status, result.promoted_fact_id),
            (ActivityStatus::Recorded, None)
        );
        // A relative token is also judged joined with the call's working directory, and the
        // stub keeps the token as spelled.
        let mut relative = request(
            "Bash",
            json!({ "command": "cat config" }),
            Some("apiVersion: v1".into()),
        );
        relative.touched_paths = vec!["config".into()];
        relative.cwd = Some("/home/u/.kube".into());
        s.engine
            .record_activity(&relative, None, &promote)
            .await
            .unwrap();
        // A stub's token holding a secret keeps the placeholder.
        let canary = format!("/srv/{} dir/{DOTENV}", aws());
        let mut held = request("Read", json!({ "file_path": canary }), None);
        held.touched_paths = vec![canary];
        held.outcome_class = Some(OutcomeClass::Other("plain".into()));
        s.engine
            .record_activity(&held, None, &promote)
            .await
            .unwrap();
        let rows = activities(&s.engine).await;
        assert_eq!(rows[0].args, json!({ "sensitive_path": DOTENV }));
        assert_eq!(rows[0].result_summary, None);
        assert_eq!(
            rows[0].outcome_class,
            OutcomeClass::Other("odd [REDACTED:aws-access-key]".into())
        );
        assert_eq!(rows[1].args, json!({ "sensitive_path": "config" }));
        assert_eq!(
            rows[2].args,
            json!({ "sensitive_path": format!("/srv/[REDACTED:aws-access-key] dir/{DOTENV}") })
        );
        assert!(
            s.engine.list_active_facts(None).await.unwrap().is_empty(),
            "a stub is never promoted"
        );
        assert_eq!(counts(s).await, (2, 3));
    }

    #[tokio::test]
    async fn a_template_path_is_not_stubbed() {
        let s = store();
        let template = format!("{DOTENV}.example");
        let mut req = request(
            "Read",
            json!({ "file_path": template, "note": aws() }),
            None,
        );
        req.touched_paths = vec![template.clone()];
        s.engine
            .record_activity(&req, None, &ActivityFilterConfig::default())
            .await
            .unwrap();
        let rows = activities(&s.engine).await;
        assert_eq!(
            rows[0].args,
            json!({ "file_path": template, "note": "[REDACTED:aws-access-key]" })
        );
    }

    #[tokio::test]
    async fn tool_input_and_result_are_cut_after_redaction() {
        let s = store();
        // A token straddles byte 512 after a space, with a quote, a backslash and a multibyte
        // character just before the cut: the excerpt holds no 8-byte fragment of it.
        let lead = format!("{}\"é\\ ", "x".repeat(498));
        let token = ghp();
        let long = format!("{lead}{token} {}", "y".repeat(1500));
        let req = request("Write", json!({ "content": long }), Some(long.clone()));
        s.engine
            .record_activity(&req, None, &ActivityFilterConfig::default())
            .await
            .unwrap();
        let small = request(
            "Read",
            json!({ "file_path": "src/lib.rs" }),
            Some("ok".into()),
        );
        s.engine
            .record_activity(&small, None, &ActivityFilterConfig::default())
            .await
            .unwrap();
        let rows = activities(&s.engine).await;
        let result = rows[0].result_summary.clone().unwrap();
        assert!(
            result.len() <= 512 && holds_no_fragment(&result, &token),
            "{result}"
        );
        let Value::String(args) = &rows[0].args else {
            panic!("a long input is a string: {:?}", rows[0].args)
        };
        assert!(
            args.len() <= 512 && holds_no_fragment(args, &token),
            "{args}"
        );
        assert_eq!(
            rows[1].args,
            json!({ "file_path": "src/lib.rs" }),
            "a short input stays JSON"
        );
    }

    #[tokio::test]
    async fn deduplication_keys_on_the_scrubbed_input() {
        let s = store();
        let config = ActivityFilterConfig::default();
        let call = |secret: String, result: &str| {
            request(
                "Bash",
                json!({ "command": format!("export K={secret}") }),
                Some(result.into()),
            )
        };
        let first = s
            .engine
            .record_activity(&call(aws(), "ok"), None, &config)
            .await
            .unwrap();
        let other_key = format!("AKIA{}", "ABCDEFGHIJKLMNOP");
        let second = s
            .engine
            .record_activity(&call(other_key, &format!("now {}", ghp())), None, &config)
            .await
            .unwrap();
        assert_eq!(
            second.activity_id, first.activity_id,
            "calls that differ only by a secret deduplicate"
        );
        assert_eq!(
            activities(&s.engine).await[0].result_summary.as_deref(),
            Some("now [REDACTED:github-pat]")
        );
        // Two long inputs that share their first 512 bytes stay two rows.
        let shared = "z".repeat(600);
        let long_a = s
            .engine
            .record_activity(
                &request("Write", json!({ "c": format!("{shared}A") }), None),
                None,
                &config,
            )
            .await
            .unwrap();
        let long_b = s
            .engine
            .record_activity(
                &request("Write", json!({ "c": format!("{shared}B") }), None),
                None,
                &config,
            )
            .await
            .unwrap();
        assert_ne!(long_a.activity_id, long_b.activity_id);
        // An ordinary call whose input equals a stub's never merges with it, in either order.
        let mut stub = request("Read", json!({ "file_path": DOTENV }), None);
        stub.touched_paths = vec![DOTENV.into()];
        let lookalike = request("Read", json!({ "sensitive_path": DOTENV }), None);
        let stub_first = s
            .engine
            .record_activity(&stub, None, &config)
            .await
            .unwrap();
        let lookalike_row = s
            .engine
            .record_activity(&lookalike, None, &config)
            .await
            .unwrap();
        let stub_again = s
            .engine
            .record_activity(&stub, None, &config)
            .await
            .unwrap();
        assert_ne!(stub_first.activity_id, lookalike_row.activity_id);
        assert_eq!(stub_first.activity_id, stub_again.activity_id);
        // 1 secret in the first input, 1 in the rewritten result, 1 stub; the second input's
        // secret only matched the row.
        assert_eq!(counts(s).await, (2, 1));
    }

    #[tokio::test]
    async fn a_promotion_is_built_from_the_redacted_result() {
        let s = store();
        let promote = ActivityFilterConfig::new(300, [] as [&str; 0], ["commit"]);
        let token = ghp();
        let result = format!("{} {token} done", "c".repeat(190));
        let r = s
            .engine
            .record_activity(
                &request("git_commit", json!({ "m": "x" }), Some(result)),
                Some(embedder()),
                &promote,
            )
            .await
            .unwrap();
        let fact = s
            .engine
            .get_fact(r.promoted_fact_id.unwrap())
            .await
            .unwrap();
        assert!(holds_no_fragment(&fact.content, &token), "{}", fact.content);
    }

    #[tokio::test]
    async fn a_forgotten_promotion_with_a_secret_stays_forgotten() {
        let s = store();
        let promote = ActivityFilterConfig::new(0, [] as [&str; 0], ["commit"]);
        let call = |session: &str| RecordActivityRequest {
            session_id: session.into(),
            scope_path: None,
            ..request(
                "git_commit",
                json!({ "m": "x" }),
                Some(format!("pushed {}", ghp())),
            )
        };
        let first = s
            .engine
            .record_activity(&call("s1"), Some(embedder()), &promote)
            .await
            .unwrap();
        let record = memory_engine::types::NewEvent {
            timestamp: Utc::now(),
            event_type: memory_engine::types::EventType::MemoryOp,
            payload: json!({}),
            source: "test".into(),
            session_id: None,
            scope_id: 1,
            origin_node_id: "test".into(),
            sequence_id: 0,
            created_at: None,
        };
        let outcome = s
            .engine
            .forget_fact(
                first.promoted_fact_id.unwrap(),
                &["/".into()],
                "wrong",
                "unscoped:test",
                &record,
            )
            .await
            .unwrap();
        assert_eq!(outcome.0, memory_engine::types::ForgetOutcome::Forgotten);
        let again = s
            .engine
            .record_activity(&call("s2"), Some(embedder()), &promote)
            .await
            .unwrap();
        assert_eq!(
            (again.status, again.promoted_fact_id),
            (ActivityStatus::Recorded, None)
        );
    }

    /// The promotion's 200-byte cut can expose a match its result did not hold; the tombstone
    /// check reads the text `add_fact` stores, so a forgotten promotion stays forgotten.
    #[tokio::test]
    async fn a_forgotten_promotion_cut_into_a_match_stays_forgotten() {
        let s = store();
        let promote = ActivityFilterConfig::new(0, [] as [&str; 0], ["commit"]);
        let result = format!(
            "{} {}{}",
            "x".repeat(177),
            "Ab3dE5fG7hI9jK1lM2nO",
            "a".repeat(300)
        );
        let call = |session: &str| RecordActivityRequest {
            session_id: session.into(),
            scope_path: None,
            ..request("git_commit", json!({ "m": "x" }), Some(result.clone()))
        };
        let first = s
            .engine
            .record_activity(&call("s1"), Some(embedder()), &promote)
            .await
            .unwrap();
        let id = first.promoted_fact_id.unwrap();
        let record = memory_engine::types::NewEvent {
            timestamp: Utc::now(),
            event_type: memory_engine::types::EventType::MemoryOp,
            payload: json!({}),
            source: "test".into(),
            session_id: None,
            scope_id: 1,
            origin_node_id: "test".into(),
            sequence_id: 0,
            created_at: None,
        };
        let (outcome, _) = s
            .engine
            .forget_fact(id, &["/".into()], "wrong", "unscoped:test", &record)
            .await
            .unwrap();
        assert_eq!(outcome, memory_engine::types::ForgetOutcome::Forgotten);
        let again = s
            .engine
            .record_activity(&call("s2"), Some(embedder()), &promote)
            .await
            .unwrap();
        assert_eq!(
            (again.status, again.promoted_fact_id),
            (ActivityStatus::Recorded, None)
        );
    }

    /// A text over the write limit is never refused: its first window is scrubbed and cut, so
    /// the excerpt is as redacted as a small one, and the call never deduplicates or promotes.
    #[tokio::test]
    async fn an_oversized_input_or_result_keeps_a_redacted_excerpt() {
        let s = store();
        let promote = ActivityFilterConfig::new(300, [] as [&str; 0], ["write"]);
        let token = ghp();
        let huge = format!(
            "{} {token} {}",
            "w".repeat(100),
            "w".repeat(2 * 1024 * 1024)
        );
        let at = Utc::now();
        let call = || RecordActivityRequest {
            timestamp: at,
            ..request("Write", json!({ "content": huge }), Some(huge.clone()))
        };
        let first = s
            .engine
            .record_activity(&call(), Some(embedder()), &promote)
            .await
            .unwrap();
        assert_eq!(
            (first.status, first.promoted_fact_id),
            (ActivityStatus::Recorded, None)
        );
        let twin = s
            .engine
            .record_activity(&call(), None, &promote)
            .await
            .unwrap();
        assert_ne!(
            twin.activity_id, first.activity_id,
            "identical args and timestamps"
        );
        let Store {
            dir,
            db,
            mut engine,
        } = s;
        engine.close().await.unwrap();
        let engine = MemoryEngine::builder(DIM).path(&db).build().unwrap();
        let again = engine
            .record_activity(&call(), None, &promote)
            .await
            .unwrap();
        assert_ne!(
            again.activity_id, first.activity_id,
            "an oversized input never deduplicates, a replay after a restart included"
        );
        let rows = activities(&engine).await;
        let excerpt = rows[0].args.as_str().expect("an excerpt of the input");
        let result = rows[0]
            .result_summary
            .as_deref()
            .expect("an excerpt of the result");
        for text in [excerpt, result] {
            assert!(text.len() <= 512, "{}", text.len());
            assert!(text.contains("[REDACTED:github-pat]"), "{text}");
            assert!(holds_no_fragment(text, &token), "{text}");
        }
        let literal = request("Write", rows[0].args.clone(), None);
        let third = engine
            .record_activity(&literal, None, &promote)
            .await
            .unwrap();
        assert_ne!(
            third.activity_id, again.activity_id,
            "a call whose input is the excerpt is another call"
        );
        drop(dir);
    }

    /// An `Other` outcome is tool text like the result: redacted, then cut, and over the write
    /// limit only its first window is scanned.
    #[tokio::test]
    async fn an_other_outcome_is_redacted_then_cut() {
        let s = store();
        let config = ActivityFilterConfig::default();
        let token = ghp();
        let long = format!("{} {token} {}", "o".repeat(497), "p".repeat(600));
        let huge = format!(
            "{} {token} {}",
            "q".repeat(100),
            "q".repeat(2 * 1024 * 1024)
        );
        for outcome in [long, huge] {
            let mut req = request(
                "Bash",
                json!({ "command": outcome.len().to_string() }),
                None,
            );
            req.outcome_class = Some(OutcomeClass::Other(outcome));
            s.engine.record_activity(&req, None, &config).await.unwrap();
        }
        let rows = activities(&s.engine).await;
        let OutcomeClass::Other(cut) = &rows[0].outcome_class else {
            panic!("{:?}", rows[0].outcome_class)
        };
        assert!(cut.len() <= 512 && holds_no_fragment(cut, &token), "{cut}");
        let OutcomeClass::Other(cut) = &rows[1].outcome_class else {
            panic!("{:?}", rows[1].outcome_class)
        };
        assert!(cut.len() <= 512 && holds_no_fragment(cut, &token), "{cut}");
        assert!(cut.contains("[REDACTED:github-pat]"), "{cut}");
    }
}
