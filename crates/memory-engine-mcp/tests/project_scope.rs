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
const DESKTOP_DEV: &str = "/home/alice/dev";

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

// --- recall reads the session's project (ADR-0020 §7, ADR-0021 §5) ---

/// A laptop session whose origin is `github.com/example/<name>`.
fn in_repo(name: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (ROOT, format!("{LAPTOP_DEV}/{name}").into_bytes()),
        (DEV, LAPTOP_DEV.as_bytes().to_vec()),
        (
            ORIGIN,
            format!("https://github.com/example/{name}").into_bytes(),
        ),
    ]
}

/// A remote-less session at `<dev>/<rel>`.
fn at_path(dev: &str, rel: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (ROOT, format!("{dev}/{rel}").into_bytes()),
        (DEV, dev.as_bytes().to_vec()),
    ]
}

fn borrow<'a>(facts: &'a [(&'static str, Vec<u8>)]) -> Facts<'a> {
    facts.iter().map(|(n, v)| (*n, v.as_slice())).collect()
}

fn home_of(key: &str) -> String {
    let label: String = key
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("user:alice/project:{label}")
}

fn result_ids(reply: &common::Reply) -> Vec<i64> {
    let mut ids: Vec<i64> = reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

fn scopes(reply: &common::Reply) -> Vec<String> {
    reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["scope"].as_str().unwrap().to_owned())
        .collect()
}

async fn recall(
    server: &TestServer,
    machine: &str,
    facts: &[(&'static str, Vec<u8>)],
    args: serde_json::Value,
) -> common::Reply {
    server
        .tool_with(machine, &borrow(facts), "recall", args)
        .await
}

#[tokio::test]
async fn recall_reads_exactly_the_project_and_the_user_level() {
    let server = TestServer::start().await;
    let p = "github.com/example/p";
    let mut want = Vec::new();
    for i in 0..3 {
        want.push(
            server
                .seed(Some(&home_of(p)), &format!("kestrel {i} in p"), json!({}))
                .await,
        );
    }
    for i in 0..2 {
        want.push(
            server
                .seed(
                    Some("user:alice"),
                    &format!("kestrel {i} for alice"),
                    json!({}),
                )
                .await,
        );
    }
    let strong = "kestrel kestrel kestrel kestrel kestrel";
    for home in [
        None,
        Some(home_of("github.com/example/q")),
        Some("unscoped:laptop".to_owned()),
        Some("unscoped:desktop".to_owned()),
    ] {
        for _ in 0..5 {
            server.seed(home.as_deref(), strong, json!({})).await;
        }
    }
    let reply = recall(
        &server,
        "laptop",
        &in_repo("p"),
        json!({ "query": "kestrel" }),
    )
    .await;
    assert!(!reply.is_error, "{reply:?}");
    want.sort_unstable();
    assert_eq!(result_ids(&reply), want);
    let labels = scopes(&reply);
    assert_eq!(labels.iter().filter(|s| *s == p).count(), 3, "{labels:?}");
    assert_eq!(
        labels.iter().filter(|s| *s == "user").count(),
        2,
        "{labels:?}"
    );
}

#[tokio::test]
async fn an_unminted_project_reads_the_user_level_only() {
    let server = TestServer::start().await;
    let at_user = server
        .seed(Some("user:alice"), "kestrel for alice", json!({}))
        .await;
    server
        .seed(Some("unscoped:laptop"), "kestrel in the bucket", json!({}))
        .await;
    let reply = recall(
        &server,
        "laptop",
        &in_repo("fresh"),
        json!({ "query": "kestrel" }),
    )
    .await;
    assert_eq!(result_ids(&reply), vec![at_user]);
    assert_eq!(project_nodes(&server), 0);
}

#[tokio::test]
async fn an_unminted_well_formed_key_is_an_unknown_project() {
    let server = TestServer::start().await;
    recall(
        &server,
        "desktop",
        &at_path(DESKTOP_DEV, "x"),
        json!({ "query": "kestrel" }),
    )
    .await;
    let reply = recall(
        &server,
        "laptop",
        &[],
        json!({ "query": "kestrel", "project": "desktop:x" }),
    )
    .await;
    assert!(
        reply.is_error && reply.body.contains("unknown project: \"desktop:x\""),
        "{reply:?}"
    );
}

#[tokio::test]
async fn another_machines_path_key_is_read_only_when_named() {
    let server = TestServer::start().await;
    let on_laptop = server
        .seed(Some(&home_of("laptop:x")), "kestrel on laptop", json!({}))
        .await;
    let on_desktop = server
        .seed(Some(&home_of("desktop:x")), "kestrel on desktop", json!({}))
        .await;
    let at_user = server
        .seed(Some("user:alice"), "kestrel for alice", json!({}))
        .await;
    let own = recall(
        &server,
        "desktop",
        &at_path(DESKTOP_DEV, "x"),
        json!({ "query": "kestrel" }),
    )
    .await;
    let mut want = vec![on_desktop, at_user];
    want.sort_unstable();
    assert_eq!(result_ids(&own), want);
    let named = recall(
        &server,
        "desktop",
        &at_path(DESKTOP_DEV, "x"),
        json!({ "query": "kestrel", "project": "laptop:x" }),
    )
    .await;
    let mut want = vec![on_laptop, at_user];
    want.sort_unstable();
    assert_eq!(result_ids(&named), want);
    assert!(scopes(&named).contains(&"laptop:x".to_owned()));
    let bare = recall(
        &server,
        "desktop",
        &at_path(DESKTOP_DEV, "x"),
        json!({ "query": "kestrel", "project": "x" }),
    )
    .await;
    assert!(
        bare.is_error && bare.body.contains("unknown project: \"x\""),
        "{bare:?}"
    );
}

#[tokio::test]
async fn the_swap_reads_the_named_project_and_never_the_own() {
    let server = TestServer::start().await;
    server
        .seed(
            Some(&home_of("github.com/example/p")),
            "kestrel in p",
            json!({}),
        )
        .await;
    let in_q = server
        .seed(
            Some(&home_of("github.com/example/q")),
            "kestrel in q",
            json!({}),
        )
        .await;
    let at_user = server
        .seed(Some("user:alice"), "kestrel for alice", json!({}))
        .await;
    let reply = recall(
        &server,
        "laptop",
        &in_repo("p"),
        json!({ "query": "kestrel", "project": "github.com/example/q" }),
    )
    .await;
    let mut want = vec![in_q, at_user];
    want.sort_unstable();
    assert_eq!(result_ids(&reply), want);
}
