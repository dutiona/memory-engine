//! The neutral lifecycle core (#1025, ADR-0020 §2, §5, §6): `MemoryEngine::record_lifecycle_event`
//! applies one neutral record under the capture contract. The adapter, the route and the segment
//! rows have their own tests; this is what the engine decides.
#![allow(clippy::unwrap_used)] // test code: panic-on-unwrap is the intended failure signal (#725)

use std::path::PathBuf;

use chrono::{DateTime, Duration, TimeZone, Utc};
use memory_engine::MemoryEngine;
use memory_engine::inspect::ReplayFilter;
use memory_engine::read_quarantined;
use memory_engine::read_redaction_counts;
use memory_engine::types::lifecycle::{
    Actor, BodyFault, Content, Delivery, NeutralRecord, ParseClass, ParseFailure, Recorded,
    StopOutcome, ToolOutcome,
};
use memory_engine::types::{Event, EventType, Fact};
use rusqlite::Connection;
use serde_json::{Value, json};

const DIM: usize = 4;
const BODY_LIMIT: usize = 16 * 1024 * 1024;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap()
}

struct Store {
    dir: tempfile::TempDir,
    db: PathBuf,
    engine: MemoryEngine,
}

impl Store {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let engine = MemoryEngine::builder(DIM).path(&db).build().unwrap();
        Self { dir, db, engine }
    }

    async fn close(mut self) -> (tempfile::TempDir, PathBuf) {
        self.engine.close().await.unwrap();
        (self.dir, self.db)
    }

    async fn events(&self) -> Vec<Event> {
        self.engine
            .replay_events(&ReplayFilter::default())
            .await
            .unwrap()
    }

    async fn captured(&self) -> Vec<Event> {
        let mut events = self.events().await;
        events.retain(|e| e.source == "lifecycle");
        events
    }

    async fn facts(&self) -> Vec<Fact> {
        self.engine.list_active_facts(None).await.unwrap()
    }

    /// Deliver `content` as `event_id`, `secs` after `t0` on the client's clock, received at `t0`.
    async fn send(&self, event_id: &str, secs: i64, content: Content) -> Recorded {
        self.send_as(event_id, secs, Actor::Main, content).await
    }

    async fn send_as(&self, event_id: &str, secs: i64, actor: Actor, content: Content) -> Recorded {
        let delivery = delivery(event_id, secs);
        let record = record("s", actor, content);
        self.engine
            .record_lifecycle_event(&delivery, Ok(record))
            .await
            .unwrap()
    }
}

fn delivery(event_id: &str, secs: i64) -> Delivery {
    Delivery {
        machine: "laptop".into(),
        event_id: event_id.into(),
        time: t0() + Duration::seconds(secs),
        seq: 1,
        harness: "claude-code".into(),
        client_version: "1.0.0".into(),
        body_sha256: *blake3::hash(event_id.as_bytes()).as_bytes(),
        body_size: 100,
        received_at: t0(),
        home: "user:u/project:p".into(),
        provenance: json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
    }
}

fn record(session: &str, actor: Actor, content: Content) -> NeutralRecord {
    NeutralRecord {
        session_id: session.into(),
        actor,
        cwd: None,
        harness_version: None,
        content,
    }
}

fn prompt(text: &str) -> Content {
    Content::Prompt {
        text: Some(text.into()),
    }
}

fn stop(outcome: StopOutcome, message: &str) -> Content {
    Content::Stop {
        outcome,
        message: Some(message.into()),
    }
}

fn compact(summary: &str) -> Content {
    Content::Compact {
        trigger: Some("manual".into()),
        summary: summary.into(),
    }
}

fn tool(paths: &[&str], cwd: Option<&str>) -> (Content, Option<String>) {
    (
        Content::ToolUse {
            tool: "Read".into(),
            input: json!({ "file_path": paths.first().copied().unwrap_or("/x") }),
            result: Some("contents".into()),
            outcome: ToolOutcome::Success,
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            call: None,
            dedup_window_secs: Some(300),
        },
        cwd.map(str::to_owned),
    )
}

fn subagent() -> Actor {
    Actor::Subagent {
        id: Some("agent-1".into()),
        kind: Some("Explore".into()),
    }
}

fn token() -> String {
    format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay")
}

fn stored_fact(recorded: &Recorded) -> Option<i64> {
    match recorded {
        Recorded::Stored { fact } => *fact,
        other => panic!("expected a stored event, got {other:?}"),
    }
}

fn sql_i64(db: &std::path::Path, sql: &str) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn only_main_completed_stops_and_main_compacts_become_facts() {
    let s = Store::new();
    let completed = stop(StopOutcome::Completed, "final answer");
    let results = [
        ("main-stop", s.send("a", 0, completed.clone()).await, true),
        (
            "empty message",
            s.send("b", 1, stop(StopOutcome::Completed, "")).await,
            false,
        ),
        (
            "no message",
            s.send(
                "c",
                2,
                Content::Stop {
                    outcome: StopOutcome::Completed,
                    message: None,
                },
            )
            .await,
            false,
        ),
        (
            "api error",
            s.send("d", 3, stop(StopOutcome::ApiError, "server_error"))
                .await,
            false,
        ),
        (
            "subagent stop",
            s.send_as("e", 4, subagent(), completed).await,
            false,
        ),
        (
            "main compact",
            s.send("f", 5, compact("the summary")).await,
            true,
        ),
        (
            "subagent compact",
            s.send_as("g", 6, subagent(), compact("not mine")).await,
            false,
        ),
        ("prompt", s.send("h", 7, prompt("hello")).await, false),
    ];
    for (name, recorded, becomes_fact) in &results {
        assert_eq!(stored_fact(recorded).is_some(), *becomes_fact, "{name}");
    }

    let facts = s.facts().await;
    let contents: Vec<&str> = facts.iter().map(|f| f.content.as_str()).collect();
    assert_eq!(contents, ["final answer", "the summary"]);
    let captures: Vec<&str> = facts
        .iter()
        .map(|f| f.metadata["capture"].as_str().unwrap())
        .collect();
    assert_eq!(captures, ["final_message", "compaction_summary"]);
    for fact in &facts {
        assert_eq!(fact.metadata["machine"], "laptop");
        assert_eq!(fact.metadata["session_id"], "s");
        assert!(fact.embedding.is_empty(), "queued without a vector");
    }
    assert_eq!(s.captured().await.len(), 8, "every event is stored");
}

#[tokio::test]
async fn a_two_mib_prompt_and_final_message_are_kept_whole() {
    let s = Store::new();
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let prose = "word ".repeat(2 * 1024 * 1024 / 5);
    let text = format!("{prose}{aws}");
    let whole = format!("{prose}[REDACTED:aws-access-key]");

    s.send("a", 0, prompt(&text)).await;
    let recorded = s.send("b", 1, stop(StopOutcome::Completed, &text)).await;

    let payloads: Vec<Value> = s.captured().await.into_iter().map(|e| e.payload).collect();
    assert_eq!(payloads[0]["prompt"], whole);
    assert_eq!(payloads[1]["message"], whole);
    let fact = s
        .engine
        .get_fact(stored_fact(&recorded).unwrap())
        .await
        .unwrap();
    assert_eq!(fact.content, whole);
}

#[tokio::test]
async fn the_engine_refuses_a_record_off_its_invariants() {
    let s = Store::new();
    let refuse = |delivery: Delivery, record: NeutralRecord| {
        let engine = &s.engine;
        async move {
            let got = engine
                .record_lifecycle_event(&delivery, Ok(record))
                .await
                .unwrap();
            assert!(matches!(got, Recorded::Refused(_)), "{got:?}");
        }
    };
    let ok = || record("s", Actor::Main, prompt("hi"));

    refuse(delivery("a", 0), record("s", Actor::Main, compact(""))).await;
    refuse(delivery("b", 0), record("", Actor::Main, prompt("hi"))).await;
    refuse(
        delivery("c", 0),
        record(&"x".repeat(257), Actor::Main, prompt("hi")),
    )
    .await;
    let big = Delivery {
        body_size: u64::try_from(BODY_LIMIT).unwrap() + 1,
        ..delivery("d", 0)
    };
    refuse(big, ok()).await;
    refuse(
        delivery("e", 0),
        record("s", Actor::Main, prompt(&"x".repeat(BODY_LIMIT + 1))),
    )
    .await;

    assert!(s.events().await.is_empty(), "nothing was written");
    assert!(s.facts().await.is_empty(), "no memory was made");
    // No key was taken: the same events are stored once they are valid.
    for id in ["a", "b", "c", "d", "e"] {
        let again = s
            .engine
            .record_lifecycle_event(&delivery(id, 0), Ok(ok()))
            .await
            .unwrap();
        assert!(matches!(again, Recorded::Stored { .. }), "{id}: {again:?}");
    }
}

/// A prompt that fills a body of exactly 16 MiB, dense with short secrets whose placeholders are
/// longer than they are and with control characters that JSON re-escapes, is stored whole and
/// redacted: its payload is larger than the body it came from.
#[tokio::test]
async fn a_text_at_the_body_limit_is_kept_whole_after_expansion() {
    let s = Store::new();
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let unit = format!("{aws}\u{1} ");
    let text = unit.repeat(BODY_LIMIT / unit.len());
    let body_size = u64::try_from(text.len()).unwrap();
    assert!(text.len() <= BODY_LIMIT && BODY_LIMIT - text.len() < unit.len());
    let delivery = Delivery {
        body_size,
        ..delivery("a", 0)
    };

    let recorded = s
        .engine
        .record_lifecycle_event(&delivery, Ok(record("s", Actor::Main, prompt(&text))))
        .await
        .unwrap();

    assert!(matches!(recorded, Recorded::Stored { .. }), "{recorded:?}");
    let stored = s.captured().await.remove(0).payload;
    let prompt = stored["prompt"].as_str().unwrap();
    let placeholder = "[REDACTED:aws-access-key]";
    assert_eq!(
        prompt.matches(placeholder).count(),
        text.matches(&aws).count()
    );
    assert!(!prompt.contains(&aws));
    assert!(
        serde_json::to_string(&stored).unwrap().len() > usize::try_from(body_size).unwrap(),
        "the stored payload is larger than the body"
    );
}

#[tokio::test]
async fn a_fact_is_valid_from_the_earlier_of_client_time_and_receipt() {
    let s = Store::new();
    let ahead = s
        .send("a", 3600, stop(StopOutcome::Completed, "fast clock"))
        .await;
    let behind = s
        .send("b", -3600, stop(StopOutcome::Completed, "slow clock"))
        .await;

    let valid = |recorded: &Recorded| {
        let id = stored_fact(recorded).unwrap();
        let engine = &s.engine;
        async move { engine.get_fact(id).await.unwrap().t_valid.unwrap() }
    };
    assert_eq!(valid(&ahead).await, t0(), "never later than its receipt");
    assert_eq!(valid(&behind).await, t0() - Duration::hours(1));
}

#[tokio::test]
async fn a_sensitive_path_makes_a_stub() {
    let s = Store::new();
    let cases = [
        ("file", tool(&["/work/fixture/.env"], None)),
        ("token", tool(&["~/.ssh/id_rsa"], None)),
        ("relative", tool(&[".env"], Some("/work/fixture"))),
        ("ordinary", tool(&["/work/fixture/main.rs"], None)),
    ];
    for (n, (name, (content, cwd))) in cases.into_iter().enumerate() {
        let delivery = delivery(&format!("e{n}"), 0);
        let record = NeutralRecord {
            cwd,
            ..record("s", Actor::Main, content)
        };
        s.engine
            .record_lifecycle_event(&delivery, Ok(record))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }

    let payloads: Vec<Value> = s.captured().await.into_iter().map(|e| e.payload).collect();
    for payload in &payloads[..3] {
        assert!(
            payload["input"].get("sensitive_path").is_some(),
            "{payload}"
        );
        assert!(
            payload["result"].is_null(),
            "a stub keeps no result: {payload}"
        );
        assert_eq!(payload["outcome"], "success", "a stub keeps its outcome");
        assert_eq!(payload["tool"], "Read");
    }
    assert_eq!(
        payloads[3]["input"],
        json!({ "file_path": "/work/fixture/main.rs" })
    );
    assert_eq!(payloads[3]["result"], "contents");
    let (_dir, db) = s.close().await;
    assert_eq!(read_redaction_counts(&db).unwrap().stubbed, 3);
}

#[tokio::test]
async fn a_quarantine_holds_metadata_only() {
    let s = Store::new();
    let failure = ParseFailure {
        class: ParseClass::NotJson,
        line: 3,
        column: 14,
    };
    let recorded = s
        .engine
        .record_lifecycle_event(&delivery("q1", 0), Err(BodyFault::Parse(failure)))
        .await
        .unwrap();
    assert_eq!(recorded, Recorded::Quarantined);

    let again = s
        .engine
        .record_lifecycle_event(&delivery("q1", 0), Err(BodyFault::Parse(failure)))
        .await
        .unwrap();
    assert_eq!(again, Recorded::Duplicate);
    assert!(s.events().await.is_empty(), "nothing of it is stored");
    let (_dir, db) = s.close().await;
    assert_eq!(read_quarantined(&db).unwrap(), 1);
    let record: String = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT quarantine FROM lifecycle_receipts WHERE event_id = 'q1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&record).unwrap(),
        json!({
            "client_time": "2026-11-21T09:00:00Z",
            "harness": "claude-code",
            "size": 100,
            "error": { "class": "not_json", "line": 3, "column": 14 },
        })
    );
}

#[tokio::test]
async fn an_identifier_holding_a_machine_token_is_refused() {
    let s = Store::new();
    let held = format!("a-{}", token());
    let refused =
        |recorded: Recorded| assert!(matches!(recorded, Recorded::Refused(_)), "{recorded:?}");

    refused(
        s.engine
            .record_lifecycle_event(
                &delivery(&held, 0),
                Ok(record("s", Actor::Main, prompt("x"))),
            )
            .await
            .unwrap(),
    );
    refused(
        s.engine
            .record_lifecycle_event(
                &delivery("b", 0),
                Ok(record(&held, Actor::Main, prompt("x"))),
            )
            .await
            .unwrap(),
    );
    let agent = Actor::Subagent {
        id: Some(held.clone()),
        kind: None,
    };
    refused(
        s.engine
            .record_lifecycle_event(&delivery("c", 0), Ok(record("s", agent, prompt("x"))))
            .await
            .unwrap(),
    );
    refused(
        s.engine
            .record_lifecycle_event(
                &delivery(&held, 0),
                Err(BodyFault::Parse(ParseFailure {
                    class: ParseClass::NotJson,
                    line: 1,
                    column: 1,
                })),
            )
            .await
            .unwrap(),
    );

    assert!(s.events().await.is_empty(), "no event was stored");
    let (_dir, db) = s.close().await;
    assert_eq!(sql_i64(&db, "SELECT COUNT(*) FROM lifecycle_receipts"), 0);
}

#[tokio::test]
async fn a_resend_after_reopen_has_no_second_effect() {
    let s = Store::new();
    let (call, _) = tool(&["/work/fixture/main.rs"], None);
    for (id, content) in [("a", stop(StopOutcome::Completed, "answer")), ("b", call)] {
        s.send(id, 0, content).await;
    }
    let (dir, db) = s.close().await;
    let before = (
        sql_i64(&db, "SELECT COUNT(*) FROM events"),
        sql_i64(&db, "SELECT COUNT(*) FROM facts"),
        sql_i64(&db, "SELECT SUM(occurrence_count) FROM activities"),
        sql_i64(&db, "SELECT main_turns FROM lifecycle_segments"),
        read_redaction_counts(&db).unwrap().redacted,
    );

    let engine = MemoryEngine::builder(DIM).path(&db).build().unwrap();
    let (call, _) = tool(&["/work/fixture/main.rs"], None);
    for (id, content) in [("a", stop(StopOutcome::Completed, "answer")), ("b", call)] {
        let again = engine
            .record_lifecycle_event(&delivery(id, 0), Ok(record("s", Actor::Main, content)))
            .await
            .unwrap();
        assert_eq!(again, Recorded::Duplicate, "{id}");
    }
    let mut engine = engine;
    engine.close().await.unwrap();

    let after = (
        sql_i64(&db, "SELECT COUNT(*) FROM events"),
        sql_i64(&db, "SELECT COUNT(*) FROM facts"),
        sql_i64(&db, "SELECT SUM(occurrence_count) FROM activities"),
        sql_i64(&db, "SELECT main_turns FROM lifecycle_segments"),
        read_redaction_counts(&db).unwrap().redacted,
    );
    assert_eq!(after, before);
    drop(dir);
}

#[tokio::test]
async fn an_ignored_tool_is_stored_with_no_activity() {
    let s = Store::new();
    let ignored = Content::ToolUse {
        tool: "prettier".into(),
        input: json!({ "file_path": "/work/fixture/main.rs" }),
        result: Some("formatted".into()),
        outcome: ToolOutcome::Success,
        paths: vec!["/work/fixture/main.rs".into()],
        call: None,
        dedup_window_secs: None,
    };

    let recorded = s.send("a", 0, ignored).await;

    assert!(matches!(recorded, Recorded::Stored { fact: None }));
    let events = s.captured().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::ToolCall);
    let (_dir, db) = s.close().await;
    assert_eq!(sql_i64(&db, "SELECT COUNT(*) FROM activities"), 0);
}
