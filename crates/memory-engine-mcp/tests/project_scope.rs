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
    // A read's record sits under the bucket; a valid remember's under the Home it wrote to.
    for (_, payload, scope) in [&records[0], &records[2]] {
        assert_eq!(scope, "unscoped:laptop", "{payload}");
    }
    assert_eq!(
        records[1].2,
        "user:alice/project:github.com%2Fexample%2Fproject"
    );
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
    let remembered = server
        .tool_with(
            "laptop",
            facts,
            "remember",
            json!({ "content": "kestrel kept" }),
        )
        .await;
    assert!(!remembered.is_error, "{remembered:?}");
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
async fn no_file_of_the_store_holds_userinfo_quoted_from_a_tool_argument() {
    let mut server = TestServer::start().await;
    let reply = server
        .tool(
            "laptop",
            "recall",
            json!({ "query": "kestrel", "project": "https://alice:s3cret@github.com/p" }),
        )
        .await;
    assert!(
        reply.is_error && reply.body.contains("s3cret"),
        "the answer names the value: {reply:?}"
    );
    let reply = server
        .tool(
            "laptop",
            "set_level",
            json!({ "id": 1, "level": "ssh://TOKEN123@host/x" }),
        )
        .await;
    assert!(reply.is_error, "{reply:?}");
    server.stop().await;
    for (name, bytes) in &store_bytes(&server) {
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

// --- remember files under the session's Home (ADR-0020 §7-§8, ADR-0021 §7) ---

type Owned = Vec<(&'static str, Vec<u8>)>;

/// A session on a machine whose dev root is `dev`, at `rel`, with optional remotes.
fn session(dev: &str, rel: &str, origin: Option<&str>, upstream: Option<&str>) -> Owned {
    let mut facts = at_path(dev, rel);
    if let Some(origin) = origin {
        facts.push((ORIGIN, origin.as_bytes().to_vec()));
    }
    if let Some(upstream) = upstream {
        facts.push((UPSTREAM, upstream.as_bytes().to_vec()));
    }
    facts
}

async fn remember(
    server: &TestServer,
    machine: &str,
    facts: &Owned,
    args: serde_json::Value,
) -> common::Reply {
    server
        .tool_with(machine, &borrow(facts), "remember", args)
        .await
}

fn stored_id(reply: &common::Reply) -> i64 {
    assert!(!reply.is_error, "{reply:?}");
    reply.body.split(' ').nth(1).unwrap().parse().unwrap()
}

async fn sees(server: &TestServer, machine: &str, facts: &Owned, id: i64) -> bool {
    let reply = recall(server, machine, facts, json!({ "query": "kestrel" })).await;
    assert!(!reply.is_error, "{reply:?}");
    result_ids(&reply).contains(&id)
}

const P_SSH: &str = "git@github.com:example/project.git";
const P_HTTPS: &str = "https://github.com/example/project";
const P_KEY: &str = "github.com/example/project";

#[tokio::test]
async fn one_remote_is_one_project_on_every_machine() {
    let server = TestServer::start().await;
    let laptop = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let reply = remember(
        &server,
        "laptop",
        &laptop,
        json!({ "content": "kestrel shared" }),
    )
    .await;
    let id = stored_id(&reply);
    assert_eq!(
        reply.body,
        format!("stored {id} in project {P_KEY}, visible to all agents")
    );
    let desktop = session(DESKTOP_DEV, "project", Some(P_HTTPS), None);
    let worktree = session(DESKTOP_DEV, "project/.worktrees/w", Some(P_HTTPS), None);
    assert!(sees(&server, "desktop", &desktop, id).await);
    assert!(sees(&server, "desktop", &worktree, id).await);
    let reply = recall(&server, "desktop", &desktop, json!({ "query": "kestrel" })).await;
    assert_eq!(scopes(&reply), vec![P_KEY.to_owned()]);
    assert_eq!(project_nodes(&server), 1);
}

#[tokio::test]
async fn upstream_wins_over_origin() {
    let server = TestServer::start().await;
    let fork = session(
        LAPTOP_DEV,
        "project",
        Some("git@github.com:alice/fork.git"),
        Some("https://github.com/example/project.git"),
    );
    let reply = remember(
        &server,
        "laptop",
        &fork,
        json!({ "content": "kestrel upstream" }),
    )
    .await;
    let id = stored_id(&reply);
    assert!(
        reply.body.contains(&format!("in project {P_KEY},")),
        "{reply:?}"
    );
    let fork_only = session(
        LAPTOP_DEV,
        "fork",
        Some("git@github.com:alice/fork.git"),
        None,
    );
    assert!(!sees(&server, "laptop", &fork_only, id).await);
}

#[tokio::test]
async fn a_default_write_is_read_in_its_project_only() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let id =
        stored_id(&remember(&server, "laptop", &p, json!({ "content": "kestrel in p" })).await);
    assert!(sees(&server, "laptop", &p, id).await);
    assert!(
        sees(
            &server,
            "desktop",
            &session(DESKTOP_DEV, "project", Some(P_HTTPS), None),
            id
        )
        .await
    );
    assert!(
        !sees(
            &server,
            "laptop",
            &session(LAPTOP_DEV, "q", Some("https://github.com/example/q"), None),
            id
        )
        .await
    );
    assert!(!sees(&server, "laptop", &Vec::new(), id).await);
}

#[tokio::test]
async fn a_user_write_is_read_everywhere() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let reply = remember(
        &server,
        "laptop",
        &p,
        json!({ "content": "kestrel for alice", "level": "user" }),
    )
    .await;
    let id = stored_id(&reply);
    assert_eq!(
        reply.body,
        format!("stored {id} in user, visible to all agents")
    );
    assert!(sees(&server, "laptop", &p, id).await);
    assert!(
        sees(
            &server,
            "laptop",
            &session(LAPTOP_DEV, "q", Some("https://github.com/example/q"), None),
            id
        )
        .await
    );
    assert!(sees(&server, "laptop", &Vec::new(), id).await);
    assert!(sees(&server, "desktop", &Vec::new(), id).await);
    let reply = remember(
        &server,
        "laptop",
        &p,
        json!({ "content": "kestrel in p", "level": "project" }),
    )
    .await;
    assert!(
        reply.body.contains(&format!("in project {P_KEY},")),
        "{reply:?}"
    );
}

#[tokio::test]
async fn a_keyless_memory_stays_in_its_machines_bucket() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    for facts in [
        Vec::new(),
        session("/home/alice/elsewhere", "x", None, None)
            .into_iter()
            .map(|(n, v)| {
                (
                    n,
                    if n == DEV {
                        LAPTOP_DEV.as_bytes().to_vec()
                    } else {
                        v
                    },
                )
            })
            .collect(),
        vec![
            (ROOT, LAPTOP_DEV.as_bytes().to_vec()),
            (DEV, LAPTOP_DEV.as_bytes().to_vec()),
        ],
        at_path(LAPTOP_DEV, "./x"),
    ] {
        let reply = remember(
            &server,
            "laptop",
            &facts,
            json!({ "content": "kestrel keyless" }),
        )
        .await;
        let id = stored_id(&reply);
        assert_eq!(
            reply.body,
            format!("stored {id} in unscoped on laptop, visible to all agents"),
            "{facts:?}"
        );
        assert!(sees(&server, "laptop", &Vec::new(), id).await);
        assert!(!sees(&server, "laptop", &p, id).await);
        assert!(!sees(&server, "desktop", &Vec::new(), id).await);
        let swap = recall(
            &server,
            "laptop",
            &p,
            json!({ "query": "kestrel", "project": "unscoped:laptop" }),
        )
        .await;
        assert!(swap.is_error, "{swap:?}");
    }
    assert_eq!(project_nodes(&server), 0);
}

#[tokio::test]
async fn remote_less_paths_are_one_project_per_machine() {
    let server = TestServer::start().await;
    let laptop_x = stored_id(
        &remember(
            &server,
            "laptop",
            &at_path(LAPTOP_DEV, "x"),
            json!({ "content": "kestrel laptop x" }),
        )
        .await,
    );
    let desktop_x = stored_id(
        &remember(
            &server,
            "desktop",
            &at_path(DESKTOP_DEV, "x"),
            json!({ "content": "kestrel desktop x" }),
        )
        .await,
    );
    assert!(!sees(&server, "desktop", &at_path(DESKTOP_DEV, "x"), laptop_x).await);
    assert!(!sees(&server, "laptop", &at_path(LAPTOP_DEV, "x"), desktop_x).await);
    let old = stored_id(
        &remember(
            &server,
            "laptop",
            &at_path(LAPTOP_DEV, "old/x"),
            json!({ "content": "kestrel old x" }),
        )
        .await,
    );
    assert!(!sees(&server, "laptop", &at_path(LAPTOP_DEV, "x"), old).await);
    let shadow = remember(
        &server,
        "laptop",
        &at_path(LAPTOP_DEV, "github.com/example/project"),
        json!({ "content": "kestrel shadow" }),
    )
    .await;
    assert!(
        shadow
            .body
            .contains("in project laptop:github.com/example/project,"),
        "{shadow:?}"
    );
    let mut by_key = Vec::new();
    for (machine, facts) in [
        (
            "laptop",
            session(
                LAPTOP_DEV,
                "ab",
                Some("https://github.com/example/a:b"),
                None,
            ),
        ),
        (
            "laptop",
            session(
                LAPTOP_DEV,
                "v6",
                Some("ssh://git@[2001:DB8::1]:22/example/x.git"),
                None,
            ),
        ),
        ("laptop", at_path(LAPTOP_DEV, "a:b")),
    ] {
        let reply = remember(
            &server,
            machine,
            &facts,
            json!({ "content": "kestrel keyed" }),
        )
        .await;
        by_key.push(stored_id(&reply));
    }
    for (key, id) in [
        "github.com/example/a:b",
        "[2001:db8::1]/example/x",
        "laptop:a:b",
    ]
    .into_iter()
    .zip(by_key)
    {
        let reply = recall(
            &server,
            "laptop",
            &Vec::new(),
            json!({ "query": "kestrel", "project": key }),
        )
        .await;
        assert_eq!(result_ids(&reply), vec![id], "{key}");
    }
    assert_eq!(project_nodes(&server), 7);
}

#[tokio::test]
async fn no_locator_falls_to_the_bucket() {
    let server = TestServer::start().await;
    let long = session(
        LAPTOP_DEV,
        "p",
        Some("https://github.com/example/p"),
        Some(&format!("https://github.com/example/{}", "a".repeat(240))),
    );
    let reply = remember(
        &server,
        "laptop",
        &long,
        json!({ "content": "kestrel long" }),
    )
    .await;
    assert!(reply.body.contains("in unscoped on laptop,"), "{reply:?}");
    let local = session(
        LAPTOP_DEV,
        "project",
        Some("file:///srv/git/project.git"),
        Some("/srv/git/project.git"),
    );
    let reply = remember(
        &server,
        "laptop",
        &local,
        json!({ "content": "kestrel local" }),
    )
    .await;
    assert!(
        reply.body.contains("in project laptop:project,"),
        "{reply:?}"
    );
}

#[tokio::test]
async fn a_percent_encoded_non_ascii_root_derives_the_key_of_its_utf8_form() {
    let server = TestServer::start().await;
    let mut ids = Vec::new();
    for root in [
        format!("{LAPTOP_DEV}/caf%C3%A9"),
        format!("{LAPTOP_DEV}/caf%c3%a9"),
        "%2Fhome%2Falice%2Fperso%2Fdev%2Fcaf%C3%A9".to_owned(),
    ] {
        let facts = vec![
            (ROOT, root.into_bytes()),
            (DEV, LAPTOP_DEV.as_bytes().to_vec()),
        ];
        let reply = remember(
            &server,
            "laptop",
            &facts,
            json!({ "content": "kestrel café" }),
        )
        .await;
        assert!(reply.body.contains("in project laptop:café,"), "{reply:?}");
        ids.push(stored_id(&reply));
    }
    let reply = recall(
        &server,
        "laptop",
        &Vec::new(),
        json!({ "query": "kestrel", "project": "laptop:café" }),
    )
    .await;
    ids.sort_unstable();
    assert_eq!(result_ids(&reply), ids);
    let raw = vec![
        (ROOT, format!("{LAPTOP_DEV}/café").into_bytes()),
        (DEV, LAPTOP_DEV.as_bytes().to_vec()),
    ];
    let reply = remember(&server, "laptop", &raw, json!({ "content": "kestrel raw" })).await;
    assert!(reply.body.contains("in unscoped on laptop,"), "{reply:?}");
}

#[tokio::test]
async fn a_doubtful_placement_gets_one_hint() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let reply = remember(
        &server,
        "laptop",
        &p,
        json!({ "content": "I prefer rebase over merge in every project" }),
    )
    .await;
    let id = reply.body.split(' ').nth(1).unwrap().to_owned();
    assert_eq!(
        reply.body,
        format!(
            "stored {id} in project {P_KEY}, visible to all agents\nhint: this reads as true in every project; set_level({id}, \"user\") files it at the user level"
        )
    );
    let reply = remember(
        &server,
        "laptop",
        &p,
        json!({ "content": "this repo pins rust 1.89", "level": "user" }),
    )
    .await;
    let id = reply.body.split(' ').nth(1).unwrap().to_owned();
    assert_eq!(
        reply.body,
        format!(
            "stored {id} in user, visible to all agents\nhint: this reads as specific to one project; set_level({id}, \"project\") files it in project {P_KEY}"
        )
    );
    for level in ["project", "user"] {
        let reply = remember(
            &server,
            "laptop",
            &p,
            json!({ "content": "the lock file sits beside the store", "level": level }),
        )
        .await;
        assert_eq!(reply.body.lines().count(), 1, "{reply:?}");
    }
}

#[tokio::test]
async fn remember_records_its_level() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    remember(&server, "laptop", &p, json!({ "content": "kestrel a" })).await;
    remember(
        &server,
        "laptop",
        &p,
        json!({ "content": "kestrel b", "level": "user" }),
    )
    .await;
    let levels: Vec<_> = server
        .records()
        .into_iter()
        .map(|(_, p, _)| p["level"].clone())
        .collect();
    assert_eq!(levels, vec![json!("project"), json!("user")]);
}

#[tokio::test]
async fn remember_files_its_record_under_its_target() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    remember(&server, "laptop", &p, json!({ "content": "kestrel a" })).await;
    remember(
        &server,
        "laptop",
        &p,
        json!({ "content": "kestrel b", "level": "user" }),
    )
    .await;
    remember(
        &server,
        "laptop",
        &p,
        json!({ "content": "kestrel c", "level": "global" }),
    )
    .await;
    let homes: Vec<_> = server.records().into_iter().map(|(_, _, h)| h).collect();
    assert_eq!(
        homes,
        vec![
            home_of(P_KEY),
            "user:alice".to_owned(),
            "unscoped:laptop".to_owned()
        ]
    );
}

#[tokio::test]
async fn a_remember_whose_record_fails_mints_nothing() {
    let server = TestServer::start().await;
    server.refuse_event_writes();
    let p = session(
        LAPTOP_DEV,
        "fresh",
        Some("https://github.com/example/fresh"),
        None,
    );
    let reply = remember(&server, "laptop", &p, json!({ "content": "kestrel lost" })).await;
    assert!(reply.is_error, "{reply:?}");
    assert_eq!(project_nodes(&server), 0);
    let reply = recall(
        &server,
        "laptop",
        &Vec::new(),
        json!({ "query": "kestrel", "project": "github.com/example/fresh" }),
    )
    .await;
    assert!(
        reply
            .body
            .contains("unknown project: \"github.com/example/fresh\""),
        "{reply:?}"
    );
}

#[tokio::test]
async fn a_failed_fact_write_leaves_a_known_project() {
    let server = TestServer::start().await;
    server.sql("CREATE TRIGGER refuse_facts BEFORE INSERT ON facts BEGIN SELECT RAISE(ABORT, 'fact writes refused by the test'); END;");
    let p = session(
        LAPTOP_DEV,
        "fresh",
        Some("https://github.com/example/fresh"),
        None,
    );
    let reply = remember(&server, "laptop", &p, json!({ "content": "kestrel lost" })).await;
    assert!(reply.is_error, "{reply:?}");
    let records = server.records();
    assert_eq!(
        records.last().unwrap().2,
        home_of("github.com/example/fresh")
    );
    let reply = recall(
        &server,
        "laptop",
        &Vec::new(),
        json!({ "query": "kestrel", "project": "github.com/example/fresh" }),
    )
    .await;
    assert!(!reply.is_error, "{reply:?}");
    assert_eq!(result_ids(&reply), Vec::<i64>::new());
}

#[tokio::test]
async fn an_unknown_level_is_refused_and_stores_nothing() {
    let server = TestServer::start().await;
    for level in [json!("global"), json!("Project"), json!(1)] {
        let reply = remember(
            &server,
            "laptop",
            &Vec::new(),
            json!({ "content": "kestrel", "level": level }),
        )
        .await;
        assert!(reply.is_error, "{level}: {reply:?}");
    }
    assert_eq!(server.fact_count().await, 0);
}

// --- set_level (ADR-0020 §8, ADR-0021 §5, §7) ---

async fn set_level(
    server: &TestServer,
    machine: &str,
    facts: &Owned,
    id: i64,
    level: &str,
) -> common::Reply {
    server
        .tool_with(
            machine,
            &borrow(facts),
            "set_level",
            json!({ "id": id, "level": level }),
        )
        .await
}

/// The `set_level` records that moved a fact (they carry `from` and `to`), oldest first.
fn moves(server: &TestServer) -> Vec<(String, String)> {
    server
        .records()
        .into_iter()
        .filter(|(_, p, _)| p["tool"] == "set_level" && p.get("from").is_some())
        .map(|(_, p, _)| {
            (
                p["from"].as_str().unwrap().to_owned(),
                p["to"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn q_session(dev: &str) -> Owned {
    session(dev, "q", Some("https://github.com/example/q"), None)
}

#[tokio::test]
async fn set_level_moves_a_memory_and_back() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let id = stored_id(
        &remember(
            &server,
            "laptop",
            &p,
            json!({ "content": "kestrel moving" }),
        )
        .await,
    );
    let before = server.engine.get_fact(id).await.unwrap();
    let up = set_level(&server, "laptop", &p, id, "user").await;
    assert_eq!(
        up.body,
        format!("stored {id} in user, visible to all agents"),
        "{up:?}"
    );
    assert!(sees(&server, "laptop", &q_session(LAPTOP_DEV), id).await);
    let down = set_level(&server, "laptop", &p, id, "project").await;
    assert_eq!(
        down.body,
        format!("stored {id} in project {P_KEY}, visible to all agents")
    );
    assert!(!sees(&server, "laptop", &q_session(LAPTOP_DEV), id).await);
    let after = server.engine.get_fact(id).await.unwrap();
    assert_eq!(
        (
            after.content,
            after.metadata,
            after.fact_type,
            after.t_created
        ),
        (
            before.content,
            before.metadata,
            before.fact_type,
            before.t_created
        )
    );
    let again = set_level(&server, "laptop", &p, id, "project").await;
    assert_eq!(
        again.body,
        format!("unchanged: {id} is already in project {P_KEY}, visible to all agents")
    );
    let last = server.records().pop().unwrap().1;
    assert_eq!(last["from"], last["to"], "{last}");
}

#[tokio::test]
async fn a_memory_rehomes_through_the_user_level() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let id = stored_id(
        &remember(
            &server,
            "laptop",
            &p,
            json!({ "content": "kestrel rehomed" }),
        )
        .await,
    );
    set_level(&server, "laptop", &p, id, "user").await;
    let q = q_session(DESKTOP_DEV);
    let down = set_level(&server, "desktop", &q, id, "project").await;
    assert_eq!(
        down.body,
        format!("stored {id} in project github.com/example/q, visible to all agents")
    );
    assert!(
        server
            .engine
            .resolve_scope_path(&home_of("github.com/example/q"))
            .is_some()
    );
    assert!(!sees(&server, "laptop", &p, id).await);
    assert!(sees(&server, "desktop", &q, id).await);
    assert_eq!(
        moves(&server),
        vec![
            (home_of(P_KEY), "user:alice".to_owned()),
            ("user:alice".to_owned(), home_of("github.com/example/q")),
        ]
    );
}

#[tokio::test]
async fn a_keyless_session_moves_between_its_bucket_and_user() {
    let server = TestServer::start().await;
    let id = stored_id(
        &remember(
            &server,
            "laptop",
            &Vec::new(),
            json!({ "content": "kestrel keyless" }),
        )
        .await,
    );
    let up = set_level(&server, "laptop", &Vec::new(), id, "user").await;
    assert_eq!(
        up.body,
        format!("stored {id} in user, visible to all agents")
    );
    let down = set_level(&server, "laptop", &Vec::new(), id, "project").await;
    assert_eq!(
        down.body,
        format!("stored {id} in unscoped on laptop, visible to all agents")
    );
}

#[tokio::test]
async fn set_level_right_after_a_machines_first_remember() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let id =
        stored_id(&remember(&server, "laptop", &p, json!({ "content": "kestrel first" })).await);
    assert_eq!(server.engine.resolve_scope_path("unscoped:laptop"), None);
    let up = set_level(&server, "laptop", &p, id, "user").await;
    assert!(!up.is_error, "{up:?}");
    assert_eq!(server.records().last().unwrap().2, "unscoped:laptop");
}

#[tokio::test]
async fn set_level_never_reaches_another_home() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let in_q = server
        .seed(
            Some(&home_of("github.com/example/q")),
            "kestrel in q",
            json!({}),
        )
        .await;
    let swap = recall(
        &server,
        "laptop",
        &p,
        json!({ "query": "kestrel", "project": "github.com/example/q" }),
    )
    .await;
    assert_eq!(result_ids(&swap), vec![in_q]);
    let at_root = server.seed(None, "kestrel at root", json!({})).await;
    let in_bucket = server
        .seed(Some("unscoped:laptop"), "kestrel in the bucket", json!({}))
        .await;
    let on_desktop = server
        .seed(Some("unscoped:desktop"), "kestrel on desktop", json!({}))
        .await;
    let expired = server
        .seed(Some(&home_of(P_KEY)), "kestrel expired", json!({}))
        .await;
    let ended = server
        .seed(Some(&home_of(P_KEY)), "kestrel ended", json!({}))
        .await;
    let later = server
        .seed(Some(&home_of(P_KEY)), "kestrel ends later", json!({}))
        .await;
    server.sql(&format!(
        "UPDATE facts SET t_expired = '2026-01-01T00:00:00+00:00' WHERE id = {expired}"
    ));
    server.sql(&format!(
        "UPDATE facts SET t_invalid = '2026-01-01T00:00:00+00:00' WHERE id = {ended}"
    ));
    server.sql(&format!(
        "UPDATE facts SET t_invalid = '2999-01-01T00:00:00+00:00' WHERE id = {later}"
    ));
    let before = server.engine.statistics().await.unwrap();
    for id in [
        in_q, at_root, in_bucket, on_desktop, 999_999, expired, ended,
    ] {
        let reply = set_level(&server, "laptop", &p, id, "user").await;
        assert!(reply.is_error, "{id}: {reply:?}");
        assert_eq!(reply.body, "error: not found", "{id}");
    }
    let after = server.engine.statistics().await.unwrap();
    assert_eq!(after.facts.total, before.facts.total);
    for id in [in_q, at_root, in_bucket, on_desktop, expired, ended] {
        let fact = server.engine.get_fact(id).await.unwrap();
        assert_ne!(
            Some(fact.scope_id),
            server.engine.resolve_scope_path("user:alice"),
            "{id}"
        );
    }
    let reply = set_level(&server, "laptop", &p, later, "user").await;
    assert!(!reply.is_error, "a future t_invalid is live: {reply:?}");
}

#[tokio::test]
async fn a_move_whose_record_fails_does_not_happen() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let id =
        stored_id(&remember(&server, "laptop", &p, json!({ "content": "kestrel stays" })).await);
    server.refuse_event_writes();
    let reply = set_level(&server, "laptop", &p, id, "user").await;
    assert!(reply.is_error && reply.body.contains("record"), "{reply:?}");
    let fact = server.engine.get_fact(id).await.unwrap();
    assert_eq!(
        Some(fact.scope_id),
        server.engine.resolve_scope_path(&home_of(P_KEY))
    );
}

#[tokio::test]
async fn a_record_never_names_a_move_that_failed() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let id =
        stored_id(&remember(&server, "laptop", &p, json!({ "content": "kestrel stays" })).await);
    server.sql("CREATE TRIGGER refuse_moves BEFORE UPDATE OF scope_id ON facts BEGIN SELECT RAISE(ABORT, 'moves refused by the test'); END;");
    let reply = set_level(&server, "laptop", &p, id, "user").await;
    assert!(reply.is_error, "{reply:?}");
    assert_eq!(moves(&server), Vec::<(String, String)>::new());
    let fact = server.engine.get_fact(id).await.unwrap();
    assert_eq!(
        Some(fact.scope_id),
        server.engine.resolve_scope_path(&home_of(P_KEY))
    );
}

#[tokio::test]
async fn concurrent_moves_leave_a_replayable_log() {
    let server = TestServer::start().await;
    let p = session(LAPTOP_DEV, "project", Some(P_SSH), None);
    let q = q_session(DESKTOP_DEV);
    for _ in 0..8 {
        let id = stored_id(
            &remember(&server, "laptop", &p, json!({ "content": "kestrel raced" })).await,
        );
        let (up_one, up_two) = tokio::join!(
            set_level(&server, "laptop", &p, id, "user"),
            set_level(&server, "laptop", &p, id, "user")
        );
        let (down_p, down_q) = tokio::join!(
            set_level(&server, "laptop", &p, id, "project"),
            set_level(&server, "desktop", &q, id, "project")
        );
        for reply in [up_one, up_two, down_p, down_q] {
            assert!(
                !reply.is_error
                    || reply.body == "error: not found"
                    || reply.body.contains("changed while it was being moved"),
                "{reply:?}"
            );
        }
        let path: Vec<(String, String)> = server
            .records()
            .into_iter()
            .filter(|(_, r, _)| {
                r["tool"] == "set_level" && r.get("from").is_some() && r["fact_ids"] == json!([id])
            })
            .map(|(_, r, _)| {
                (
                    r["from"].as_str().unwrap().to_owned(),
                    r["to"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let mut at = home_of(P_KEY);
        for (from, to) in &path {
            assert_eq!(from, &at, "{path:?}");
            at.clone_from(to);
        }
        assert_eq!(
            server.engine.explain_fact(id).await.unwrap().scope_path,
            at,
            "{path:?}"
        );
    }
}
