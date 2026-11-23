//! `POST /backfill/uploads` (#1026), driven with the checked-in synthetic Claude Code transcripts
//! and native memory files.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

mod common;

use common::{TestServer, fixture, holds, store_bytes, token};

const SESSION: &str = "52c6c7b7-c890-43ec-81a5-a1cf99d3cc84";
const SOURCE: &str = "projects/-work-fixture-p/52c6c7b7-c890-43ec-81a5-a1cf99d3cc84.jsonl";
const DENYLIST_VARIABLE: &str = "ME_REDACT_DENYLIST_FILE";

/// The main session file.
fn transcript() -> Vec<u8> {
    std::fs::read(fixtures_dir().join(format!(
        "claude-code-transcripts/-work-fixture-p/{SESSION}.jsonl"
    )))
    .unwrap()
}

/// The subagent file of the main session.
fn subagent() -> Vec<u8> {
    std::fs::read(fixtures_dir().join(format!(
        "claude-code-transcripts/-work-fixture-p/{SESSION}/subagents/agent-a3c91f2.jsonl"
    )))
    .unwrap()
}

fn memory_file(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join("claude-code-memory").join(name)).unwrap()
}

/// One upload: its headers and its body. Each builder method changes a copy.
#[derive(Clone)]
struct Upload {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Upload {
    /// A dry run of the main session file, with the project facts of its manifest row.
    fn transcript() -> Self {
        Self::of("transcript", SOURCE, transcript())
    }

    /// A dry run of the memory file `name`, copied at `mtime`.
    fn memory(name: &str, mtime: &str) -> Self {
        Self::of("memory-file", &format!("memory/{name}"), memory_file(name))
            .header("ME-Backfill-Mtime", mtime)
    }

    fn of(kind: &str, source: &str, body: Vec<u8>) -> Self {
        let headers = [
            ("ME-Harness", "claude-code"),
            ("ME-Backfill-Kind", kind),
            ("ME-Backfill-Mode", "dry-run"),
            ("ME-Backfill-Source", source),
            ("ME-Client-Version", "0.1.0"),
            ("ME-Project-Root", "/work/fixture/p"),
            ("ME-Project-Dev-Root", "/work/fixture"),
            ("ME-Project-Origin-Url", "https://example.com/example/p.git"),
        ];
        Self {
            headers: headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
            body,
        }
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    fn without(mut self, name: &str) -> Self {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self
    }

    fn repeated(mut self, name: &str) -> Self {
        let value = self
            .headers
            .iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1
            .clone();
        self.headers.push((name.to_owned(), value));
        self
    }

    fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// The same upload as a write under `preview`.
    fn write(self, preview: &str) -> Self {
        self.header("ME-Backfill-Mode", "write")
            .header("ME-Backfill-Preview", preview)
    }
}

/// What the route answered: the status and the JSON body (`Null` when there is none).
async fn send(server: &TestServer, machine: &str, upload: &Upload) -> (u16, Value) {
    let mut request = server
        .client
        .post(format!("{}/backfill/uploads", server.url))
        .bearer_auth(token(machine))
        .body(upload.body.clone());
    for (name, value) in &upload.headers {
        request = request.header(name, value);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// A dry run of `upload`, then its write under the dry run's preview; both reports.
async fn dry_run_then_write(server: &TestServer, upload: &Upload) -> (Value, Value) {
    let (status, dry) = send(server, "laptop", upload).await;
    assert_eq!(status, 200, "{dry}");
    let preview = dry["preview"].as_str().unwrap().to_owned();
    let (status, write) = send(server, "laptop", &upload.clone().write(&preview)).await;
    assert_eq!(status, 200, "{write}");
    assert_eq!(write["committed"], true, "{write}");
    (dry, write)
}

/// Open a connection, send `upload`'s headers with `Content-Length: claimed` and the first
/// `sent` bytes of its body, and return the stream, its body left half-sent.
async fn half_sent(
    server: &TestServer,
    upload: &Upload,
    claimed: u64,
    sent: &[u8],
) -> tokio::net::TcpStream {
    use std::fmt::Write as _;
    use tokio::io::AsyncWriteExt;
    let host = server.url.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let mut head = format!(
        "POST /backfill/uploads HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\nContent-Length: {claimed}\r\n",
        token("laptop")
    );
    for (name, value) in &upload.headers {
        write!(head, "{name}: {value}\r\n").unwrap();
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(sent).await.unwrap();
    stream
}

/// The status the route answers from `upload`'s headers alone, its body claimed and never sent.
async fn status_from_headers(server: &TestServer, upload: &Upload, claimed: u64) -> u16 {
    use tokio::io::AsyncReadExt;
    let mut stream = half_sent(server, upload, claimed, b"").await;
    let mut answer = vec![0; 12];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut answer))
        .await
        .expect("an answer before any body byte")
        .unwrap();
    String::from_utf8(answer[9..12].to_vec())
        .unwrap()
        .parse()
        .unwrap()
}

/// The rows an upload can write.
fn rows(server: &TestServer) -> [i64; 5] {
    [
        "SELECT COUNT(*) FROM events",
        "SELECT COUNT(*) FROM facts",
        "SELECT COUNT(*) FROM lifecycle_receipts",
        "SELECT COUNT(*) FROM lifecycle_segments",
        "SELECT COUNT(*) FROM activities",
    ]
    .map(|sql| server.number(sql))
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every file under `dir`, its subdirectories included, sorted.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn string_leaves<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| string_leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_leaves(v, out)),
        _ => {}
    }
}

fn holds_no_local_path(path: &Path, text: &str) {
    for local in ["/home/", "/Users/", "/tmp/", "/var/folders/"] {
        assert!(!text.contains(local), "{} holds {local}", path.display());
    }
}

/// The repository is public: no fixture holds a local path or a string a redaction rule would
/// change. Every transcript line but the one malformed on purpose is a JSON object.
#[test]
fn fixtures_are_public_safe() {
    let transcripts = files(&fixtures_dir().join("claude-code-transcripts"));
    let mut malformed = 0;
    let mut lines = 0;
    for path in transcripts
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
    {
        let text = std::fs::read_to_string(path).unwrap();
        holds_no_local_path(path, &text);
        for line in text.lines() {
            lines += 1;
            let Ok(json) = serde_json::from_str::<Value>(line) else {
                malformed += 1;
                continue;
            };
            assert!(json.is_object(), "{}: {line}", path.display());
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
        }
    }
    assert_eq!(malformed, 1, "exactly one line is malformed on purpose");
    assert!(lines > 40, "{lines} transcript lines");

    let memories = files(&fixtures_dir().join("claude-code-memory"));
    assert!(memories.len() >= 6, "{memories:?}");
    for path in &memories {
        let text = String::from_utf8(std::fs::read(path).unwrap()).unwrap();
        holds_no_local_path(path, &text);
        assert_eq!(
            memory_engine::redact::scrub(&text).unwrap(),
            text,
            "{}: a redaction rule matches the file",
            path.display()
        );
    }
    let readme = fixtures_dir().join("claude-code-transcripts/README.md");
    holds_no_local_path(&readme, &std::fs::read_to_string(&readme).unwrap());
}

/// Every check of decision 3, in its order, and nothing stored on any refusal.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the route's whole check table, one step after the other
async fn the_upload_checks_run_in_order() {
    let server = TestServer::start_with_denylist(&[]).await;
    let ok = Upload::transcript();
    let error = |answer: &(u16, Value)| answer.1["error"].as_str().unwrap_or_default().to_owned();

    // 2: a required header missing, repeated, too long or off its grammar.
    for name in [
        "ME-Harness",
        "ME-Backfill-Kind",
        "ME-Backfill-Mode",
        "ME-Backfill-Source",
        "ME-Client-Version",
    ] {
        let missing = send(&server, "laptop", &ok.clone().without(name)).await;
        assert_eq!(
            (missing.0, error(&missing)),
            (400, name.to_owned()),
            "missing {name}"
        );
        let repeated = send(&server, "laptop", &ok.clone().repeated(name)).await;
        assert_eq!(
            (repeated.0, error(&repeated)),
            (400, name.to_owned()),
            "repeated {name}"
        );
        let long = send(
            &server,
            "laptop",
            &ok.clone().header(name, &"a".repeat(4097)),
        )
        .await;
        assert_eq!(long.0, 400, "a long {name}");
    }
    for (name, value) in [
        ("ME-Harness", "Claude-Code"),
        ("ME-Backfill-Kind", "trans_script"),
        ("ME-Backfill-Kind", &"a".repeat(33)),
        ("ME-Backfill-Mode", "commit"),
        ("ME-Backfill-Source", "/abs/path.jsonl"),
        ("ME-Backfill-Source", "a/../b.jsonl"),
        ("ME-Backfill-Source", "./b.jsonl"),
        ("ME-Backfill-Source", "a%0Ab.jsonl"),
        ("ME-Backfill-Source", "a%zz"),
        ("ME-Client-Version", "0 1"),
        ("ME-Backfill-Mtime", "2026-03-19 10:00:00"),
    ] {
        let answer = send(&server, "laptop", &ok.clone().header(name, value)).await;
        assert_eq!(
            (answer.0, error(&answer)),
            (400, name.to_owned()),
            "{name}: {value}"
        );
    }
    let memory = Upload::memory("user_role.md", "2026-03-20T08:00:00Z");
    let no_mtime = send(
        &server,
        "laptop",
        &memory.clone().without("ME-Backfill-Mtime"),
    )
    .await;
    assert_eq!(
        (no_mtime.0, error(&no_mtime)),
        (400, "ME-Backfill-Mtime".to_owned())
    );
    let no_preview = send(
        &server,
        "laptop",
        &ok.clone().header("ME-Backfill-Mode", "write"),
    )
    .await;
    assert_eq!(
        (no_preview.0, error(&no_preview)),
        (400, "ME-Backfill-Preview".to_owned())
    );
    for preview in ["A".repeat(64), "a".repeat(63), "g".repeat(64)] {
        let answer = send(&server, "laptop", &ok.clone().write(&preview)).await;
        assert_eq!(
            (answer.0, error(&answer)),
            (400, "ME-Backfill-Preview".to_owned())
        );
    }

    // 3: an unknown harness or kind, before any body byte; an envelope fault comes first.
    let codex = ok.clone().header("ME-Harness", "codex");
    assert_eq!(status_from_headers(&server, &codex, 10).await, 422);
    let kind = ok.clone().header("ME-Backfill-Kind", "no-such-kind");
    assert_eq!(status_from_headers(&server, &kind, 10).await, 422);
    let both = codex.clone().without("ME-Client-Version");
    assert_eq!(send(&server, "laptop", &both).await.0, 400);

    // 5: a declared size over 256 MiB, from the headers alone.
    let over = u64::try_from(memory_engine::BACKFILL_BODY_BYTES).unwrap() + 1;
    assert_eq!(status_from_headers(&server, &ok, over).await, 413);
    assert_eq!(
        status_from_headers(&server, &kind, over).await,
        422,
        "the kind first"
    );

    // 6: one upload at a time; the slot is taken before the body is read.
    let held = half_sent(&server, &ok, 1000, b"{").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let busy = send(&server, "laptop", &ok).await;
    assert_eq!(busy.0, 503, "{}", busy.1);
    assert_eq!(
        status_from_headers(&server, &ok, over).await,
        413,
        "the size first"
    );
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 9: a body the reader cannot read at all.
    let unread = send(
        &server,
        "laptop",
        &ok.clone().body(b"[1]\nnot json\n".to_vec()),
    )
    .await;
    assert_eq!((unread.0, error(&unread)), (422, "no session".to_owned()));
    let latin = memory.clone().body(b"caf\xe9".to_vec());
    assert_eq!(send(&server, "laptop", &latin).await.0, 422);
    assert_eq!(rows(&server), [0; 5], "nothing stored on any refusal");
    let accepted = send(&server, "laptop", &ok).await;
    assert_eq!(accepted.0, 200, "{}", accepted.1);
    assert_eq!(rows(&server), [0; 5], "a dry run stores nothing");

    // 4: without a denylist every upload is refused, a dry run included, before its size.
    let refusing = TestServer::start().await;
    assert_eq!(status_from_headers(&refusing, &ok, over).await, 503);
    let refused = send(&refusing, "laptop", &ok).await;
    assert_eq!(refused.0, 503);
    assert!(error(&refused).contains(DENYLIST_VARIABLE), "{}", refused.1);
    assert_eq!(
        status_from_headers(&refusing, &kind, 10).await,
        422,
        "the kind first"
    );
}

/// A write commits only the effects its dry run showed: the dry run's preview commits, and any
/// change since (the body, a header, the denylist, the store) is a 412 that stores nothing.
#[tokio::test]
async fn a_write_needs_the_preview_of_its_dry_run() {
    let mut server = TestServer::start_with_denylist(&[]).await;
    let upload = Upload::transcript();
    let (_, dry) = send(&server, "laptop", &upload).await;
    let preview = dry["preview"].as_str().unwrap().to_owned();
    assert_eq!(preview.len(), 64);

    let changed_body = upload.clone().body(subagent()).write(&preview);
    let other_fact = upload
        .clone()
        .header("ME-Project-Origin-Url", "https://example.com/example/q.git")
        .write(&preview);
    // An edit no item reads, in a skipped `progress` entry, is still another body.
    let text = String::from_utf8(transcript()).unwrap();
    assert!(text.contains(r#""hookName":"Stop""#));
    let skipped_edit = upload
        .clone()
        .body(text.replacen(r#""hookName":"Stop""#, r#""hookName":"Edited""#, 1))
        .write(&preview);
    for stale in [changed_body, other_fact, skipped_edit] {
        let answer = send(&server, "laptop", &stale).await;
        assert_eq!(answer.0, 412, "{}", answer.1);
        assert_eq!(rows(&server), [0; 5]);
    }
    let memory = Upload::memory("user_role.md", "2026-03-20T08:00:00Z");
    let (_, memory_dry) = send(&server, "laptop", &memory).await;
    let memory_preview = memory_dry["preview"].as_str().unwrap();
    let recopied = memory
        .clone()
        .header("ME-Backfill-Mtime", "2026-03-21T08:00:00Z")
        .write(memory_preview);
    assert_eq!(send(&server, "laptop", &recopied).await.0, 412);

    server
        .restart_with_denylist(Some(vec!["verbose".to_owned()]))
        .await;
    let answer = send(&server, "laptop", &upload.clone().write(&preview)).await;
    assert_eq!(answer.0, 412, "another denylist since the dry run");
    server.restart_with_denylist(Some(Vec::new())).await;

    let live = fixture("user-prompt-submit")
        .header("ME-Event-Time", "2026-03-19T10:52:00Z")
        .json(|b| b["session_id"] = json!(SESSION));
    assert!(
        server
            .post_event("laptop", &live)
            .await
            .is(204, Some("stored"))
    );
    let answer = send(&server, "laptop", &upload.clone().write(&preview)).await;
    assert_eq!(answer.0, 412, "a capture of the session since the dry run");

    let (_, dry) = send(&server, "laptop", &upload).await;
    let fresh = dry["preview"].as_str().unwrap();
    let (status, write) = send(&server, "laptop", &upload.clone().write(fresh)).await;
    assert_eq!(status, 200, "{write}");
    assert_eq!(write["committed"], true);
    assert_eq!(write["preview"], dry["preview"]);
}

/// The report holds no byte the store would not: canaries in the source and in every content
/// field are redacted, and a machine token in a project fact appears nowhere.
#[tokio::test]
async fn the_report_never_echoes_an_unredacted_byte() {
    let server = TestServer::start_with_denylist(&["fixturehost"]).await;
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let ghp = common::ghp();
    let token = common::machine_token();
    let secret = |what: &str| format!("{what} {aws} {ghp} on fixturehost");
    let entry = |uuid: &str, kind: &str, rest: &Value| {
        let mut entry = json!({
            "type": kind, "uuid": uuid, "timestamp": "2026-03-19T10:00:00.000Z",
            "sessionId": "s", "cwd": format!("/work/fixturehost/{aws}"), "version": "2.1.310",
        });
        entry
            .as_object_mut()
            .unwrap()
            .extend(rest.as_object().unwrap().clone());
        entry.to_string()
    };
    let lines = [
        entry(
            "u1",
            "user",
            &json!({ "message": { "content": secret("prompt") } }),
        ),
        entry(
            "u2",
            "assistant",
            &json!({ "message": { "id": "m1", "content": [
            { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": secret("input") } },
        ], "stop_reason": "tool_use" } }),
        ),
        entry(
            "u3",
            "user",
            &json!({ "message": { "content": [
            { "type": "tool_result", "tool_use_id": "t1", "content": secret("result") },
        ] } }),
        ),
        entry(
            "u4",
            "assistant",
            &json!({ "message": { "id": "m2", "content": [
            { "type": "text", "text": secret("final") },
        ], "stop_reason": "end_turn" } }),
        ),
        entry(
            "u5",
            "user",
            &json!({ "isCompactSummary": true, "message": { "content": secret("summary") } }),
        ),
    ];
    let upload = Upload::transcript()
        .body(lines.join("\n").into_bytes())
        .header(
            "ME-Backfill-Source",
            &format!("projects/fixturehost/{aws}.jsonl"),
        )
        .header("ME-Project-Branch", &token);

    let (status, report) = send(&server, "laptop", &upload).await;

    assert_eq!(status, 200, "{report}");
    let text = report.to_string();
    for canary in [aws.as_str(), ghp.as_str(), token.as_str(), "fixturehost"] {
        assert!(!text.contains(canary), "the report echoes {canary}: {text}");
    }
    assert_eq!(report["project"]["dropped"]["branch"], "machine_token");
    assert_eq!(
        report["facts"]["items"].as_array().unwrap().len(),
        2,
        "{report}"
    );
    let (_, write) = dry_run_then_write(&server, &upload).await;
    let text = write.to_string();
    for canary in [aws.as_str(), ghp.as_str(), token.as_str(), "fixturehost"] {
        assert!(!text.contains(canary), "the write's report echoes {canary}");
    }
    for (_, bytes) in store_bytes(&server) {
        for canary in [&aws, &ghp, &token] {
            assert!(!holds(&bytes, canary.as_bytes()), "{canary} is on disk");
        }
    }
}

/// The route redacts with the author denylist: every content member an upload stores loses the
/// literal, the provenance keeps it (machine-token rule only), and the report counts it.
#[tokio::test]
async fn an_upload_redacts_with_the_author_denylist() {
    let server = TestServer::start_with_denylist(&["verbose"]).await;

    let (dry, write) = dry_run_then_write(&server, &Upload::transcript()).await;

    assert_eq!(dry["denylist_literals"], 1);
    assert!(
        write["redaction"]["placeholders"]["denylist"]
            .as_u64()
            .unwrap()
            >= 5,
        "{write}"
    );
    let conn = rusqlite::Connection::open(server.db()).unwrap();
    let payloads: Vec<String> = conn
        .prepare("SELECT payload FROM events WHERE source = 'backfill'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(payloads.len() > 15);
    for payload in &payloads {
        let payload: Value = serde_json::from_str(payload).unwrap();
        let mut content = payload.clone();
        content.as_object_mut().unwrap().remove("project");
        assert!(!content.to_string().contains("verbose"), "{content}");
        assert_eq!(payload["project"]["facts"]["root"], "/work/fixture/p");
    }
    let facts: Vec<String> = conn
        .prepare("SELECT content FROM facts")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        facts.iter().any(|f| f.contains("[REDACTED:denylist]")),
        "{facts:?}"
    );
    assert!(facts.iter().all(|f| !f.contains("verbose")));
}

// ── The acceptance (#1026) ───────────────────────────────────────────────────

/// The project facts of the upload's manifest row, as a session of that project sends them.
fn project_session() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("ME-Project-Root", b"/work/fixture/p".to_vec()),
        ("ME-Project-Dev-Root", b"/work/fixture".to_vec()),
        (
            "ME-Project-Origin-Url",
            b"https://example.com/example/p.git".to_vec(),
        ),
    ]
}

fn other_project() -> Vec<(&'static str, Vec<u8>)> {
    vec![(
        "ME-Project-Origin-Url",
        b"https://example.com/example/q.git".to_vec(),
    )]
}

/// What `recall` of `query` finds for a session with `facts`: each result's content and kind.
async fn recall(
    server: &TestServer,
    facts: &[(&'static str, Vec<u8>)],
    query: &str,
) -> Vec<(i64, String, String)> {
    let borrowed: Vec<(&str, &[u8])> = facts.iter().map(|(n, v)| (*n, v.as_slice())).collect();
    let reply = server
        .tool_with("laptop", &borrowed, "recall", json!({ "query": query }))
        .await;
    assert!(!reply.is_error, "{reply:?}");
    let mut found: Vec<(i64, String, String)> = reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_i64().unwrap(),
                r["content"].as_str().unwrap().to_owned(),
                r["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    found.sort();
    found
}

/// The imported events' payloads, in order-key order.
fn imported_events(server: &TestServer) -> Vec<Value> {
    let conn = rusqlite::Connection::open(server.db()).unwrap();
    let payloads: Vec<String> = conn
        .prepare(
            "SELECT payload FROM events WHERE source = 'backfill' ORDER BY timestamp,
                    CAST(json_extract(payload, '$.seq') AS INTEGER),
                    json_extract(payload, '$.event_id')",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    payloads
        .iter()
        .map(|p| serde_json::from_str(p).unwrap())
        .collect()
}

/// Every table's row count, and the facts' summed access counts.
fn table_counts(server: &TestServer) -> Vec<(String, i64)> {
    let conn = rusqlite::Connection::open(server.db()).unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut counts: Vec<(String, i64)> = names
        .into_iter()
        .map(|name| {
            let n = conn
                .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |r| {
                    r.get(0)
                })
                .unwrap();
            (name, n)
        })
        .collect();
    counts.push((
        "access_count".into(),
        server.number("SELECT COALESCE(SUM(access_count), 0) FROM facts"),
    ));
    counts
}

/// One transcript line of session `s`, at `time`, with `rest`.
fn entry(uuid: &str, kind: &str, time: &str, cwd: &str, rest: &Value) -> String {
    let mut entry = json!({
        "type": kind, "uuid": uuid, "timestamp": time, "sessionId": "s",
        "cwd": cwd, "version": "2.1.310",
    });
    entry
        .as_object_mut()
        .unwrap()
        .extend(rest.as_object().unwrap().clone());
    entry.to_string()
}

/// A transcript of one turn: a prompt, a tool call and its result, a final message, a summary.
fn one_turn(prefix: &str, cwd: &str, text: &dyn Fn(&str) -> String) -> Vec<u8> {
    let at = |s: u32| format!("2026-03-19T10:00:0{s}.000Z");
    let lines = [
        entry(
            &format!("{prefix}1"),
            "user",
            &at(0),
            cwd,
            &json!({ "message": { "content": text("prompt") } }),
        ),
        entry(
            &format!("{prefix}2"),
            "assistant",
            &at(1),
            cwd,
            &json!({ "message": { "id": format!("{prefix}m1"), "content": [
            { "type": "tool_use", "id": format!("{prefix}t"), "name": "Bash", "input": { "command": text("input") } },
        ], "stop_reason": "tool_use" } }),
        ),
        entry(
            &format!("{prefix}3"),
            "user",
            &at(2),
            cwd,
            &json!({ "message": { "content": [
            { "type": "tool_result", "tool_use_id": format!("{prefix}t"), "content": text("result") },
        ] } }),
        ),
        entry(
            &format!("{prefix}4"),
            "assistant",
            &at(3),
            cwd,
            &json!({ "message": { "id": format!("{prefix}m2"), "content": [
            { "type": "text", "text": text("final") },
        ], "stop_reason": "end_turn" } }),
        ),
        entry(
            &format!("{prefix}5"),
            "user",
            &at(4),
            cwd,
            &json!({ "isCompactSummary": true, "message": { "content": text("summary") } }),
        ),
    ];
    let mut body = lines.join("\n");
    body.push('\n');
    body.into_bytes()
}

/// An uploaded transcript is imported as if captured live: every event in the log, in order,
/// with its outcome, tool I/O cut to 512 bytes, and the final messages and the summary
/// recalled in their project, never in another.
#[tokio::test]
async fn a_transcript_imports_as_if_captured_live() {
    let server = TestServer::start_with_denylist(&[]).await;

    let (_, write) = dry_run_then_write(&server, &Upload::transcript()).await;

    let events = imported_events(&server);
    let seen: Vec<String> = events
        .iter()
        .map(|p| {
            let outcome = p["outcome"]
                .as_str()
                .map(|o| format!(" {o}"))
                .unwrap_or_default();
            let tool = p["tool"]
                .as_str()
                .map(|t| format!(" {t}"))
                .unwrap_or_default();
            format!("{}{tool}{outcome}", p["kind"].as_str().unwrap())
        })
        .collect();
    assert_eq!(
        seen,
        [
            "prompt",
            "tool_use Bash success",
            "tool_use Read failure",
            "tool_use Read success",
            "tool_use mcp__me__recall success",
            "stop completed",
            "prompt",
            "tool_use Bash interrupted",
            "prompt",
            "tool_use Grep success",
            "prompt",
            "stop api_error",
            "prompt",
            "stop completed",
            "compact",
            "prompt",
            "stop completed",
            "prompt",
            "tool_use Task success",
            "stop completed",
            "prompt",
        ]
    );
    assert_eq!(write["events"]["stored"], 21, "{write}");
    assert_eq!(write["events"]["duplicate"], 1, "the forked entry");
    assert_eq!(write["events"]["pending"], 1);
    assert_eq!(write["census"]["incomplete_turn"], 4);
    for payload in events.iter().filter(|p| p["kind"] == "tool_use") {
        assert!(payload["input"].to_string().len() <= 512 + 2, "{payload}");
        assert!(payload["result"].as_str().is_none_or(|r| r.len() <= 512));
        assert_eq!(
            payload["source"],
            Value::Null,
            "the payload is live capture's"
        );
        assert_eq!(payload["machine"], "laptop");
    }
    // Once embedded (the mock gives every text one vector), recall finds every memory of the
    // project, and none from another project or a keyless session.
    server.wait_drained().await;
    let mut kinds: Vec<String> = recall(&server, &project_session(), "flag")
        .await
        .into_iter()
        .map(|(_, _, kind)| kind)
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        [
            "compaction summary",
            "final message",
            "final message",
            "final message",
            "final message"
        ]
    );
    assert_eq!(recall(&server, &other_project(), "flag").await.len(), 0);
    assert!(
        recall(&server, &[], "flag").await.is_empty(),
        "a keyless session"
    );
}

/// Facts are filed under the key the upload's project facts derive: the remote's, else the path
/// key `<machine>:<path>` below the dev root, else the machine's bucket.
#[tokio::test]
async fn facts_are_filed_under_the_uploads_key() {
    let server = TestServer::start_with_denylist(&[]).await;
    let memory = || Upload::memory("feedback_tests_first.md", "2026-03-20T08:00:00Z");
    let cases = [
        (memory(), "user:alice/project:example.com%2Fexample%2Fp"),
        (
            memory().without("ME-Project-Origin-Url"),
            "user:alice/project:laptop%3Ap",
        ),
        (
            memory()
                .without("ME-Project-Origin-Url")
                .without("ME-Project-Root")
                .without("ME-Project-Dev-Root"),
            "unscoped:laptop",
        ),
    ];
    for (upload, home) in cases {
        let (_, write) = dry_run_then_write(&server, &upload).await;
        assert_eq!(write["project"]["home"], home, "{write}");
        assert_eq!(write["facts"]["items"][0]["home"], home);
        let scope = server.engine.resolve_scope_path(home).unwrap();
        let filed = server.number(&format!(
            "SELECT COUNT(*) FROM facts WHERE scope_id = {scope}"
        ));
        assert_eq!(filed, 1, "{home}");
    }
}

/// Redaction applies to every text an upload stores: placeholders stored and counted, and no
/// canary, nor the denylist literal, in the store's files or in the report.
#[tokio::test]
async fn redaction_applies_to_every_imported_text() {
    let server = TestServer::start_with_denylist(&["fixturehost"]).await;
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let ghp = common::ghp();
    let text = |what: &str| format!("{what} {aws} {ghp} on fixturehost");
    let body = one_turn("r", &format!("/work/fixturehost/{aws}"), &text);
    let transcript = Upload::transcript().body(body);
    let memory = Upload::memory("user_role.md", "2026-03-20T08:00:00Z").body(format!(
        "---\nname: x\nmetadata:\n  type: user\n---\n{}\n",
        text("memory")
    ));

    let mut reports = Vec::new();
    for upload in [transcript, memory] {
        let (dry, write) = dry_run_then_write(&server, &upload).await;
        reports.push(dry);
        reports.push(write);
    }

    let placeholders = &reports[1]["redaction"]["placeholders"];
    for rule in ["aws-access-key", "github-pat", "denylist"] {
        assert!(
            placeholders[rule].as_u64().unwrap() >= 5,
            "{rule}: {placeholders}"
        );
    }
    assert!(reports[1]["redaction"]["found"].as_u64().unwrap() >= 15);
    let events = imported_events(&server);
    assert_eq!(
        events[0]["prompt"],
        "prompt [REDACTED:aws-access-key] [REDACTED:github-pat] on [REDACTED:denylist]"
    );
    assert_eq!(
        events[0]["cwd"],
        "/work/[REDACTED:denylist]/[REDACTED:aws-access-key]"
    );
    for canary in [aws.as_str(), ghp.as_str(), "fixturehost"] {
        for report in &reports {
            assert!(
                !report.to_string().contains(canary),
                "the report holds {canary}"
            );
        }
        for (name, bytes) in store_bytes(&server) {
            assert!(!holds(&bytes, canary.as_bytes()), "{name} holds {canary}");
        }
    }
}

/// A call that touches a sensitive path is a stub: its tool, path, outcome and time, and
/// nothing of its input or result.
#[tokio::test]
async fn a_sensitive_path_call_is_a_stub() {
    let server = TestServer::start_with_denylist(&[]).await;

    let (dry, write) = dry_run_then_write(&server, &Upload::transcript()).await;

    let stub = imported_events(&server)
        .into_iter()
        .find(|p| p["input"]["sensitive_path"].is_string())
        .expect("a stub");
    assert_eq!(stub["tool"], "Read");
    assert_eq!(
        stub["input"],
        json!({ "sensitive_path": "/work/fixture/p/.env" })
    );
    assert_eq!(stub["result"], Value::Null);
    assert_eq!(stub["outcome"], "success");
    assert_eq!(
        dry["stubs"],
        json!([{ "tool": "Read", "path": "/work/fixture/p/.env", "time": "2026-03-19T10:00:05Z" }])
    );
    assert_eq!(write["redaction"]["stubbed"], 1);
    for (name, bytes) in store_bytes(&server) {
        assert!(
            !holds(&bytes, b"FIXTURE_MODE"),
            "{name} holds the file's content"
        );
    }
}

/// A denylist literal inside the stub's own key name rewrites neither the stored stub nor the
/// report's list of stubs: the key is the core's, only its value loses the literal.
#[tokio::test]
async fn a_stub_is_reported_whatever_the_denylist_holds() {
    let server = TestServer::start_with_denylist(&["path", "fixture"]).await;

    let (dry, write) = dry_run_then_write(&server, &Upload::transcript()).await;

    let path = format!("/work/[REDACTED:denylist]/p/{}", ".env");
    let want = json!([{ "tool": "Read", "path": path, "time": "2026-03-19T10:00:05Z" }]);
    assert_eq!(dry["stubs"], want, "{dry}");
    assert_eq!(write["stubs"], want);
    let stub = imported_events(&server)
        .into_iter()
        .find(|p| p["tool"] == "Read" && p["result"].is_null())
        .expect("the stub");
    assert_eq!(stub["input"], json!({ "sensitive_path": path }));
}

/// A fork's file repeats the original's assistant message under its own session: once the
/// original is imported, the fork's copy is a duplicate of it, never a collision.
#[tokio::test]
async fn a_forks_repeated_message_is_a_duplicate_of_the_original() {
    let server = TestServer::start_with_denylist(&[]).await;
    let at = |s: u32| format!("2026-03-19T10:00:0{s}.000Z");
    let message = |uuid: &str, s: u32, text: &str, stop: Value| {
        entry(
            uuid,
            "assistant",
            &at(s),
            "/work/fixture/p",
            &json!({ "message": {
            "id": "m1", "content": [{ "type": "text", "text": text }], "stop_reason": stop,
        } }),
        )
    };
    let original = [
        entry(
            "u1",
            "user",
            &at(0),
            "/work/fixture/p",
            &json!({ "message": { "content": "a prompt" } }),
        ),
        message("a1", 1, "part one", Value::Null),
        message("a2", 2, "part two", json!("end_turn")),
    ];
    let fork: Vec<String> = original
        .iter()
        .map(|line| {
            let mut value: Value = serde_json::from_str(line).unwrap();
            value["sessionId"] = json!("a-fork");
            value.to_string()
        })
        .collect();
    let file = |lines: &[String]| {
        let mut text = lines.join("\n");
        text.push('\n');
        Upload::transcript().body(text.into_bytes())
    };
    dry_run_then_write(&server, &file(&original)).await;

    let both: Vec<String> = original.iter().cloned().chain(fork).collect();
    let (dry, write) = dry_run_then_write(&server, &file(&both)).await;

    assert_eq!(dry["events"]["collision"], 0, "{dry}");
    assert_eq!(dry["events"]["stored"], 0);
    assert_eq!(
        write["events"]["duplicate"], 4,
        "the prompt and the stop, twice"
    );
    let stops: Vec<Value> = imported_events(&server)
        .into_iter()
        .filter(|p| p["kind"] == "stop")
        .collect();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0]["message"], "part one\npart two");
}

/// With the embedder down an upload stores and queues its memories: full-text recall finds them
/// at once, and the drain gives them their vectors once the embedder is up.
#[tokio::test]
async fn an_upload_with_the_embedder_down_stores_and_queues() {
    let mut server = TestServer::start_unreachable_with_denylist(&[]).await;

    let (_, write) = dry_run_then_write(&server, &Upload::transcript()).await;

    assert_eq!(write["facts"]["created"], 5, "{write}");
    assert_eq!(server.number("SELECT COUNT(*) FROM embed_queue"), 5);
    let found = recall(&server, &project_session(), "flag").await;
    assert_eq!(
        found.len(),
        3,
        "full-text recall, the embedder down: {found:?}"
    );
    server.bring_embedder_up().await;
    server.wait_drained().await;
    for (id, _, _) in found {
        assert!(server.has_vector(id).await, "fact {id}");
    }
}

/// The same file again, of either kind, adds no row and reinforces nothing.
#[tokio::test]
async fn re_uploading_the_same_file_adds_no_rows() {
    let server = TestServer::start_with_denylist(&[]).await;
    let uploads = [
        Upload::transcript(),
        Upload::memory("feedback_tests_first.md", "2026-03-20T08:00:00Z"),
    ];
    for upload in &uploads {
        dry_run_then_write(&server, upload).await;
    }
    server.wait_drained().await;
    let before = table_counts(&server);

    for upload in &uploads {
        let (dry, write) = dry_run_then_write(&server, upload).await;
        assert_eq!(dry["events"]["stored"], 0, "{dry}");
        assert_eq!(write["facts"]["items"], json!([]));
    }

    assert_eq!(table_counts(&server), before);
}

/// A forgotten fact stays absent: neither the same file again nor another file holding the same
/// text brings it back.
#[tokio::test]
async fn a_forgotten_fact_stays_absent_after_its_source_is_re_uploaded() {
    let server = TestServer::start_with_denylist(&[]).await;
    dry_run_then_write(&server, &Upload::transcript()).await;
    let text = "A terminal showing the new --verbose flag.";
    let (id, _, _) = recall(&server, &project_session(), "terminal")
        .await
        .into_iter()
        .find(|(_, content, _)| content == text)
        .expect("the final message is recalled");
    let facts = project_session();
    let borrowed: Vec<(&str, &[u8])> = facts.iter().map(|(n, v)| (*n, v.as_slice())).collect();
    let reply = server
        .tool_with(
            "laptop",
            &borrowed,
            "forget",
            json!({ "id": id, "reason": "wrong" }),
        )
        .await;
    assert!(!reply.is_error, "{reply:?}");

    let (again, _) = dry_run_then_write(&server, &Upload::transcript()).await;
    assert_eq!(
        again["facts"]["items"],
        json!([]),
        "the same file: all duplicates"
    );
    let copy = one_turn("f", "/work/fixture/p", &|what| {
        if what == "final" {
            text.to_owned()
        } else {
            format!("a {what}")
        }
    });
    let (other, _) = dry_run_then_write(&server, &Upload::transcript().body(copy)).await;
    let outcomes: Vec<(&str, &str)> = other["facts"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["capture"].as_str().unwrap(),
                f["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("final_message", "blocked"),
            ("compaction_summary", "created")
        ]
    );
    assert!(
        recall(&server, &project_session(), "terminal")
            .await
            .iter()
            .all(|(_, content, _)| content != text)
    );
}

/// A copy of a session that grew since the last upload adds only its new entries.
#[tokio::test]
async fn a_grown_transcript_adds_only_its_new_entries() {
    let server = TestServer::start_with_denylist(&[]).await;
    let file = transcript();
    let text = std::str::from_utf8(&file).unwrap();
    let mut head: String = text.lines().take(30).collect::<Vec<_>>().join("\n");
    head.push('\n');

    let (_, first) = dry_run_then_write(&server, &Upload::transcript().body(head)).await;
    let (grown, _) = dry_run_then_write(&server, &Upload::transcript()).await;

    let stored = first["events"]["stored"].as_u64().unwrap();
    assert_eq!(
        grown["events"]["duplicate"].as_u64().unwrap(),
        stored + 1,
        "and the fork"
    );
    assert_eq!(
        stored + grown["events"]["stored"].as_u64().unwrap(),
        21,
        "every event once"
    );
    assert_eq!(imported_events(&server).len(), 21);
}

/// A session the plugin captured live imports only what came before its first live event.
#[tokio::test]
async fn a_session_captured_live_imports_only_what_came_before() {
    let server = TestServer::start_with_denylist(&[]).await;
    for (name, time) in [
        ("user-prompt-submit", "2026-03-19T10:51:00Z"),
        ("stop", "2026-03-19T10:51:03Z"),
    ] {
        let live = fixture(name)
            .header("ME-Event-Time", time)
            .header("ME-Event-Seq", "000001")
            .json(|b| b["session_id"] = json!(SESSION));
        assert!(
            server
                .post_event("laptop", &live)
                .await
                .is(204, Some("stored"))
        );
    }

    let (dry, _) = dry_run_then_write(&server, &Upload::transcript()).await;

    let session = dry["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == SESSION)
        .unwrap()
        .clone();
    assert_eq!(session["live_since"], "2026-03-19T10:51:00Z");
    assert_eq!(dry["events"]["live_overlap"], 8, "{dry}");
    let times: Vec<String> = imported_events(&server)
        .iter()
        .map(|p| p["time"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(times.len(), 13);
    assert!(
        times.iter().all(|t| t.as_str() <= "2026-03-19T10:51:00Z"),
        "{times:?}"
    );
}

/// A parent and its subagent file store the same events, order keys and memories whichever is
/// uploaded first.
#[tokio::test]
async fn a_parent_and_its_subagent_file_store_the_same_in_either_order() {
    let parent = Upload::transcript();
    let child = Upload::transcript().header(
        "ME-Backfill-Source",
        &format!("projects/-work-fixture-p/{SESSION}/subagents/agent-a3c91f2.jsonl"),
    );
    let child = child.body(subagent());
    let mut stored = Vec::new();
    for order in [[&parent, &child], [&child, &parent]] {
        let server = TestServer::start_with_denylist(&[]).await;
        for upload in order {
            dry_run_then_write(&server, upload).await;
        }
        let mut events: Vec<(String, String, u64, String, String)> = imported_events(&server)
            .iter()
            .map(|p| {
                (
                    p["event_id"].as_str().unwrap().to_owned(),
                    p["time"].as_str().unwrap().to_owned(),
                    p["seq"].as_u64().unwrap(),
                    p["kind"].as_str().unwrap().to_owned(),
                    p["actor"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        events.sort();
        let conn = rusqlite::Connection::open(server.db()).unwrap();
        let mut facts: Vec<(String, String)> = conn
            .prepare("SELECT content, t_created FROM facts")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        facts.sort();
        stored.push((events, facts));
    }
    assert_eq!(stored[0].0.len(), 21 + 3);
    assert_eq!(stored[0], stored[1]);
}

/// A native auto-memory file in the `my-dotfiles#134` format imports correctly: its type, its
/// Home, its metadata, its date by priority, found by `recall`; the index is skipped and a
/// `user` memory carries the `set_level` hint.
#[tokio::test]
async fn a_native_auto_memory_file_imports_correctly() {
    let server = TestServer::start_with_denylist(&[]).await;
    let mtime = "2026-03-20T08:00:00Z";
    let home = "user:alice/project:example.com%2Fexample%2Fp";
    let cases = [
        (
            "user_role.md",
            "Semantic",
            mtime,
            "created",
            Some("set_level"),
        ),
        (
            "feedback_tests_first.md",
            "Procedural",
            mtime,
            "created",
            None,
        ),
        (
            "feedback_red_before_green.md",
            "Procedural",
            mtime,
            "reinforced",
            None,
        ),
        (
            "project_release_2026_03_14.md",
            "Episodic",
            "2026-03-14T00:00:00Z",
            "created",
            None,
        ),
        (
            "reference_fixture_docs.md",
            "Semantic",
            "2026-02-01T09:00:00Z",
            "created",
            None,
        ),
    ];
    for (name, fact_type, date, outcome, hint) in cases {
        let (_, write) = dry_run_then_write(&server, &Upload::memory(name, mtime)).await;
        let item = &write["facts"]["items"][0];
        assert_eq!(
            item["fact_type"],
            fact_type.to_lowercase(),
            "{name}: {write}"
        );
        assert_eq!(item["t_created"], date, "{name}");
        assert_eq!(item["outcome"], outcome, "{name}");
        assert_eq!(item["hint"], json!(hint), "{name}");
        assert_eq!(item["home"], home, "{name}");
    }
    server.wait_drained().await;
    for name in ["MEMORY.md", "project_empty_stub.md"] {
        let before = table_counts(&server);
        let (status, report) = send(&server, "laptop", &Upload::memory(name, mtime)).await;
        assert_eq!(status, 200, "{report}");
        let reason = if name == "MEMORY.md" {
            "index"
        } else {
            "empty"
        };
        assert_eq!(report["census"]["skipped"][reason], 1, "{report}");
        assert_eq!(table_counts(&server), before);
    }
    let conn = rusqlite::Connection::open(server.db()).unwrap();
    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM facts WHERE content LIKE 'The fixture CLI documents%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let metadata: Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(
        metadata,
        json!({
            "source": "memory-dir", "file": "reference_fixture_docs.md", "name": "fixture-docs",
            "description": "Where the fixture docs live", "memory_type": "reference",
            "route": "knowledge-base", "machine": "laptop",
        })
    );
    let found = recall(&server, &project_session(), "documents").await;
    assert!(
        found.iter().any(|(_, content, kind)| content
            == "The fixture CLI documents its flags in docs/flags.md."
            && kind == "remembered"),
        "{found:?}"
    );
    assert_eq!(
        recall(&server, &other_project(), "documents").await.len(),
        0
    );
}

/// The operator's `erase --machine`, with the server stopped, takes every row a machine's
/// uploads wrote, even when the machine's name is in the denylist; another machine's stay.
#[test]
fn erasing_a_machine_takes_its_imported_rows() {
    let dir = tempfile::tempdir().unwrap();
    let denylist = dir.path().join("denylist");
    std::fs::write(&denylist, "laptop\n").unwrap();
    let mut server = common::ServerProcess::spawn(
        dir.path(),
        &[],
        &[(DENYLIST_VARIABLE, denylist.to_str().unwrap())],
    );
    server.wait_ready().unwrap();
    let client = reqwest::blocking::Client::new();
    let post = |machine: &str, upload: &Upload| -> Value {
        let mut answers = Vec::new();
        let mut upload = upload.clone();
        for _ in 0..2 {
            let mut request = client
                .post(format!("{}/backfill/uploads", server.url()))
                .bearer_auth(token(machine))
                .body(upload.body.clone());
            for (name, value) in &upload.headers {
                request = request.header(name, value);
            }
            let response = request.send().unwrap();
            assert_eq!(response.status(), 200);
            let report: Value = response.json().unwrap();
            upload = upload.write(report["preview"].as_str().unwrap());
            answers.push(report);
        }
        answers.pop().unwrap()
    };
    // Markers that hold no denylist literal: redaction leaves them whole.
    let (laptop, desktop) = ("zqxjvkpwqqalpha", "zqxjvkpwqqbravo");
    for (machine, marker) in [("laptop", laptop), ("desktop", desktop)] {
        let body = one_turn(&machine[..1], "/work/p", &|what| format!("{what} {marker}"));
        let write = post(machine, &Upload::transcript().body(body));
        assert_eq!(write["committed"], true);
        let memory = Upload::memory("user_role.md", "2026-03-20T08:00:00Z").body(format!(
            "---\nname: x\nmetadata:\n  type: user\n---\n{marker} memory\n"
        ));
        assert_eq!(post(machine, &memory)["committed"], true);
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
    for (table, machine) in [
        ("events", "json_extract(payload, '$.machine')"),
        ("facts", "json_extract(metadata, '$.machine')"),
        ("lifecycle_receipts", "machine"),
        ("lifecycle_segments", "machine"),
        ("activities", "machine"),
    ] {
        let by = |name: &str| {
            count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE {machine} = '{name}'"
            ))
        };
        assert_eq!(by("laptop"), 0, "{table}");
        assert!(by("desktop") > 0, "{table}");
    }
    assert_eq!(
        count("SELECT COUNT(*) FROM embed_queue"),
        3,
        "desktop's final message, summary and memory wait for their vectors"
    );
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("memory.db") {
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(!holds(&bytes, laptop.as_bytes()), "{:?}", entry.file_name());
        }
    }
}

/// A dry run writes nothing, and its report is the write's but for its mode and commit.
#[tokio::test]
async fn a_dry_run_writes_nothing_and_matches_the_write() {
    let server = TestServer::start_with_denylist(&[]).await;
    let before = table_counts(&server);

    let (status, dry) = send(&server, "laptop", &Upload::transcript()).await;

    assert_eq!(status, 200);
    assert_eq!(table_counts(&server), before, "nothing written");
    let preview = dry["preview"].as_str().unwrap();
    let (status, mut write) = send(&server, "laptop", &Upload::transcript().write(preview)).await;
    assert_eq!(status, 200);
    assert_ne!(table_counts(&server), before);
    assert_eq!(
        (&write["mode"], &write["committed"]),
        (&json!("write"), &json!(true))
    );
    write["mode"] = dry["mode"].clone();
    write["committed"] = dry["committed"].clone();
    assert_eq!(write, dry);
}

/// The cost of a large upload (#1026): a synthetic 50,000-event transcript, as a dry run and as
/// its write, on a store in `ME_LATENCY_DIR`, which must be a directory on disk (not tmpfs).
#[tokio::test]
#[ignore = "a timing, run by hand: ME_LATENCY_DIR=<dir on disk> cargo test --test backfill_upload backfill_upload_latency -- --ignored --nocapture"]
async fn backfill_upload_latency() {
    let parent = PathBuf::from(std::env::var("ME_LATENCY_DIR").expect("ME_LATENCY_DIR"));
    let server = TestServer::start_with_denylist_in(&["fixturehost"], &parent).await;
    let mut lines = Vec::new();
    let mut ms: i64 = 0;
    let mut at = || {
        ms += 7;
        (chrono::DateTime::from_timestamp_millis(1_773_914_400_000 + ms).unwrap())
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    // 10,000 turns of five events: a prompt, three tool calls and a final message.
    for turn in 0..10_000 {
        let cwd = "/work/fixture/p";
        lines.push(entry(
            &format!("p{turn}"),
            "user",
            &at(),
            cwd,
            &json!({ "message": { "content": format!("turn {turn}: change the parser") } }),
        ));
        for call in 0..3 {
            let id = format!("t{turn}-{call}");
            lines.push(entry(&format!("a{turn}-{call}"), "assistant", &at(), cwd, &json!({ "message": { "id": format!("m{turn}-{call}"), "content": [
                { "type": "tool_use", "id": id, "name": "Read", "input": { "file_path": format!("src/file{call}.rs") } },
            ], "stop_reason": "tool_use" } })));
            lines.push(entry(&format!("r{turn}-{call}"), "user", &at(), cwd, &json!({ "message": { "content": [
                { "type": "tool_result", "tool_use_id": id, "content": "fn parse() {}\n".repeat(20) },
            ] } })));
        }
        lines.push(entry(
            &format!("s{turn}"),
            "assistant",
            &at(),
            cwd,
            &json!({ "message": { "id": format!("f{turn}"), "content": [
            { "type": "text", "text": format!("Turn {turn} changed the parser.") },
        ], "stop_reason": "end_turn" } }),
        ));
    }
    let upload = Upload::transcript().body(lines.join("\n").into_bytes());

    let started = std::time::Instant::now();
    let (status, dry) = send(&server, "laptop", &upload).await;
    let dry_run = started.elapsed();
    assert_eq!(status, 200);
    assert_eq!(dry["events"]["stored"], 50_000);
    let preview = dry["preview"].as_str().unwrap().to_owned();
    let started = std::time::Instant::now();
    let (status, write) = send(&server, "laptop", &upload.clone().write(&preview)).await;
    let committed = started.elapsed();
    assert_eq!((status, &write["committed"]), (200, &json!(true)));
    println!(
        "backfill_upload_latency: {} bytes, 50,000 events: dry run {dry_run:?}, write {committed:?}",
        upload.body.len()
    );
}
