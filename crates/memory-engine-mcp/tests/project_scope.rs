//! The project key over HTTP (ADR-0020 §3, §7, §8; ADR-0021 §5-§8): each request's
//! `ME-Project-*` facts decide the session's Home; `remember`, `recall` and `set_level` act on
//! it; every call record keeps the facts, the key and the rules version.

mod common;

use common::TestServer;
use serde_json::json;

/// `ME-Project-*` headers as a test sends them: name and raw value.
type Facts<'a> = Vec<(&'a str, &'a [u8])>;

const ROOT: &str = "ME-Project-Root";
const DEV: &str = "ME-Project-Dev-Root";
const UPSTREAM: &str = "ME-Project-Upstream-Url";
const ORIGIN: &str = "ME-Project-Origin-Url";
const BRANCH: &str = "ME-Project-Branch";

const LAPTOP_DEV: &str = "/home/alice/perso/dev";

/// Every file of the server's directory, as bytes.
fn store_bytes(server: &TestServer) -> Vec<(String, Vec<u8>)> {
    std::fs::read_dir(server.dir.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn project_nodes(server: &TestServer) -> usize {
    let conn = rusqlite::Connection::open(server.dir.path().join("memory.db")).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM scopes WHERE label LIKE 'project:%'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
    .try_into()
    .unwrap()
}

#[tokio::test]
async fn every_record_keeps_the_project_facts_key_and_rules() {
    let server = TestServer::start().await;
    let facts: &[(&str, &[u8])] = &[
        (ROOT, b"/home/alice/perso/dev/project"),
        (DEV, LAPTOP_DEV.as_bytes()),
        (
            UPSTREAM,
            b"https://alice:s3cret@github.com/example/project.git",
        ),
        (ORIGIN, b"git@github.com:alice/fork.git"),
        (BRANCH, b"feat/x%20y"),
    ];
    server
        .tool_with("laptop", facts, "recall", json!({ "query": "kestrel" }))
        .await;
    server
        .tool_with(
            "laptop",
            facts,
            "remember",
            json!({ "content": "kestrel kept" }),
        )
        .await;
    server
        .tool_with(
            "laptop",
            facts,
            "recall",
            json!({ "query": "kestrel", "project": "nowhere" }),
        )
        .await;
    let want = json!({
        "facts": {
            "root": "/home/alice/perso/dev/project",
            "dev_root": "/home/alice/perso/dev",
            "upstream_url": "https://github.com/example/project.git",
            "origin_url": "github.com:alice/fork.git",
            "branch": "feat/x y",
        },
        "dropped": {},
        "key": "github.com/example/project",
        "rules": 1,
    });
    let records = server.records();
    assert_eq!(records.len(), 3, "{records:?}");
    for (_, payload, _) in &records {
        assert_eq!(payload["project"], want, "{payload}");
    }
    for (_, payload, scope) in &records {
        assert_eq!(scope, "unscoped:laptop", "{payload}");
    }
}

#[tokio::test]
async fn bad_facts_are_dropped_and_flagged_never_the_call() {
    let server = TestServer::start().await;
    let long = vec![b'a'; 4097];
    let cases: Vec<(Facts, &str, &str)> = vec![
        (vec![(ROOT, b"/home/alice/a\tb")], "root", "raw_byte"),
        (vec![(ROOT, b"/home/alice/a%0Ab")], "root", "control"),
        (vec![(ROOT, b"/home/alice/a%zz")], "root", "bad_escape"),
        (vec![(ROOT, b"/home/alice/a%C3")], "root", "not_utf8"),
        (vec![(ROOT, &long)], "root", "too_long"),
        (
            vec![
                (ORIGIN, b"https://github.com/a/p"),
                (ORIGIN, b"https://github.com/b/p"),
            ],
            "origin_url",
            "repeated",
        ),
        (
            vec![(DEV, b"/home/alice/my%20dev")],
            "dev_root",
            "not_plain_absolute",
        ),
    ];
    for (facts, name, flaw) in cases {
        let reply = server
            .tool_with("laptop", &facts, "recall", json!({ "query": "kestrel" }))
            .await;
        assert!(!reply.is_error, "{name}: {reply:?}");
        let records = server.records();
        let project = &records.last().unwrap().1["project"];
        assert_eq!(project["dropped"][name], flaw, "{project}");
        assert!(project["facts"].get(name).is_none(), "{project}");
    }
}

#[tokio::test]
async fn no_file_of_the_store_holds_userinfo() {
    let mut server = TestServer::start().await;
    let facts: &[(&str, &[u8])] = &[
        (ROOT, b"/home/alice/perso/dev/project"),
        (DEV, LAPTOP_DEV.as_bytes()),
        (
            UPSTREAM,
            b"https://alice:s3cret@github.com/example/project.git",
        ),
        (ORIGIN, b"TOKEN123@github.com:alice/fork.git"),
    ];
    server
        .tool_with(
            "laptop",
            facts,
            "remember",
            json!({ "content": "kestrel kept" }),
        )
        .await;
    server
        .tool_with("laptop", facts, "recall", json!({ "query": "kestrel" }))
        .await;
    server
        .tool_with(
            "laptop",
            facts,
            "recall",
            json!({ "query": "kestrel", "project": "nowhere" }),
        )
        .await;
    server.stop().await;
    let files = store_bytes(&server);
    assert!(files.iter().any(|(name, _)| name == "memory.db"));
    for (name, bytes) in &files {
        for secret in [&b"s3cret"[..], b"TOKEN123"] {
            assert!(!holds(bytes, secret), "{name} holds {secret:?}");
        }
    }
}

#[tokio::test]
async fn a_read_never_mints_a_project() {
    let server = TestServer::start().await;
    let facts: &[(&str, &[u8])] = &[
        (ROOT, b"/home/alice/perso/dev/fresh"),
        (DEV, LAPTOP_DEV.as_bytes()),
        (ORIGIN, b"https://github.com/example/fresh"),
    ];
    server
        .tool_with("laptop", facts, "recall", json!({ "query": "kestrel" }))
        .await;
    server
        .tool_with(
            "laptop",
            facts,
            "recall",
            json!({ "query": "kestrel", "project": "github.com/example/fresh" }),
        )
        .await;
    let refused = server
        .tool_with(
            "laptop",
            facts,
            "remember",
            json!({ "content": "c", "scope": "x" }),
        )
        .await;
    assert!(refused.is_error, "{refused:?}");
    assert_eq!(project_nodes(&server), 0);
}
