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
