//! The claude-mem and claude-memory plugin stores (#1117), driven with the checked-in synthetic
//! stores of `tests/fixtures/plugin-stores/`.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use memory_engine::types::FactType;
use memory_engine_mcp::adapters::Slice;
use memory_engine_mcp::adapters::claude_code::{claude_mem, claude_memory};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::{Value, json};

mod common;

use common::{TestServer, fixture, holds, store_bytes, token};

const STORES: [&str; 2] = ["claude-mem.sql", "claude-memory.sql"];
const LOCAL: [&str; 4] = ["/home/", "/Users/", "/tmp/", "/var/folders/"];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-stores")
}

/// A fixture store, built in memory from its checked-in SQL.
fn store(name: &str) -> Connection {
    let sql = std::fs::read_to_string(fixtures_dir().join(name)).unwrap();
    let store = Connection::open_in_memory().unwrap();
    store.execute_batch(&sql).unwrap();
    store
}

/// Every TEXT value of every table of `store`.
fn texts(store: &Connection) -> Vec<String> {
    let tables: Vec<String> = store
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut texts = Vec::new();
    for table in tables {
        let mut stmt = store
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .unwrap();
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..columns {
                if let ValueRef::Text(t) = row.get_ref(i).unwrap() {
                    texts.push(String::from_utf8(t.to_vec()).unwrap());
                }
            }
        }
    }
    texts
}

fn assert_public_safe(what: &str, text: &str) {
    for local in LOCAL {
        assert!(!text.contains(local), "{what} holds {local}");
    }
    assert_eq!(
        memory_engine::redact::scrub(text).unwrap(),
        text,
        "{what}: a redaction rule matches a fixture string"
    );
}

/// The fixture stores and their README hold no local path and no string a redaction rule would
/// change: every secret and canary is planted at run time.
#[test]
fn plugin_store_fixtures_are_public_safe() {
    for name in STORES {
        let texts = texts(&store(name));
        assert!(!texts.is_empty(), "{name} holds no row");
        for text in &texts {
            assert_public_safe(name, text);
        }
    }
    let readme = std::fs::read_to_string(fixtures_dir().join("README.md")).unwrap();
    assert_public_safe("README.md", &readme);
}

// ── The exporter ────────────────────────────────────────────────────────────────

fn sql(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name)).unwrap()
}

/// A fixture store written to `path`.
fn file_store(path: &Path, name: &str) -> Connection {
    let store = Connection::open(path).unwrap();
    store.execute_batch(&sql(name)).unwrap();
    store
}

fn export(kind: &str, store: &Path, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memory-engine-plugin-export"))
        .args([kind.as_ref(), store.as_os_str(), out.as_os_str()])
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every file of `dir`, by name, with its bytes.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// One file per project, named by its project's digest, private, listed in `index.tsv`, its
/// body the store module's export; a second run is byte-identical.
#[test]
fn the_exporter_writes_one_file_per_project() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conversations.db");
    let store = file_store(&path, "claude-memory.sql");
    let out = dir.path().join("out");

    let run = export("claude-memory", &path, &out);

    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3 files, 23 rows\n");
    assert_eq!(mode(&out), 0o700);
    let slices = claude_memory::export(&store).unwrap();
    let name = |project: Option<&str>| {
        use sha2::{Digest, Sha256};
        let tag = project.map_or_else(
            || "none".to_owned(),
            |p| {
                Sha256::digest(p.as_bytes())[..6]
                    .iter()
                    .fold(String::new(), |mut hex, b| {
                        let _ = write!(hex, "{b:02x}");
                        hex
                    })
            },
        );
        format!("claude-memory-{tag}.jsonl")
    };
    let mut index = String::from("file\tproject\tsessions\trows\tbytes\n");
    let mut want: Vec<(String, Vec<u8>)> = Vec::new();
    for slice in &slices {
        let file = name(slice.project.as_deref());
        assert_eq!(mode(&out.join(&file)), 0o600, "{file}");
        let _ = writeln!(
            index,
            "{file}\t{}\t{}\t{}\t{}",
            slice.project.as_deref().unwrap_or_default(),
            slice.sessions,
            slice.rows,
            slice.body.len()
        );
        want.push((file, slice.body.clone()));
    }
    want.push(("index.tsv".to_owned(), index.into_bytes()));
    want.sort();
    let written = files(&out);
    assert_eq!(written, want);
    assert_eq!(mode(&out.join("index.tsv")), 0o600);

    let again = dir.path().join("again");
    assert!(export("claude-memory", &path, &again).status.success());
    assert_eq!(files(&again), written);
}

/// A WAL store's committed frames are read from its `-wal`, and neither file of the copy
/// changes; a copy whose header says WAL without a `-wal` beside it is refused, and with an
/// empty one it is the checkpointed store.
#[test]
fn the_exporter_reads_the_wal_and_refuses_a_copy_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let path = source.join("claude-mem.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    writer.execute_batch(&sql("claude-mem.sql")).unwrap();
    writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    writer
        .execute_batch(
            "INSERT INTO user_prompts (content_session_id, prompt_number, prompt_text, created_at,
               created_at_epoch) VALUES ('eef5665a-2f30-497d-87e7-ef64be1076a7', 3,
               'Only in the WAL.', '2026-03-25T11:00:00.000Z', 1774436400000);",
        )
        .unwrap();
    let copy = dir.path().join("copy");
    std::fs::create_dir(&copy).unwrap();
    for name in ["claude-mem.db", "claude-mem.db-wal"] {
        std::fs::copy(source.join(name), copy.join(name)).unwrap();
    }
    let copied = copy.join("claude-mem.db");
    let before = files(&copy);
    let in_source = files(&source);

    let out = dir.path().join("out");
    let run = export("claude-mem", &copied, &out);

    assert!(run.status.success(), "{}", stderr(&run));
    let bodies: String = files(&out)
        .into_iter()
        .filter(|(name, _)| name != "index.tsv")
        .map(|(_, body)| String::from_utf8(body).unwrap())
        .collect();
    assert!(bodies.contains("Only in the WAL."));
    for (name, bytes) in &before {
        assert_eq!(
            &std::fs::read(copy.join(name)).unwrap(),
            bytes,
            "{name} changed"
        );
    }
    assert_eq!(files(&source), in_source, "the source is untouched");
    assert!(files(&out).iter().all(|(name, _)| name == "index.tsv"
        || (name.starts_with("claude-mem-")
            && Path::new(name).extension().is_some_and(|e| e == "jsonl"))));
    drop(writer);

    std::fs::remove_file(copy.join("claude-mem.db-wal")).unwrap();
    let _ = std::fs::remove_file(copy.join("claude-mem.db-shm"));
    let refused = dir.path().join("refused");
    let run = export("claude-mem", &copied, &refused);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("-wal"), "{}", stderr(&run));
    assert!(!refused.exists(), "a refusal writes nothing");

    std::fs::write(copy.join("claude-mem.db-wal"), b"").unwrap();
    let checkpointed = dir.path().join("checkpointed");
    let run = export("claude-mem", &copied, &checkpointed);
    assert!(run.status.success(), "{}", stderr(&run));
    let all: Vec<u8> = files(&checkpointed)
        .into_iter()
        .flat_map(|(_, b)| b)
        .collect();
    assert!(!String::from_utf8(all).unwrap().contains("Only in the WAL."));
    let want =
        claude_mem::export(&file_store(&dir.path().join("plain.db"), "claude-mem.sql")).unwrap();
    assert_eq!(files(&checkpointed).len(), want.len() + 1);
}

/// An output directory that exists is refused, and so is a store missing a column the export
/// reads, by its name; neither writes anything.
#[test]
fn the_exporter_refuses_a_used_directory_and_names_a_missing_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conversations.db");
    drop(file_store(&path, "claude-memory.sql"));
    let used = dir.path().join("used");
    std::fs::create_dir(&used).unwrap();
    let run = export("claude-memory", &path, &used);
    assert_eq!(run.status.code(), Some(1));
    assert_eq!(files(&used), Vec::new());

    let old = dir.path().join("old.db");
    let store = file_store(&old, "claude-memory.sql");
    store
        .execute_batch("ALTER TABLE messages DROP COLUMN is_notification;")
        .unwrap();
    drop(store);
    let out = dir.path().join("out");
    let run = export("claude-memory", &old, &out);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("is_notification"), "{}", stderr(&run));
    assert!(!out.exists());
}

// ── The route (#1117) ───────────────────────────────────────────────────────────

const A: &str = "eef5665a-2f30-497d-87e7-ef64be1076a7";
const B: &str = "b14d1330-d7c4-4979-9fd6-1cbfad0113eb";
const C: &str = "6abf0af7-d5d4-4183-97b4-14142808dffb";

/// A fixture store in memory, `edit` run on it first.
fn edited(name: &str, edit: &str) -> Connection {
    let store = Connection::open_in_memory().unwrap();
    store.execute_batch(&sql(name)).unwrap();
    store.execute_batch(edit).unwrap();
    store
}

/// The slices of the fixture store of `kind`, `edit` run on it first.
fn slices_of(kind: &str, edit: &str) -> Vec<Slice> {
    match kind {
        "claude-mem" => claude_mem::export(&edited("claude-mem.sql", edit)).unwrap(),
        _ => claude_memory::export(&edited("claude-memory.sql", edit)).unwrap(),
    }
}

/// One upload's headers, a dry run, and its body.
#[derive(Clone)]
struct Upload {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Upload {
    /// A dry run of one store slice, with its project as the project root.
    fn slice(kind: &str, slice: &Slice) -> Self {
        let mut headers = vec![
            ("ME-Harness", "claude-code".to_owned()),
            ("ME-Backfill-Kind", kind.to_owned()),
            ("ME-Backfill-Mode", "dry-run".to_owned()),
            (
                "ME-Backfill-Source",
                format!("plugin-stores/{kind}/{kind}.jsonl"),
            ),
            ("ME-Client-Version", "0.1.0".to_owned()),
        ];
        if let Some(project) = &slice.project {
            let root = if project.starts_with('/') {
                project.clone()
            } else {
                format!("/work/fixture/{project}")
            };
            headers.push(("ME-Project-Root", root));
            headers.push(("ME-Project-Dev-Root", "/work/fixture".to_owned()));
        }
        Self {
            headers: headers
                .into_iter()
                .map(|(n, v)| (n.to_owned(), v))
                .collect(),
            body: slice.body.clone(),
        }
    }

    /// A dry run of a transcript holding `lines`.
    fn transcript(lines: &[Value]) -> Self {
        let body = lines.iter().fold(String::new(), |mut body, line| {
            let _ = writeln!(body, "{line}");
            body
        });
        Self {
            headers: [
                ("ME-Harness", "claude-code"),
                ("ME-Backfill-Kind", "transcript"),
                ("ME-Backfill-Mode", "dry-run"),
                ("ME-Backfill-Source", "projects/-work-fixture-alpha/s.jsonl"),
                ("ME-Client-Version", "0.1.0"),
                ("ME-Project-Root", "/work/fixture/alpha"),
                ("ME-Project-Dev-Root", "/work/fixture"),
            ]
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect(),
            body: body.into_bytes(),
        }
    }

    fn write(mut self, preview: &str) -> Self {
        self.headers.retain(|(n, _)| n != "ME-Backfill-Mode");
        self.headers
            .push(("ME-Backfill-Mode".into(), "write".into()));
        self.headers
            .push(("ME-Backfill-Preview".into(), preview.into()));
        self
    }
}

/// A transcript's user entry of `session`.
fn transcript_prompt(session: &str, uuid: &str, time: &str, text: &str) -> Value {
    json!({
        "type": "user", "uuid": uuid, "sessionId": session, "timestamp": time,
        "cwd": "/work/fixture/alpha", "version": "2.1.310", "isSidechain": false,
        "message": { "role": "user", "content": text },
    })
}

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

async fn dry_run_then_write(server: &TestServer, upload: &Upload) -> (Value, Value) {
    let (status, dry) = send(server, "laptop", upload).await;
    assert_eq!(status, 200, "{dry}");
    let preview = dry["preview"].as_str().unwrap().to_owned();
    let (status, write) = send(server, "laptop", &upload.clone().write(&preview)).await;
    assert_eq!(status, 200, "{write}");
    assert_eq!(write["committed"], true, "{write}");
    (dry, write)
}

/// Import every slice of a store; the write reports, by slice.
async fn import(server: &TestServer, kind: &str, slices: &[Slice]) -> Vec<Value> {
    let mut reports = Vec::new();
    for slice in slices {
        reports.push(
            dry_run_then_write(server, &Upload::slice(kind, slice))
                .await
                .1,
        );
    }
    reports
}

/// The sum over `reports` of the count at `path`.
fn total(reports: &[Value], path: &[&str]) -> u64 {
    reports
        .iter()
        .map(|r| {
            path.iter()
                .fold(r, |v, p| &v[*p])
                .as_u64()
                .unwrap_or_default()
        })
        .sum()
}

/// The report's entry for `session`.
fn session<'a>(reports: &'a [Value], id: &str) -> &'a Value {
    reports
        .iter()
        .flat_map(|r| r["sessions"].as_array().unwrap())
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no session {id}"))
}

/// The `(source, kind, actor)` of every stored event of a store.
fn logged(server: &TestServer, source: &str) -> Vec<(String, String)> {
    let conn = Connection::open(server.db()).unwrap();
    let mut rows: Vec<(String, String)> = conn
        .prepare(
            "SELECT json_extract(payload, '$.kind'), json_extract(payload, '$.actor')
               FROM events WHERE source = ?1 AND session_id IS NOT NULL",
        )
        .unwrap()
        .query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows.sort();
    rows
}

async fn recall(server: &TestServer, root: &[u8], query: &str) -> Vec<String> {
    let reply = server
        .tool_with(
            "laptop",
            &[
                ("ME-Project-Root", root),
                ("ME-Project-Dev-Root", b"/work/fixture"),
            ],
            "recall",
            json!({ "query": query }),
        )
        .await;
    assert!(!reply.is_error, "{reply:?}");
    reply.json()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["content"].as_str().unwrap().to_owned())
        .collect()
}

/// Both stores import as if captured live: a dry run reports what its write does; prompts and
/// stops are logged under the store's kind; observations, summaries and main stops become
/// Episodic memories, an observation's importance by its concepts, none Procedural, found in
/// their project only; a subagent's stop stays in the log.
#[tokio::test]
#[expect(clippy::too_many_lines, reason = "both kinds, rule by rule")]
async fn a_plugin_store_imports_as_if_captured_live() {
    for kind in ["claude-mem", "claude-memory"] {
        let server = TestServer::start_unreachable_with_denylist(&[]).await;
        let mut writes = Vec::new();
        for slice in slices_of(kind, "") {
            let (mut dry, mut write) =
                dry_run_then_write(&server, &Upload::slice(kind, &slice)).await;
            for report in [&mut dry, &mut write] {
                report["mode"] = Value::Null;
                report["committed"] = Value::Null;
            }
            assert_eq!(dry, write, "{kind}");
            writes.push(write);
        }
        let facts = server.engine.list_active_facts(None).await.unwrap();
        assert!(
            facts.iter().all(|f| f.fact_type == FactType::Episodic),
            "{kind}"
        );
        let skipped = |reason: &str| total(&writes, &["census", "skipped", reason]);
        if kind == "claude-mem" {
            assert_eq!(
                logged(&server, kind),
                vec![("prompt".to_owned(), "main".to_owned()); 4]
            );
            let mut weights: Vec<(String, u64)> = facts
                .iter()
                .map(|f| {
                    let what = f.metadata["memory_type"].as_str().unwrap().to_owned();
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    (what, (f.base_importance * 10.0).round() as u64)
                })
                .collect();
            weights.sort();
            assert_eq!(
                weights,
                [
                    ("observation".to_owned(), 5),
                    ("observation".to_owned(), 5),
                    ("observation".to_owned(), 6),
                    ("observation".to_owned(), 6),
                    ("session_summary".to_owned(), 5),
                    ("session_summary".to_owned(), 5),
                ]
            );
            for (reason, n) in [
                ("empty prompt", 1),
                ("empty observation", 1),
                ("empty summary", 1),
                ("incomplete row", 1),
            ] {
                assert_eq!(skipped(reason), n, "{reason}");
            }
            let found = recall(&server, b"/work/fixture/alpha", "flag").await;
            assert!(
                found.iter().any(|f| f.starts_with("Retry flag parsing")),
                "{found:?}"
            );
            assert!(
                found
                    .iter()
                    .any(|f| f.starts_with("Request: Add a retry flag."))
            );
            let beta = recall(&server, b"/work/fixture/beta", "default").await;
            assert!(
                beta.iter().any(|f| f.starts_with("Keep the default"))
                    && beta
                        .iter()
                        .all(|f| !f.contains("retry flag") && !f.contains("Retry flag")),
                "{beta:?}"
            );
        } else {
            let mut want = vec![("prompt".to_owned(), "main".to_owned()); 6];
            want.extend(vec![("prompt".to_owned(), "subagent".to_owned()); 2]);
            want.extend(vec![("stop".to_owned(), "main".to_owned()); 6]);
            want.extend(vec![("stop".to_owned(), "subagent".to_owned()); 2]);
            want.sort();
            assert_eq!(logged(&server, kind), want);
            assert_eq!(facts.len(), 6, "one memory per main stop");
            assert!(facts.iter().all(|f| f.content != "Three callers found."));
            for (reason, n) in [
                ("narration", 2),
                ("notification", 1),
                ("interrupt", 1),
                ("interrupted turn", 1),
                ("empty message", 1),
                ("incomplete row", 1),
            ] {
                assert_eq!(skipped(reason), n, "{reason}");
            }
            let found = recall(&server, b"/work/fixture/alpha", "section").await;
            assert!(
                found.contains(&"Added a short section with an example.".to_owned()),
                "{found:?}"
            );
            assert_eq!(
                recall(&server, b"/work/fixture/beta", "section").await,
                Vec::<String>::new()
            );
        }
        let items = writes
            .iter()
            .flat_map(|w| w["facts"]["items"].as_array().unwrap().clone());
        for item in items {
            assert_eq!(item["fact_type"], "episodic", "{item}");
            assert!(
                item["importance"].is_number(),
                "the dry run shows each weight: {item}"
            );
        }
    }
}

/// A store's rows yield to live capture from a minute before a session's first live event, and
/// to a transcript or the other store holding a session's prompts, in either order of the
/// stores, while every observation and claude-memory's stops import; a live capture between a
/// dry run and its write is a 412; a transcript of a session holding store rows imports whole
/// and names the store.
#[tokio::test]
async fn a_store_row_yields_to_live_capture_and_transcripts() {
    let server = TestServer::start_unreachable_with_denylist(&[]).await;
    let live = |id: &str, time: &str| {
        fixture("user-prompt-submit")
            .header("ME-Event-Id", id)
            .header("ME-Event-Time", time)
            .json(|b| b["session_id"] = json!(B))
    };
    let transcript = Upload::transcript(&[transcript_prompt(
        C,
        "9c8b7a65-4321-4fed-8cba-987654321012",
        "2026-03-29T16:00:00.000Z",
        "What does the retry flag default to?",
    )]);
    dry_run_then_write(&server, &transcript).await;

    let mem = import(&server, "claude-mem", &slices_of("claude-mem", "")).await;
    assert_eq!(
        total(&mem, &["facts", "created"]),
        6,
        "every observation and summary"
    );

    let memory_slices = slices_of("claude-memory", "");
    let beta = memory_slices
        .iter()
        .find(|s| s.project.as_deref() == Some("/work/fixture/beta"))
        .unwrap();
    let (_, dry) = send(&server, "laptop", &Upload::slice("claude-memory", beta)).await;
    assert!(
        server
            .post_event(
                "laptop",
                &live("1806746490-000001-1", "2026-03-28T14:01:30Z")
            )
            .await
            .is(204, Some("stored"))
    );
    let preview = dry["preview"].as_str().unwrap();
    let stale = Upload::slice("claude-memory", beta).write(preview);
    assert_eq!(send(&server, "laptop", &stale).await.0, 412);

    let memory = import(&server, "claude-memory", &memory_slices).await;
    assert_eq!(
        total(&memory, &["events", "live_overlap"]),
        2,
        "B's last prompt and stop"
    );
    assert_eq!(session(&memory, B)["held_by"], json!(["lifecycle"]));
    assert_eq!(session(&memory, C)["held_by"], json!(["backfill"]));
    assert_eq!(session(&memory, C)["superseded"], 1);
    assert_eq!(session(&memory, A)["held_by"], json!(["claude-mem"]));
    assert_eq!(
        session(&memory, A)["superseded"],
        4,
        "claude-mem's prompts win"
    );
    assert_eq!(total(&memory, &["events", "superseded"]), 5);
    let stops = logged(&server, "claude-memory")
        .into_iter()
        .filter(|(kind, _)| kind == "stop")
        .count();
    assert_eq!(
        stops, 7,
        "every stop but B's last, which live capture holds"
    );

    let resumed = Upload::transcript(&[transcript_prompt(
        A,
        "8d7c6b5a-4f3e-4d2c-9b1a-0f9e8d7c6b5a",
        "2026-03-25T12:00:00.000Z",
        "Resume the retry work.",
    )]);
    let (dry, write) = dry_run_then_write(&server, &resumed).await;
    assert_eq!(
        write["events"]["stored"], 1,
        "a harness import never yields to a copy"
    );
    assert_eq!(
        session(std::slice::from_ref(&dry), A)["held_by"],
        json!(["claude-mem", "claude-memory"])
    );

    // The other order: claude-memory's prompts are kept, claude-mem's yield, and the stops
    // import all the same.
    let other = TestServer::start_unreachable_with_denylist(&[]).await;
    let memory = import(&other, "claude-memory", &memory_slices).await;
    assert_eq!(total(&memory, &["events", "superseded"]), 0);
    let mem = import(&other, "claude-mem", &slices_of("claude-mem", "")).await;
    assert_eq!(session(&mem, A)["superseded"], 2);
    assert_eq!(total(&mem, &["facts", "created"]), 6);
    let stops = logged(&other, "claude-memory")
        .into_iter()
        .filter(|(kind, _)| kind == "stop")
        .count();
    assert_eq!(stops, 8);
}

/// The same export again writes nothing and reinforces nothing; a grown export adds only its new
/// rows.
#[tokio::test]
async fn re_exporting_a_store_adds_no_rows() {
    let server = TestServer::start_unreachable_with_denylist(&[]).await;
    for kind in ["claude-mem", "claude-memory"] {
        import(&server, kind, &slices_of(kind, "")).await;
    }
    let counts = |server: &TestServer| {
        [
            "events",
            "facts",
            "lifecycle_receipts",
            "lifecycle_segments",
            "activities",
        ]
        .map(|t| server.number(&format!("SELECT COUNT(*) FROM {t}")))
    };
    let accesses = || server.number("SELECT COALESCE(SUM(access_count), 0) FROM facts");
    let (rows_before, access_before) = (counts(&server), accesses());
    for kind in ["claude-mem", "claude-memory"] {
        let again = import(&server, kind, &slices_of(kind, "")).await;
        assert_eq!(total(&again, &["events", "stored"]), 0, "{kind}");
        assert_eq!(
            total(&again, &["facts", "created"]) + total(&again, &["facts", "reinforced"]),
            0
        );
    }
    assert_eq!(counts(&server), rows_before);
    assert_eq!(accesses(), access_before);

    let grown = import(
        &server,
        "claude-memory",
        &slices_of(
            "claude-memory",
            "INSERT INTO messages (id, session_id, uuid, timestamp, role, content) VALUES
               (50, 2, '0f1e2d3c-4b5a-4697-8877-665544332211', '2026-03-28T15:00:00.000Z', 'user',
                'And the lints?'),
               (51, 2, '1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d', '2026-03-28T15:00:30.000Z',
                'assistant', 'They pass too.');",
        ),
    )
    .await;
    assert_eq!(
        total(&grown, &["events", "stored"]),
        2,
        "the new prompt and stop"
    );
    assert_eq!(total(&grown, &["events", "collision"]), 0);
}

/// Every store text is redacted with every rule and the denylist before it is stored or
/// reported: no canary reaches the store files or the report.
#[tokio::test]
async fn redaction_applies_to_every_store_text() {
    let server = TestServer::start_unreachable_with_denylist(&["fixturehost"]).await;
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let canary = format!("{aws} on fixturehost");
    let mem = format!(
        "UPDATE user_prompts SET prompt_text = prompt_text || ' {canary}';
         UPDATE observations SET title = title || ' {canary}', narrative = narrative || ' {canary}',
           facts = '[\"{canary}\"]', concepts = '[\"pattern\", \"{canary}\"]' WHERE id = 1;
         UPDATE session_summaries SET request = '{canary}', investigated = '{canary}',
           learned = '{canary}', completed = '{canary}', next_steps = '{canary}',
           notes = '{canary}' WHERE id = 1;"
    );
    let memory = format!(
        "UPDATE messages SET content = content || ' {canary}';
         UPDATE sessions SET cwd = '/work/fixturehost/{aws}';"
    );
    let mut reports = import(&server, "claude-mem", &slices_of("claude-mem", &mem)).await;
    reports.extend(
        import(
            &server,
            "claude-memory",
            &slices_of("claude-memory", &memory),
        )
        .await,
    );

    assert!(total(&reports, &["redaction", "found"]) > 0);
    assert!(total(&reports, &["redaction", "placeholders", "denylist"]) > 0);
    assert!(total(&reports, &["redaction", "placeholders", "aws-access-key"]) > 0);
    for report in &reports {
        let text = report.to_string();
        assert!(
            !text.contains(&aws) && !text.contains("fixturehost"),
            "{text}"
        );
    }
    for (name, bytes) in store_bytes(&server) {
        assert!(!holds(&bytes, aws.as_bytes()), "{name}");
        assert!(!holds(&bytes, b"fixturehost"), "{name}");
    }
}

/// The operator's `erase --machine`, with the server stopped, takes every row a machine's store
/// uploads wrote; another machine's stay.
#[test]
fn erasing_a_machine_takes_its_store_rows() {
    let dir = tempfile::tempdir().unwrap();
    let denylist = dir.path().join("denylist");
    std::fs::write(&denylist, "").unwrap();
    let mut server = common::ServerProcess::spawn(
        dir.path(),
        &[],
        &[("ME_REDACT_DENYLIST_FILE", denylist.to_str().unwrap())],
    );
    server.wait_ready().unwrap();
    let client = reqwest::blocking::Client::new();
    let post = |machine: &str, upload: &Upload| {
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
        }
    };
    let (laptop, desktop) = ("zqxjvkpwqqalpha", "zqxjvkpwqqbravo");
    for (machine, marker) in [("laptop", laptop), ("desktop", desktop)] {
        let mem = format!("UPDATE observations SET narrative = narrative || ' {marker}';");
        let memory = format!("UPDATE messages SET content = content || ' {marker}';");
        for (kind, edit) in [("claude-mem", mem), ("claude-memory", memory)] {
            for slice in slices_of(kind, &edit) {
                post(machine, &Upload::slice(kind, &slice));
            }
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
        Connection::open(&db)
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    };
    for (table, machine) in [
        ("events", "json_extract(payload, '$.machine')"),
        ("facts", "json_extract(metadata, '$.machine')"),
        ("lifecycle_receipts", "machine"),
        ("lifecycle_segments", "machine"),
    ] {
        let by = |name: &str| {
            count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE {machine} = '{name}'"
            ))
        };
        assert_eq!(by("laptop"), 0, "{table}");
        assert!(by("desktop") > 0, "{table}");
    }
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("memory.db") {
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(!holds(&bytes, laptop.as_bytes()), "{:?}", entry.file_name());
        }
    }
}
