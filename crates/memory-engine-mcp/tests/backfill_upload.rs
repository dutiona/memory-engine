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
    let kind = ok.clone().header("ME-Backfill-Kind", "claude-mem");
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
    for stale in [changed_body, other_fact] {
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
