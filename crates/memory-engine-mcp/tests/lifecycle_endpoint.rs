//! `POST /lifecycle/events` (ADR-0020 §3, §5, §6, §8, §11), driven with the checked-in Claude Code hook fixtures.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use memory_engine_mcp::home::User;
use memory_engine_mcp::lifecycle::{BODY_LIMIT, Envelope};
use memory_engine_mcp::server::MemoryMcpServer;

use serde_json::{Value, json};

mod common;

use common::{TestServer, fixture, fixture_names, holds, store_bytes};

/// The fixtures every acceptance test draws from: one per ADR-0020 §11 hook, plus the variants the tests need.
const REQUIRED_FIXTURES: [&str; 14] = [
    "session-start-startup",
    "session-start-resume",
    "session-start-compact",
    "user-prompt-submit",
    "post-tool-use-bash",
    "post-tool-use-read",
    "post-tool-use-mcp-me-recall",
    "post-tool-use-failure-read",
    "subagent-start",
    "subagent-stop",
    "stop",
    "stop-failure",
    "post-compact",
    "session-end",
];

const ENVELOPE: [&str; 5] = [
    "ME-Event-Id",
    "ME-Event-Time",
    "ME-Event-Seq",
    "ME-Harness",
    "ME-Client-Version",
];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code-hooks")
}

fn string_leaves<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| string_leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_leaves(v, out)),
        _ => {}
    }
}

/// The repository is public: a fixture holds no secret shape a scanner could flag, no local path, and a header file
/// holds only the envelope and the project facts.
#[test]
fn fixtures_are_public_safe() {
    let dir = fixtures_dir();
    for name in REQUIRED_FIXTURES {
        assert!(dir.join(name).join("body").is_file(), "no fixture {name}");
    }
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_dir() {
            continue;
        }
        let body = std::fs::read(path.join("body")).unwrap();
        let text = String::from_utf8(body).unwrap();
        for local in ["/home/", "/Users/", "/tmp/", "/var/folders/"] {
            assert!(!text.contains(local), "{} holds {local}", path.display());
        }
        let json: Value = serde_json::from_str(&text).unwrap();
        assert!(json.is_object(), "{}", path.display());
        let mut leaves = Vec::new();
        string_leaves(&json, &mut leaves);
        for leaf in leaves {
            assert_eq!(
                memory_engine::redact::scrub(leaf).unwrap(),
                leaf,
                "{}: a redaction rule matches a fixture string",
                path.display()
            );
        }
        let headers = std::fs::read_to_string(path.join("headers")).unwrap();
        for local in ["/home/", "/Users/", "/tmp/", "/var/folders/"] {
            assert!(
                !headers.contains(local),
                "{} headers hold {local}",
                path.display()
            );
        }
        let names: Vec<&str> = headers
            .lines()
            .map(|l| l.split_once(": ").unwrap().0)
            .collect();
        for want in ENVELOPE {
            assert!(names.contains(&want), "{}: no {want}", path.display());
        }
        assert!(
            names.iter().all(|n| n.starts_with("ME-")),
            "{}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= REQUIRED_FIXTURES.len());
}
const EVENTS: &str = "SELECT COUNT(*) FROM events WHERE source = 'lifecycle'";
const RECEIPTS: &str = "SELECT COUNT(*) FROM lifecycle_receipts";

/// What a delivery changes in the store: events, memories, activity occurrences, each segment's
/// counts and the redaction totals.
fn effects(server: &TestServer) -> (i64, i64, i64, String, i64) {
    (
        server.number(EVENTS),
        server.number("SELECT COUNT(*) FROM facts"),
        server.number("SELECT COALESCE(SUM(occurrence_count), 0) FROM activities"),
        rusqlite::Connection::open(server.db())
            .unwrap()
            .query_row(
                "SELECT COALESCE(group_concat(counts), '') FROM lifecycle_segments",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        server.number(
            "SELECT COALESCE(SUM(CAST(value AS INTEGER)), 0) FROM config WHERE key = 'redact_count'",
        ),
    )
}

#[tokio::test]
async fn the_checks_run_in_the_adr_order() {
    let server = TestServer::start().await;
    let stop = fixture("stop");
    let over = u64::try_from(BODY_LIMIT).unwrap() + 1;

    // The envelope comes first: a bad one is a 400 whatever the size and the harness.
    for broken in [
        stop.clone().without("ME-Event-Id"),
        stop.clone().header("ME-Event-Seq", "1000000"),
        stop.clone().header("ME-Harness", "Codex"),
    ] {
        let status = server.status_from_headers("laptop", &broken, over).await;
        assert_eq!(status, 400, "a bad envelope beats the size");
    }
    // Then the size: declared over the limit is a 413 at once, before the harness is looked at.
    assert_eq!(server.status_from_headers("laptop", &stop, over).await, 413);
    let codex = stop.clone().header("ME-Harness", "codex");
    assert_eq!(
        server.status_from_headers("laptop", &codex, over).await,
        413
    );

    // Then the harness, the mapping and the key: each refusal leaves no key behind.
    assert_eq!(server.post_event("laptop", &codex).await.status, 422);
    let unknown_hook = stop
        .clone()
        .json(|v| v["hook_event_name"] = json!("PreCompact"));
    assert_eq!(server.post_event("laptop", &unknown_hook).await.status, 422);
    let no_session = stop.clone().json(|v| {
        v.as_object_mut().unwrap().remove("session_id");
    });
    assert_eq!(server.post_event("laptop", &no_session).await.status, 422);
    assert_eq!(server.number(RECEIPTS), 0, "a refusal holds no key");
    assert!(
        server
            .post_event("laptop", &stop)
            .await
            .is(204, Some("stored"))
    );

    // The key beats the parse: a reused ID is a 409, a body that does not parse included.
    let other = stop
        .clone()
        .json(|v| v["last_assistant_message"] = json!("another"));
    assert_eq!(server.post_event("laptop", &other).await.status, 409);
    let unparseable = stop.clone().body(&b"{ not json"[..]);
    assert_eq!(server.post_event("laptop", &unparseable).await.status, 409);
    assert!(
        server
            .post_event("laptop", &stop)
            .await
            .is(204, Some("duplicate"))
    );

    assert_eq!(
        server.number(EVENTS),
        1,
        "nothing was stored on any refusal"
    );
    assert_eq!(server.number(RECEIPTS), 1);
}

#[tokio::test]
async fn every_fixture_is_stored_once() {
    let server = TestServer::start().await;
    let names = fixture_names();
    for name in &names {
        let posted = server.post_event("laptop", &fixture(name)).await;
        assert!(posted.is(204, Some("stored")), "{name}: {posted:?}");
    }
    let count = i64::try_from(names.len()).unwrap();
    assert_eq!(server.number(EVENTS), count);
    for name in &names {
        let posted = server.post_event("laptop", &fixture(name)).await;
        assert!(posted.is(204, Some("duplicate")), "{name}: {posted:?}");
    }
    assert_eq!(server.number(EVENTS), count, "a resend stores nothing");
    assert_eq!(server.number(RECEIPTS), count);
}

#[tokio::test]
async fn a_stored_event_survives_a_restart_as_a_duplicate() {
    let mut server = TestServer::start().await;
    let stop = fixture("stop");
    assert!(
        server
            .post_event("laptop", &stop)
            .await
            .is(204, Some("stored"))
    );

    server.restart().await;

    assert!(
        server
            .post_event("laptop", &stop)
            .await
            .is(204, Some("duplicate"))
    );
    assert_eq!(server.number(EVENTS), 1);
}

/// The server cannot tell a commit whose 204 was lost from a resend: the event is committed
/// through the seam, so no answer ever left, then posted.
#[tokio::test]
async fn an_event_committed_without_its_ack_is_a_duplicate_on_resend() {
    let server = TestServer::start().await;
    let mcp = MemoryMcpServer::new(
        Arc::clone(&server.engine),
        None,
        User::parse("alice").unwrap(),
    );
    for name in ["post-tool-use-bash", "stop"] {
        let sent = fixture(name);
        let headers = sent.header_map();
        let envelope = Envelope::from_headers(&headers).unwrap();
        let committed = mcp
            .record_lifecycle(&common::machine("laptop"), &envelope, &headers, &sent.body)
            .await;
        assert_eq!(committed.outcome, Some("stored"), "{name}");
        let before = effects(&server);

        let resent = server.post_event("laptop", &sent).await;

        assert!(resent.is(204, Some("duplicate")), "{name}: {resent:?}");
        assert_eq!(effects(&server), before, "{name}");
    }
}

#[tokio::test]
async fn an_apply_fault_is_422_and_leaves_nothing() {
    let server = TestServer::start().await;
    let stop = fixture("stop");
    server.refuse_event_writes();

    let refused = server.post_event("laptop", &stop).await;

    assert_eq!(refused.status, 422, "{refused:?}");
    assert_eq!((server.number(EVENTS), server.number(RECEIPTS)), (0, 0));
    server.sql("DROP TRIGGER refuse_events");
    assert!(
        server
            .post_event("laptop", &stop)
            .await
            .is(204, Some("stored"))
    );
}

/// Another writer holds the store: the answer is a 503, the client retries, and the retry stores
/// the event. The engine waits its 5 s busy timeout first.
#[tokio::test]
async fn a_busy_store_is_503_and_the_retry_stores_it() {
    let server = TestServer::start().await;
    let stop = fixture("stop");
    let rival = rusqlite::Connection::open(server.db()).unwrap();
    rival.execute_batch("BEGIN IMMEDIATE").unwrap();

    let busy = server.post_event("laptop", &stop).await;

    assert_eq!(busy.status, 503, "{busy:?}");
    rival.execute_batch("ROLLBACK").unwrap();
    assert_eq!(server.number(RECEIPTS), 0);
    assert!(
        server
            .post_event("laptop", &stop)
            .await
            .is(204, Some("stored"))
    );
}

#[tokio::test]
async fn a_malformed_body_is_quarantined_as_metadata_only() {
    let mut server = TestServer::start().await;
    let token = common::ghp();
    let path = format!("/work/fixture/{}", ".env");
    let secrets = format!("{token} {path}");
    let truncated = format!(r#"{{"session_id":"s","note":"{secrets}"#);
    let not_an_object = format!(r#"["{token}","{path}"]"#);
    let mut not_utf8 = b"\xff\xfe ".to_vec();
    not_utf8.extend_from_slice(secrets.as_bytes());
    let base = fixture("stop");
    let bodies: [(&str, Vec<u8>); 3] = [
        ("not utf-8", not_utf8),
        ("truncated", truncated.into_bytes()),
        ("not an object", not_an_object.into_bytes()),
    ];
    for (n, (name, body)) in bodies.into_iter().enumerate() {
        let event = base
            .clone()
            .header("ME-Event-Id", &format!("q{n}"))
            .body(body);
        let posted = server.post_event("laptop", &event).await;
        assert!(posted.is(204, Some("quarantined")), "{name}: {posted:?}");
        let again = server.post_event("laptop", &event).await;
        assert!(again.is(204, Some("duplicate")), "{name}: {again:?}");
        let other = event.clone().body(&b"{ other"[..]);
        assert_eq!(
            server.post_event("laptop", &other).await.status,
            409,
            "{name}"
        );
    }

    assert_eq!(memory_engine::read_quarantined(&server.db()).unwrap(), 3);
    assert_eq!(
        server.number(EVENTS),
        0,
        "nothing of a quarantined body is stored"
    );
    server.stop().await;
    for (name, bytes) in &store_bytes(&server) {
        assert!(!holds(bytes, token.as_bytes()), "{name} holds the token");
        assert!(!holds(bytes, path.as_bytes()), "{name} holds the path");
    }
}

#[tokio::test]
async fn a_ten_megabyte_tool_result_is_stored_truncated() {
    let mut server = TestServer::start().await;
    let token = common::ghp();
    let filler = "w".repeat(64 * 1024);
    // The call line, then a canary below byte 512, then ten megabytes.
    let text = format!(
        "call 518b327349710550\n{} {token} {}",
        "x".repeat(430),
        filler.repeat(160)
    );
    assert!(text.len() > 10_000_000);
    let big = fixture("post-tool-use-mcp-me-recall")
        .json(|v| v["tool_response"] = json!([{ "type": "text", "text": text }]));

    let posted = server.post_event("laptop", &big).await;

    assert!(posted.is(204, Some("stored")), "{posted:?}");
    let payload: String = rusqlite::Connection::open(server.db())
        .unwrap()
        .query_row(
            "SELECT payload FROM events WHERE source = 'lifecycle' AND event_type = 'ToolCall'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    let result = payload["result"].as_str().unwrap();
    assert!(result.len() <= 512, "{}", result.len());
    assert!(result.contains("[REDACTED:github-pat]"), "{result}");
    assert!(common::holds_no_fragment(result, &token));
    server.stop().await;
    for (name, bytes) in &store_bytes(&server) {
        assert!(!holds(bytes, token.as_bytes()), "{name} holds the token");
        assert!(
            !holds(bytes, filler.as_bytes()),
            "{name} holds a 64 KiB run of the filler"
        );
    }
}
// --- The capture contract over HTTP (ADR-0020 §5, §6, §8, §11) ---

/// The client's clock, `secs` after a fixed second.
fn at(secs: i64) -> String {
    use chrono::TimeZone;
    (chrono::Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap() + chrono::Duration::seconds(secs))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Fixture `name` as event `id`, `secs` into the client's clock with tie-break `seq`.
fn event(name: &str, id: &str, secs: i64, seq: u32) -> common::Fixture {
    fixture(name)
        .header("ME-Event-Id", id)
        .header("ME-Event-Time", &at(secs))
        .header("ME-Event-Seq", &seq.to_string())
}

fn saying(fixture: common::Fixture, member: &str, text: &str) -> common::Fixture {
    fixture.json(|v| v[member] = json!(text))
}

async fn stored(server: &TestServer, fixture: &common::Fixture) {
    let posted = server.post_event("laptop", fixture).await;
    assert!(
        posted.is(204, Some("stored")),
        "{}: {posted:?}",
        fixture.id()
    );
}

/// One row of `lifecycle_segments`.
#[derive(Debug, Clone, PartialEq)]
struct Segment {
    number: i64,
    counts: Value,
    main_turns: i64,
    final_fact: Option<i64>,
    ended_by: Option<String>,
    last_key: String,
}

fn segments(server: &TestServer, session: &str) -> Vec<Segment> {
    let conn = rusqlite::Connection::open(server.db()).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT number, counts, main_turns, final_fact, ended_by, last_key
               FROM lifecycle_segments WHERE session_id = ?1 ORDER BY number",
        )
        .unwrap();
    stmt.query_map([session], |r| {
        Ok(Segment {
            number: r.get(0)?,
            counts: serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
            main_turns: r.get(2)?,
            final_fact: r.get(3)?,
            ended_by: r.get(4)?,
            last_key: r.get(5)?,
        })
    })
    .unwrap()
    .map(std::result::Result::unwrap)
    .collect()
}

/// The payload of the one captured event `id`.
fn payload_of(server: &TestServer, id: &str) -> Value {
    let payload: String = rusqlite::Connection::open(server.db())
        .unwrap()
        .query_row(
            "SELECT e.payload FROM events e JOIN lifecycle_receipts r ON r.event_row = e.id
              WHERE r.event_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&payload).unwrap()
}

/// The fact made from event `id`.
fn fact_of(server: &TestServer, id: &str) -> i64 {
    server.number(&format!(
        "SELECT f.id FROM facts f JOIN lifecycle_receipts r ON r.event_row = f.source_event_id
          WHERE r.event_id = '{id}'"
    ))
}

#[tokio::test]
async fn provenance_is_stored() {
    let server = TestServer::start().await;
    let call = event("post-tool-use-bash-subagent", "p1", 0, 1);
    let session = call.json_value()["session_id"].as_str().unwrap().to_owned();
    stored(&server, &call).await;

    let payload = payload_of(&server, "p1");

    assert_eq!(payload["machine"], "laptop");
    assert_eq!(payload["event_id"], "p1");
    assert_eq!(payload["harness"], "claude-code");
    assert_eq!(payload["client_version"], "0.1.0");
    assert!(payload["harness_version"].is_null());
    assert_eq!(payload["kind"], "tool_use");
    assert_eq!(payload["actor"], "subagent");
    assert!(payload["agent_id"].is_string());
    assert!(payload["agent_type"].is_string());
    assert_eq!(payload["cwd"], "/work/fixture-home/dev/proj");
    let project = &payload["project"];
    assert_eq!(project["facts"]["root"], "/work/fixture-home/dev/proj");
    assert_eq!(project["facts"]["branch"], "main");
    assert_eq!(project["key"], "example.com/example/proj");
    assert_eq!(project["rules"], 1);
    let column: String = rusqlite::Connection::open(server.db())
        .unwrap()
        .query_row(
            "SELECT e.session_id FROM events e JOIN lifecycle_receipts r ON r.event_row = e.id
              WHERE r.event_id = 'p1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(column, session);
}

#[tokio::test]
async fn events_order_by_time_seq_and_id() {
    let server = TestServer::start().await;
    stored(&server, &event("session-start-startup", "a", 0, 1)).await;
    // The same second: seq 10 arrives first, then seq 9, which sorts before it.
    stored(
        &server,
        &saying(
            event("stop", "ten", 5, 10),
            "last_assistant_message",
            "seq ten",
        ),
    )
    .await;
    stored(
        &server,
        &saying(
            event("stop", "nine", 5, 9),
            "last_assistant_message",
            "seq nine",
        ),
    )
    .await;
    stored(&server, &event("session-end", "z", 6, 1)).await;

    let session = fixture("stop").json_value()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let rows = segments(&server, &session);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ended_by.as_deref(), Some("session_end"));
    assert_eq!(rows[0].main_turns, 2);
    assert_eq!(
        rows[0].final_fact,
        Some(fact_of(&server, "ten")),
        "the greater key's message, though it arrived first"
    );
}

#[tokio::test]
async fn a_late_event_is_applied_without_reopening_its_segment() {
    let server = TestServer::start().await;
    stored(&server, &event("session-start-startup", "a", 0, 1)).await;
    stored(
        &server,
        &saying(event("stop", "s", 5, 1), "last_assistant_message", "first"),
    )
    .await;
    stored(&server, &event("session-end", "z", 10, 1)).await;
    let session = fixture("stop").json_value()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = segments(&server, &session);
    assert_eq!(before[0].ended_by.as_deref(), Some("session_end"));
    let facts = server.number("SELECT COUNT(*) FROM facts");

    // A late prompt, a late final message and a late resume.
    stored(&server, &event("user-prompt-submit", "late-prompt", 3, 1)).await;
    stored(
        &server,
        &saying(
            event("stop", "late-stop", 4, 1),
            "last_assistant_message",
            "late",
        ),
    )
    .await;
    stored(&server, &event("session-start-resume", "late-resume", 2, 1)).await;

    assert_eq!(
        segments(&server, &session),
        before,
        "the ended row is as it was"
    );
    assert_eq!(
        server.number("SELECT COUNT(*) FROM facts"),
        facts + 1,
        "a late final message still becomes a memory"
    );
    assert_eq!(server.number(EVENTS), 6);
}

#[tokio::test]
async fn a_resumed_session_keeps_capturing() {
    let server = TestServer::start().await;
    stored(&server, &event("session-start-startup", "a", 0, 1)).await;
    stored(
        &server,
        &saying(
            event("stop", "s1", 5, 1),
            "last_assistant_message",
            "first run",
        ),
    )
    .await;
    stored(&server, &event("session-end", "z", 6, 1)).await;

    stored(&server, &event("session-start-resume", "r", 100, 1)).await;
    stored(
        &server,
        &saying(
            event("stop", "s2", 105, 1),
            "last_assistant_message",
            "second run",
        ),
    )
    .await;

    let session = fixture("stop").json_value()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let rows = segments(&server, &session);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].ended_by.as_deref(), Some("session_end"));
    assert_eq!(rows[1].ended_by, None);
    assert_eq!(rows[1].final_fact, Some(fact_of(&server, "s2")));
}

#[tokio::test]
async fn a_session_idle_past_the_threshold_keeps_capturing() {
    let server = TestServer::start().await;
    stored(&server, &event("user-prompt-submit", "a", 0, 1)).await;
    // Exactly 30 minutes is not a gap.
    stored(&server, &event("user-prompt-submit", "b", 1800, 1)).await;
    let session = fixture("stop").json_value()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(segments(&server, &session).len(), 1);

    // 30 minutes and a second after the last event is.
    stored(
        &server,
        &saying(
            event("stop", "c", 1800 + 1801, 1),
            "last_assistant_message",
            "after the gap",
        ),
    )
    .await;
    stored(
        &server,
        &saying(
            event("stop", "d", 1800 + 1900, 1),
            "last_assistant_message",
            "and after",
        ),
    )
    .await;

    let rows = segments(&server, &session);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].ended_by.as_deref(), Some("gap"));
    assert_eq!(rows[1].final_fact, Some(fact_of(&server, "d")));
    assert_eq!(server.number(EVENTS), 4);
}

#[tokio::test]
async fn captured_events_are_redacted_over_http() {
    let mut server = TestServer::start().await;
    let tokens: Vec<String> = common::AC2
        .iter()
        .enumerate()
        .map(|(i, (prefix, _))| common::ac2_token(i, prefix))
        .collect();
    let text = format!(
        "keys {} and {} end",
        tokens.join(" "),
        common::machine_token()
    );
    let placeholders: Vec<String> = common::AC2
        .iter()
        .map(|(_, rule)| format!("[REDACTED:{rule}]"))
        .collect();
    let want = format!(
        "keys {} and [REDACTED:machine-token] end",
        placeholders.join(" ")
    );

    stored(
        &server,
        &saying(event("user-prompt-submit", "a", 0, 1), "prompt", &text),
    )
    .await;
    stored(
        &server,
        &event("post-tool-use-bash", "b", 1, 1).json(|v| v["tool_response"] = json!(text)),
    )
    .await;
    stored(
        &server,
        &saying(event("stop", "c", 2, 1), "last_assistant_message", &text),
    )
    .await;

    assert_eq!(payload_of(&server, "a")["prompt"], want);
    assert_eq!(payload_of(&server, "c")["message"], want);
    let result = payload_of(&server, "b")["result"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        result.len() <= 512 && result.starts_with("keys [REDACTED:huggingface-token]"),
        "{result}"
    );
    assert_eq!(
        server
            .engine
            .get_fact(fact_of(&server, "c"))
            .await
            .unwrap()
            .content,
        want
    );
    let counts = memory_engine::read_redaction_counts(&server.db()).unwrap();
    assert_eq!(
        counts.redacted,
        3 * 17,
        "seventeen secrets in each of three texts"
    );
    server.wait_drained().await;
    let mut secrets = tokens;
    secrets.push(common::machine_token());
    for request in server.embedder.received_requests().await.unwrap() {
        let body = String::from_utf8_lossy(&request.body);
        for secret in &secrets {
            assert!(!body.contains(secret.as_str()), "the embedder saw {secret}");
        }
    }
    server.stop().await;
    for (name, bytes) in &store_bytes(&server) {
        for secret in &secrets {
            assert!(!holds(bytes, secret.as_bytes()), "{name} holds {secret}");
        }
    }
}

#[tokio::test]
async fn a_tool_call_on_a_sensitive_path_is_a_stub() {
    let mut server = TestServer::start().await;
    let secret_file = format!("/work/fixture/{}", ".env");
    let contents = "kestrel-secret-contents";
    let read = event("post-tool-use-read", "a", 0, 1).json(|v| {
        v["tool_input"]["file_path"] = json!(secret_file);
        v["tool_response"] = json!({ "content": contents });
    });

    stored(&server, &read).await;

    let payload = payload_of(&server, "a");
    assert_eq!(payload["input"], json!({ "sensitive_path": secret_file }));
    assert!(payload["result"].is_null());
    assert_eq!(payload["tool"], "Read");
    assert_eq!(payload["outcome"], "success");
    assert_eq!(
        memory_engine::read_redaction_counts(&server.db())
            .unwrap()
            .stubbed,
        1
    );
    let stub: String = rusqlite::Connection::open(server.db())
        .unwrap()
        .query_row("SELECT args FROM activities", [], |r| r.get(0))
        .unwrap();
    assert!(!stub.contains(contents), "{stub}");
    server.stop().await;
    for (name, bytes) in &store_bytes(&server) {
        assert!(
            !holds(bytes, contents.as_bytes()),
            "{name} holds the file's contents"
        );
    }
}

#[tokio::test]
async fn the_activity_filter_classifies_for_the_stream_only() {
    let server = TestServer::start().await;
    let activities = "SELECT COUNT(*) FROM activities";

    // An ignored tool: its event is stored, no activity is.
    stored(
        &server,
        &event("post-tool-use-bash", "fmt", 0, 1).json(|v| v["tool_name"] = json!("prettier")),
    )
    .await;
    assert_eq!((server.number(EVENTS), server.number(activities)), (1, 0));

    // One Bash twice within 300 s: one activity, counted twice, and two events.
    stored(&server, &event("post-tool-use-bash", "one", 10, 1)).await;
    stored(&server, &event("post-tool-use-bash", "two", 20, 1)).await;
    assert_eq!(server.number(EVENTS), 3);
    assert_eq!(server.number(activities), 1);
    assert_eq!(server.number("SELECT occurrence_count FROM activities"), 2);
    // Past the window it is another row.
    stored(&server, &event("post-tool-use-bash", "three", 20 + 301, 1)).await;
    assert_eq!(server.number(activities), 2);
}

#[tokio::test]
async fn capture_succeeds_with_the_embedder_down() {
    let mut server = TestServer::start_unreachable().await;
    let text = "the kestrel nests in the tower";
    stored(
        &server,
        &saying(event("stop", "a", 0, 1), "last_assistant_message", text),
    )
    .await;

    let id = fact_of(&server, "a");
    assert!(
        !server.has_vector(id).await,
        "no vector without the embedder"
    );
    assert!(server.queue(id).is_some(), "the fact waits in the queue");
    let facts: Vec<(&str, Vec<u8>)> = vec![
        ("ME-Project-Root", b"/work/fixture-home/dev/proj".to_vec()),
        ("ME-Project-Dev-Root", b"/work/fixture-home/dev".to_vec()),
        (
            "ME-Project-Origin-Url",
            b"https://example.com/example/proj.git".to_vec(),
        ),
    ];
    let borrowed: Vec<(&str, &[u8])> = facts.iter().map(|(n, v)| (*n, v.as_slice())).collect();
    let reply = server
        .tool_with("laptop", &borrowed, "recall", json!({ "query": "kestrel" }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let found: Vec<i64> = reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(found, [id], "full-text search finds it at once");

    server.bring_embedder_up().await;
    server.wait_drained().await;
    assert!(
        server.has_vector(id).await,
        "a vector once the embedder is up"
    );
}

/// A project session's headers for the checked-in fixture's repository, or another one.
fn session_in(name: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "ME-Project-Root",
            format!("/work/fixture-home/dev/{name}").into_bytes(),
        ),
        ("ME-Project-Dev-Root", b"/work/fixture-home/dev".to_vec()),
        (
            "ME-Project-Origin-Url",
            format!("https://example.com/example/{name}.git").into_bytes(),
        ),
    ]
}

async fn recalled(server: &TestServer, facts: &[(&'static str, Vec<u8>)]) -> Vec<(String, String)> {
    let borrowed: Vec<(&str, &[u8])> = facts.iter().map(|(n, v)| (*n, v.as_slice())).collect();
    let reply = server
        .tool_with("laptop", &borrowed, "recall", json!({ "query": "kestrel" }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let mut found: Vec<(String, String)> = reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["content"].as_str().unwrap().to_owned(),
                r["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    found.sort();
    found
}

#[tokio::test]
async fn final_messages_and_summaries_are_recalled_in_their_project_only() {
    let server = TestServer::start().await;
    stored(
        &server,
        &saying(
            event("stop", "a", 0, 1),
            "last_assistant_message",
            "kestrel final",
        ),
    )
    .await;
    stored(
        &server,
        &saying(
            event("post-compact", "b", 1, 1),
            "compact_summary",
            "kestrel summary",
        ),
    )
    .await;
    // A subagent's final message and a failed stop are never recalled.
    stored(
        &server,
        &saying(
            event("subagent-stop", "c", 2, 1),
            "last_assistant_message",
            "kestrel sub",
        ),
    )
    .await;
    stored(
        &server,
        &saying(
            event("stop-failure", "d", 3, 1),
            "last_assistant_message",
            "kestrel error",
        ),
    )
    .await;

    // Another session of the same project finds both, under their kinds.
    let want = [
        ("kestrel final".to_owned(), "final message".to_owned()),
        (
            "kestrel summary".to_owned(),
            "compaction summary".to_owned(),
        ),
    ];
    assert_eq!(recalled(&server, &session_in("proj")).await, want);
    // Another project, and a session with no project key, find neither.
    assert!(
        recalled(&server, &session_in("other")).await.is_empty(),
        "another project finds nothing"
    );
    assert!(
        recalled(&server, &[]).await.is_empty(),
        "a keyless session finds nothing"
    );
}

#[tokio::test]
async fn an_me_call_is_linked_through_its_call_id() {
    let server = TestServer::start().await;
    let recall = server
        .tool("laptop", "recall", json!({ "query": "anything" }))
        .await;
    let record: i64 = server.number(&format!(
        "SELECT id FROM events WHERE source = 'mcp' AND json_extract(payload, '$.call') = '{}'",
        recall.call
    ));
    let linking = |id: &str, call: &str| {
        event("post-tool-use-mcp-me-recall", id, 0, 1).json(|v| {
            v["tool_response"] = json!([{ "type": "text", "text": format!("call {call}\n{{}}") }]);
        })
    };
    let link_of = |machine: &str, id: &str| -> Option<i64> {
        rusqlite::Connection::open(server.db())
            .unwrap()
            .query_row(
                "SELECT call_event FROM lifecycle_receipts WHERE machine = ?1 AND event_id = ?2",
                [machine, id],
                |r| r.get(0),
            )
            .unwrap()
    };

    let own = linking("own", &recall.call);
    assert!(
        server
            .post_event("laptop", &own)
            .await
            .is(204, Some("stored"))
    );
    let desktop = linking("desktop", &recall.call);
    assert!(
        server
            .post_event("desktop", &desktop)
            .await
            .is(204, Some("stored"))
    );
    let forged = linking("forged", "0123456789abcdef");
    assert!(
        server
            .post_event("laptop", &forged)
            .await
            .is(204, Some("stored"))
    );

    assert_eq!(link_of("laptop", "own"), Some(record));
    assert_eq!(
        link_of("desktop", "desktop"),
        None,
        "another machine's call"
    );
    assert_eq!(
        link_of("laptop", "forged"),
        None,
        "a forged line matches nothing"
    );
}

/// The operator's `erase`, with the server stopped: laptop's captured rows are gone from the
/// tables and from the files, desktop's are not.
#[test]
fn erasing_a_machine_takes_its_captured_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = common::ServerProcess::spawn(dir.path(), &[], &[]);
    server.wait_ready().unwrap();
    let (laptop, desktop) = ("zqxjvkpwqqlaptop", "zqxjvkpwqqdesktop");
    for (machine, marker) in [("laptop", laptop), ("desktop", desktop)] {
        let prefix = &machine[..1];
        let stop = saying(
            event("stop", &format!("{prefix}-stop"), 0, 1),
            "last_assistant_message",
            &format!("{marker} final"),
        );
        let bash = event("post-tool-use-bash", &format!("{prefix}-bash"), 1, 1);
        let torn =
            event("stop", &format!("{prefix}-torn"), 2, 1).body(format!("{{\"torn\": \"{marker}"));
        for (event, outcome) in [(stop, "stored"), (bash, "stored"), (torn, "quarantined")] {
            let posted = server.post_event(machine, &event);
            assert!(
                posted.is(204, Some(outcome)),
                "{machine} {}: {posted:?}",
                event.id()
            );
        }
    }
    assert!(server.terminate().success(), "{}", server.stderr());
    let db = dir.path().join("memory.db");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    #[allow(
        clippy::disallowed_methods,
        reason = "the test stands in for the operator's `erase` command, run with the server stopped"
    )]
    let report = runtime.block_on(async {
        let mut engine = memory_engine::MemoryEngine::builder(common::DIM)
            .path(&db)
            .build()
            .unwrap();
        let report = engine
            .erase(&memory_engine::types::EraseTarget::Machine("laptop".into()))
            .await
            .unwrap();
        engine.close().await.unwrap();
        report
    });

    assert!(report.purged, "{report:?}");
    let count = |sql: &str| -> i64 {
        rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    };
    for table in ["lifecycle_receipts", "lifecycle_segments"] {
        let by = |machine: &str| {
            count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE machine = '{machine}'"
            ))
        };
        assert_eq!(by("laptop"), 0, "{table}");
        assert!(by("desktop") > 0, "{table}");
    }
    assert_eq!(
        count("SELECT COUNT(*) FROM events WHERE json_extract(payload, '$.machine') = 'laptop'"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM events WHERE source = 'lifecycle'"),
        2,
        "desktop's stop and call"
    );
    assert_eq!(count("SELECT COUNT(*) FROM facts"), 1, "desktop's memory");
    assert_eq!(
        count("SELECT COUNT(*) FROM activities WHERE machine = 'laptop'"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM activities WHERE machine = 'desktop'"),
        1
    );
    assert_eq!(
        memory_engine::read_quarantined(&db).unwrap(),
        1,
        "desktop's quarantine row"
    );
    let files: Vec<Vec<u8>> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    assert!(
        files.iter().all(|bytes| !holds(bytes, laptop.as_bytes())),
        "a laptop marker is on disk"
    );
    assert!(
        files.iter().any(|bytes| holds(bytes, desktop.as_bytes())),
        "desktop's text is intact"
    );
}
/// The sweep runs once at start, so a session that went quiet while the server was down is
/// finished at once: its segment ends as `idle` and keeps its checkpoint; a later event opens the
/// next segment; a second start changes nothing.
#[tokio::test]
async fn the_server_ends_idle_segments_at_start() {
    let mut server = TestServer::start().await;
    stored(&server, &event("session-start-startup", "a", 0, 1)).await;
    stored(
        &server,
        &saying(event("stop", "b", 5, 1), "last_assistant_message", "done"),
    )
    .await;
    let session = fixture("stop").json_value()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(segments(&server, &session)[0].ended_by, None);
    server.stop().await;
    let hour_ago = (chrono::Utc::now() - chrono::Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string();
    server.sql(&format!(
        "UPDATE lifecycle_segments SET received_at = '{hour_ago}'"
    ));

    server.restart().await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while segments(&server, &session)[0].ended_by.is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the segment was not ended within 5 s of the start"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let ended = segments(&server, &session);
    assert_eq!(ended[0].ended_by.as_deref(), Some("idle"));
    assert_eq!(ended[0].main_turns, 1, "the checkpoint: a main turn");
    assert_eq!(ended[0].final_fact, Some(fact_of(&server, "b")));

    stored(
        &server,
        &saying(
            event("stop", "c", 100, 1),
            "last_assistant_message",
            "later",
        ),
    )
    .await;
    let rows = segments(&server, &session);
    assert_eq!(rows.len(), 2, "a later event opens the next segment");
    assert_eq!(rows[0], ended[0]);

    server.restart().await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        segments(&server, &session),
        rows,
        "a second start changes nothing"
    );
}

/// How long a client waits for the 204 of one captured prompt, p50, p95 and p99 over 500 events
/// posted one at a time to a server whose store lives in `ME_LATENCY_DIR`. The 204 means durable,
/// so this is the price of a synced commit; run it where `fsync` costs something, never on tmpfs:
///
/// ```text
/// ME_LATENCY_DIR=<a directory on disk> cargo test -p memory-engine-mcp --test lifecycle_endpoint \
///     lifecycle_write_latency -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "a measurement: needs ME_LATENCY_DIR, a directory on disk"]
async fn lifecycle_write_latency() {
    let parent = std::env::var("ME_LATENCY_DIR").expect("ME_LATENCY_DIR names a directory on disk");
    std::fs::create_dir_all(&parent).unwrap();
    let server = TestServer::start_without_embedder_in(std::path::Path::new(&parent)).await;
    let mut micros = Vec::new();
    for i in 0..500_u32 {
        let prompt = event(
            "user-prompt-submit",
            &format!("lat-{i}"),
            i64::from(i),
            i + 1,
        );
        let start = std::time::Instant::now();
        let posted = server.post_event("laptop", &prompt).await;
        micros.push(start.elapsed().as_micros());
        assert!(posted.is(204, Some("stored")), "{posted:?}");
    }
    micros.sort_unstable();
    let at = |percent: usize| micros[(micros.len() * percent / 100).min(micros.len() - 1)];
    println!(
        "lifecycle write latency over {} posts in {parent}: p50 {} us, p95 {} us, p99 {} us",
        micros.len(),
        at(50),
        at(95),
        at(99)
    );
}
/// A bare name relative to the call's working directory has no `/` and no leading dot, and is a
/// secret all the same: the call is a stub, and nothing of its result reaches the store.
#[tokio::test]
async fn a_bare_relative_sensitive_name_is_a_stub() {
    let mut server = TestServer::start().await;
    let secret = common::ghp();
    let marker = "kestrel-secret-contents";
    let read = event("post-tool-use-read", "a", 0, 1).json(|v| {
        v["tool_input"]["file_path"] = json!("credentials.json");
        v["tool_response"] = json!({ "content": format!("{marker} {secret}") });
    });

    stored(&server, &read).await;

    let payload = payload_of(&server, "a");
    assert_eq!(
        payload["input"],
        json!({ "sensitive_path": "credentials.json" })
    );
    assert!(
        payload["result"].is_null(),
        "a stub keeps no result: {payload}"
    );
    assert_eq!(
        memory_engine::read_redaction_counts(&server.db())
            .unwrap()
            .stubbed,
        1
    );
    server.stop().await;
    for (name, bytes) in &store_bytes(&server) {
        for planted in [marker, secret.as_str()] {
            assert!(!holds(bytes, planted.as_bytes()), "{name} holds {planted}");
        }
    }
}

/// The paths of a call are the adapter's copies of words already in its input, so they are not
/// text the body held a second time: a command of many words, well inside the body limit, is
/// stored truncated, not refused. Counted twice, its 12 MB would be 25 MB.
#[tokio::test]
async fn a_bash_command_of_many_short_words_is_stored_truncated() {
    let server = TestServer::start().await;
    let command = format!("{} ", "abcdefgh".repeat(4)).repeat(400_000);
    let big = event("post-tool-use-bash", "a", 0, 1)
        .json(|v| v["tool_input"]["command"] = json!(command));
    assert!(
        (10_000_000..BODY_LIMIT).contains(&big.body.len()),
        "{}",
        big.body.len()
    );

    let posted = server.post_event("laptop", &big).await;

    assert!(posted.is(204, Some("stored")), "{posted:?}");
    let payload = payload_of(&server, "a");
    let input = payload["input"].as_str().expect("an excerpt of the input");
    assert!(input.len() <= 512, "{}", input.len());
    assert!(input.contains("abcdefgh"), "{input}");
}

/// #1121: live capture redacts the author denylist from its content (a prompt, a tool call's
/// input and result, a final message and its memory) with the upload route's rules, and never
/// from an identifier: the machine, the receipts and the project key keep the listed names.
#[tokio::test]
async fn live_capture_redacts_the_author_denylist_never_an_identifier() {
    let mut server = TestServer::start_with_denylist(&["fixturehost", "laptop"]).await;
    let origin = |fixture: common::Fixture| {
        fixture.header(
            "ME-Project-Origin-Url",
            "https://example.com/fixturehost/proj.git",
        )
    };
    stored(
        &server,
        &origin(saying(
            event("user-prompt-submit", "a", 0, 1),
            "prompt",
            "deploy to fixturehost now",
        )),
    )
    .await;
    stored(
        &server,
        &origin(event("post-tool-use-bash", "b", 1, 1).json(|v| {
            v["tool_input"]["command"] = json!("ssh fixturehost uptime");
            v["tool_response"]["stdout"] = json!("fixturehost: up");
        })),
    )
    .await;
    stored(
        &server,
        &origin(saying(
            event("stop", "c", 2, 1),
            "last_assistant_message",
            "fixturehost is up",
        )),
    )
    .await;

    assert_eq!(
        payload_of(&server, "a")["prompt"],
        "deploy to [REDACTED:denylist] now"
    );
    assert_eq!(
        payload_of(&server, "c")["message"],
        "[REDACTED:denylist] is up"
    );
    for id in ["a", "b", "c"] {
        let mut payload = payload_of(&server, id);
        assert_eq!(payload["machine"], "laptop", "{id}: {payload}");
        assert_eq!(
            payload["project"]["key"], "example.com/fixturehost/proj",
            "{id}: {payload}"
        );
        payload.as_object_mut().unwrap().remove("project");
        let content = payload.to_string();
        assert!(!content.contains("fixturehost"), "{id}: {content}");
    }
    assert!(
        payload_of(&server, "b")
            .to_string()
            .contains("[REDACTED:denylist]"),
        "the tool excerpt loses the literal"
    );
    let memory = server.engine.get_fact(fact_of(&server, "c")).await.unwrap();
    assert_eq!(memory.content, "[REDACTED:denylist] is up");
    assert_eq!(memory.metadata["machine"], "laptop");
    assert_eq!(
        server.number("SELECT COUNT(*) FROM activities WHERE machine = 'laptop'"),
        1
    );
    assert_eq!(
        server.number(
            "SELECT COUNT(*) FROM activities \
             WHERE args LIKE '%fixturehost%' OR result_summary LIKE '%fixturehost%'"
        ),
        0
    );
    assert_eq!(
        server.number("SELECT COUNT(*) FROM lifecycle_receipts WHERE machine = 'laptop'"),
        3
    );
    server.stop().await;
}
