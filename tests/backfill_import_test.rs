//! The engine's backfill import (#1026): `MemoryEngine::import_backfill` applies an upload's
//! neutral items under the capture contract in one transaction, or rolls it back. The readers
//! and the route have their own tests; this is what the engine decides.
#![allow(clippy::unwrap_used)] // test code: panic-on-unwrap is the intended failure signal (#725)

use std::path::PathBuf;

use chrono::{DateTime, Duration, TimeZone, Utc};
use memory_engine::MemoryEngine;
use memory_engine::inspect::ReplayFilter;
use memory_engine::read_redaction_counts;
use memory_engine::types::lifecycle::{
    Actor, BatchMode, Content, Delivery, FactPreview, ImportEvent, ImportItem, ImportMemory,
    ImportOutcome, Imported, MemoryItem, NeutralRecord, Recorded, StopOutcome, ToolOutcome, Upload,
};
use memory_engine::types::{Event, FactType, NewEvent};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const DIM: usize = 4;
const HOME: &str = "user:alice/project:p";
const SESSION: &str = "52c6c7b7-c890-43ec-81a5-a1cf99d3cc84";

fn at(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 19, 10, 0, 0).unwrap() + Duration::seconds(secs)
}

fn received() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 21, 12, 0, 0).unwrap()
}

fn provenance(root: &str) -> Value {
    json!({ "facts": { "root": root }, "dropped": {}, "key": null, "rules": 1 })
}

fn upload(home: &str) -> Upload {
    Upload {
        machine: "laptop".into(),
        harness: "claude-code".into(),
        client_version: "0.1.0".into(),
        kind: "transcript".into(),
        source: "projects/-work-p/s.jsonl".into(),
        received_at: received(),
        home: home.into(),
        provenance: provenance("/work/p"),
    }
}

fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

fn record(session: &str, actor: Actor, content: Content) -> NeutralRecord {
    NeutralRecord {
        session_id: session.into(),
        actor,
        cwd: Some("/work/p".into()),
        harness_version: Some("2.1.296".into()),
        content,
    }
}

/// An item `id` of `session` at `secs`, whose digest is that of its content.
fn item(id: &str, secs: i64, session: &str, actor: Actor, content: Content) -> ImportItem {
    ImportItem::Event(ImportEvent {
        event_id: id.into(),
        time: at(secs),
        seq: 0,
        body_sha256: digest(&format!("{content:?}{actor:?}")),
        body_size: 100,
        line: 1,
        record: record(session, actor, content),
    })
}

fn main_item(id: &str, secs: i64, content: Content) -> ImportItem {
    item(id, secs, SESSION, Actor::Main, content)
}

fn subagent() -> Actor {
    Actor::Subagent {
        id: Some("a3c91f2".into()),
        kind: None,
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

fn tool(input: Value, result: &str, paths: &[&str]) -> Content {
    Content::ToolUse {
        tool: "Bash".into(),
        input,
        result: Some(result.into()),
        outcome: ToolOutcome::Success,
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        call: None,
        dedup_window_secs: Some(300),
    }
}

fn memory(id: &str, text: &str, time: DateTime<Utc>) -> ImportItem {
    ImportItem::Memory(ImportMemory {
        event_id: id.into(),
        body_sha256: digest(text),
        body_size: 64,
        item: MemoryItem {
            text: text.into(),
            fact_type: FactType::Procedural,
            importance: 0.6,
            time,
            metadata: json!({
                "source": "memory-dir", "file": "feedback_x.md", "name": "x",
                "description": "a rule", "memory_type": "feedback",
            }),
        },
    })
}

struct Store {
    _dir: tempfile::TempDir,
    db: PathBuf,
    engine: MemoryEngine,
}

impl Store {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let engine = MemoryEngine::builder(DIM).path(&db).build().unwrap();
        Self {
            _dir: dir,
            db,
            engine,
        }
    }

    async fn import(
        &self,
        upload: &Upload,
        items: Vec<ImportItem>,
        denylist: &[String],
        mode: BatchMode,
    ) -> Imported {
        self.engine
            .import_backfill(upload, items, denylist, mode)
            .await
            .unwrap()
    }

    /// A dry run of `items`, then their write under its preview.
    async fn write(&self, upload: &Upload, items: &[ImportItem], denylist: &[String]) -> Imported {
        let dry = self
            .import(upload, items.to_vec(), denylist, BatchMode::DryRun)
            .await;
        let write = self
            .import(
                upload,
                items.to_vec(),
                denylist,
                BatchMode::Write {
                    preview: dry.preview,
                },
            )
            .await;
        assert!(write.committed, "{write:?}");
        write
    }

    async fn events(&self) -> Vec<Event> {
        let mut events = self
            .engine
            .replay_events(&ReplayFilter::default())
            .await
            .unwrap();
        events.retain(|e| e.source == "backfill");
        events
    }

    fn tables(&self) -> Vec<(String, Vec<String>)> {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        names
            .into_iter()
            .map(|name| {
                let mut stmt = conn.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
                let columns = stmt.column_count();
                let rows = stmt
                    .query_map([], |r| {
                        Ok((0..columns)
                            .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                            .collect::<Vec<_>>()
                            .join("|"))
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                (name, rows)
            })
            .collect()
    }
}

fn outcomes(imported: &Imported) -> Vec<ImportOutcome> {
    imported.items.iter().map(|(o, _)| *o).collect()
}

fn an_upload() -> Vec<ImportItem> {
    vec![
        main_item("bf.1.p", 0, prompt("Add a flag.")),
        main_item(
            "bf.2.t0",
            5,
            tool(
                json!({ "command": "ls src" }),
                "cli.rs\nlib.rs",
                &["ls", "src"],
            ),
        ),
        main_item("bf.3.s", 9, stop(StopOutcome::Completed, "Added the flag.")),
        main_item("bf.4.c", 20, compact("The flag was added.")),
    ]
}

/// A dry run reports what the write of the same upload does, and changes nothing: no row, no
/// redaction counter, nothing in the scope cache.
#[tokio::test]
async fn a_dry_run_reports_what_a_write_does() {
    let s = Store::new();
    let upload = upload(HOME);
    let before = s.tables();
    let counts = read_redaction_counts(&s.db).unwrap();

    let dry = s.import(&upload, an_upload(), &[], BatchMode::DryRun).await;

    assert!(!dry.committed);
    assert_eq!(s.tables(), before, "no row written");
    assert_eq!(read_redaction_counts(&s.db).unwrap(), counts);
    assert_eq!(s.engine.resolve_scope_path(HOME), None, "the scope cache");
    assert_eq!(outcomes(&dry), [ImportOutcome::Stored; 4]);
    assert_eq!(dry.facts.len(), 2);

    let write = s
        .import(
            &upload,
            an_upload(),
            &[],
            BatchMode::Write {
                preview: dry.preview,
            },
        )
        .await;

    assert!(write.committed);
    assert_eq!(write.preview, dry.preview);
    assert_eq!(write.items, dry.items);
    assert_eq!(write.facts, dry.facts);
    assert!(s.engine.resolve_scope_path(HOME).is_some());
    assert_eq!(s.events().await.len(), 4);
}

/// Only a main-agent completed final message with text and a main-agent compaction summary
/// become memories (ADR-0020 §6); prompts, tool calls, API errors and subagents stay in the log.
#[tokio::test]
async fn only_main_completed_stops_and_main_compacts_become_facts() {
    let s = Store::new();
    let items = vec![
        main_item("bf.1.p", 0, prompt("a prompt")),
        main_item("bf.2.t0", 1, tool(json!({}), "a result", &[])),
        main_item("bf.3.s", 2, stop(StopOutcome::Completed, "a final message")),
        main_item("bf.4.s", 3, stop(StopOutcome::ApiError, "API Error: down")),
        main_item("bf.5.s", 4, stop(StopOutcome::Completed, "")),
        item(
            "bf.6.s",
            5,
            SESSION,
            subagent(),
            stop(StopOutcome::Completed, "sub"),
        ),
        item(
            "bf.7.c",
            6,
            SESSION,
            subagent(),
            compact("a subagent summary"),
        ),
        main_item("bf.8.c", 7, compact("a summary")),
    ];

    let write = s.write(&upload(HOME), &items, &[]).await;

    let made: Vec<(&str, &str, &str)> = write
        .facts
        .iter()
        .map(|f| (f.event_id.as_str(), f.capture, f.outcome))
        .collect();
    assert_eq!(
        made,
        [
            ("bf.3.s", "final_message", "created"),
            ("bf.8.c", "compaction_summary", "created")
        ]
    );
    let mut facts: Vec<String> = s
        .engine
        .list_active_facts(None)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.content)
        .collect();
    facts.sort();
    assert_eq!(facts, ["a final message", "a summary"]);
    assert_eq!(s.events().await.len(), 8, "every event is in the log");
}

/// An imported memory is as old as its event (ADR-0009 D2): created, last accessed and valid
/// from the event's time, never from the upload's.
#[tokio::test]
async fn imported_facts_are_backdated_to_their_event() {
    let s = Store::new();
    let items = vec![
        main_item("bf.1.s", 30, stop(StopOutcome::Completed, "an old message")),
        memory("bf.m.1", "an old rule", at(-86_400)),
    ];

    let write = s.write(&upload(HOME), &items, &[]).await;

    assert_eq!(write.facts[0].t_created, at(-86_400));
    assert_eq!(write.facts[1].t_created, at(30));
    for fact in s.engine.list_active_facts(None).await.unwrap() {
        let time = if fact.content == "an old rule" {
            at(-86_400)
        } else {
            at(30)
        };
        assert_eq!(
            (fact.t_created, fact.last_accessed, fact.t_valid),
            (time, time, Some(time)),
            "{}",
            fact.content
        );
    }
    let events = s.events().await;
    assert!(
        events
            .iter()
            .all(|e| e.created_at == Some(received()) && e.timestamp < received())
    );
}

/// A session captured live is authoritative from its first live event: the imported events at
/// or after it are dropped, the earlier ones kept as late events.
#[tokio::test]
async fn events_from_a_sessions_live_start_on_are_dropped() {
    let s = Store::new();
    let live = Delivery {
        machine: "laptop".into(),
        event_id: "1795282823-000001-1".into(),
        time: at(100),
        seq: 1,
        harness: "claude-code".into(),
        client_version: "0.1.0".into(),
        body_sha256: [1; 32],
        body_size: 10,
        received_at: received(),
        home: HOME.into(),
        provenance: provenance("/work/p"),
    };
    let recorded = s
        .engine
        .record_lifecycle_event(&live, Ok(record(SESSION, Actor::Main, prompt("live"))))
        .await
        .unwrap();
    assert!(matches!(recorded, Recorded::Stored { .. }));
    let items = vec![
        main_item("bf.1.p", 50, prompt("before")),
        main_item("bf.2.p", 100, prompt("in the second, before by seq")),
        main_item("bf.3.s", 150, stop(StopOutcome::Completed, "after")),
        item(
            "bf.4.p",
            150,
            "another-session",
            Actor::Main,
            prompt("elsewhere"),
        ),
    ];

    let write = s.write(&upload(HOME), &items, &[]).await;

    assert_eq!(
        outcomes(&write),
        [
            ImportOutcome::Stored,
            ImportOutcome::Stored,
            ImportOutcome::LiveOverlap,
            ImportOutcome::Stored,
        ]
    );
    assert!(write.facts.is_empty(), "the overlap makes no memory");
    let since = &write.live_since[SESSION];
    assert_eq!((since.time, since.seq), (at(100), 1));
    assert_eq!(s.events().await.len(), 3);
}

/// The same upload again is all duplicates: no row, no reinforcement, no memory.
#[tokio::test]
async fn a_second_import_is_all_duplicates() {
    let s = Store::new();
    let mut items = an_upload();
    items.push(memory("bf.m.1", "a rule", at(-10)));
    s.write(&upload(HOME), &items, &[]).await;
    let before = s.tables();

    let again = s.write(&upload(HOME), &items, &[]).await;

    assert_eq!(outcomes(&again), [ImportOutcome::Duplicate; 5]);
    assert_eq!(again.facts, Vec::<FactPreview>::new());
    assert_eq!(s.tables(), before, "not even a reinforcement");
}

/// A forked or resumed session file repeats earlier entries under its own session ID: the same
/// key and digest meet them as duplicates, in one upload and across two.
#[tokio::test]
async fn a_repeated_entry_in_another_session_is_a_duplicate() {
    let s = Store::new();
    let first = main_item("bf.1.p", 0, prompt("Add a flag."));
    let forked = item(
        "bf.1.p",
        0,
        "a-forked-session",
        Actor::Main,
        prompt("Add a flag."),
    );

    let write = s
        .write(&upload(HOME), &[first.clone(), forked.clone()], &[])
        .await;
    assert_eq!(
        outcomes(&write),
        [ImportOutcome::Stored, ImportOutcome::Duplicate]
    );
    let again = s.write(&upload(HOME), &[forked], &[]).await;
    assert_eq!(outcomes(&again), [ImportOutcome::Duplicate]);
    assert_eq!(s.events().await.len(), 1);
}

/// A forgotten text stays absent when its source, or another file holding it, is uploaded again.
#[tokio::test]
async fn a_forgotten_text_is_blocked_on_reimport() {
    let s = Store::new();
    let items = vec![main_item(
        "bf.1.s",
        0,
        stop(StopOutcome::Completed, "forget me"),
    )];
    s.write(&upload(HOME), &items, &[]).await;
    let id = s.engine.list_active_facts(None).await.unwrap()[0].id;
    let record = NewEvent {
        timestamp: Utc::now(),
        event_type: memory_engine::types::EventType::MemoryOp,
        payload: json!({}),
        source: "mcp".into(),
        session_id: None,
        scope_id: 1,
        origin_node_id: "mcp".into(),
        sequence_id: 0,
        created_at: None,
    };
    s.engine
        .forget_fact(id, &[HOME.to_owned()], "wrong", HOME, &record)
        .await
        .unwrap();

    let again = s.write(&upload(HOME), &items, &[]).await;
    assert_eq!(outcomes(&again), [ImportOutcome::Duplicate]);
    let other = vec![
        main_item("bf.9.s", 50, stop(StopOutcome::Completed, "forget me")),
        memory("bf.m.9", "forget me", at(60)),
    ];
    let other = s.write(&upload(HOME), &other, &[]).await;

    let blocked: Vec<&str> = other.facts.iter().map(|f| f.outcome).collect();
    assert_eq!(blocked, ["blocked", "blocked"]);
    assert_eq!(s.engine.list_active_facts(None).await.unwrap().len(), 0);
}

/// A record off the engine's invariants is refused with its reason and skipped: it holds no key,
/// and the rest of the upload is stored.
#[tokio::test]
async fn a_refused_record_is_skipped_and_counted() {
    let s = Store::new();
    let token = format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay");
    let items = vec![
        main_item("bf.1.c", 0, compact("")),
        item("bf.2.p", 1, &token, Actor::Main, prompt("a token session")),
        main_item("bf.3.p", 2, prompt("kept")),
    ];

    let write = s.write(&upload(HOME), &items, &[]).await;

    assert_eq!(
        outcomes(&write),
        [
            ImportOutcome::Refused("the compaction summary is empty"),
            ImportOutcome::Refused("the session ID holds a machine token"),
            ImportOutcome::Stored,
        ]
    );
    assert_eq!(s.events().await.len(), 1);
    let fixed = vec![main_item("bf.1.c", 0, compact("now with a summary"))];
    let later = s.write(&upload(HOME), &fixed, &[]).await;
    assert_eq!(outcomes(&later), [ImportOutcome::Stored], "no key was held");
}

/// An imported memory names its machine in its metadata, and its event, which records the import,
/// is its `source_event_id`.
#[tokio::test]
async fn an_imported_memory_names_its_machine_and_event() {
    let s = Store::new();
    let mut upload = upload(HOME);
    upload.kind = "memory-file".into();
    upload.source = "memory/feedback_x.md".into();

    let write = s
        .write(&upload, &[memory("bf.m.1", "a rule", at(-5))], &[])
        .await;

    assert_eq!(write.facts[0].fact_type, FactType::Procedural);
    let fact = &s.engine.list_active_facts(None).await.unwrap()[0];
    assert_eq!(fact.metadata["machine"], "laptop");
    assert_eq!(fact.metadata["memory_type"], "feedback");
    let events = s.events().await;
    assert_eq!(fact.source_event_id, Some(events[0].id));
    assert_eq!(
        events[0].payload,
        json!({
            "machine": "laptop", "kind": "memory-file", "source": "memory/feedback_x.md",
            "harness": "claude-code", "client_version": "0.1.0", "project": provenance("/work/p"),
        })
    );
    assert_eq!(events[0].timestamp, at(-5));
}

/// One text filed under two projects is two memories, one in each.
#[tokio::test]
async fn identical_memory_bodies_in_two_projects_are_two_memories() {
    let s = Store::new();
    let other = "user:alice/project:q";
    s.write(&upload(HOME), &[memory("bf.m.1", "a rule", at(0))], &[])
        .await;
    s.write(&upload(other), &[memory("bf.m.2", "a rule", at(0))], &[])
        .await;

    let mut homes: Vec<i64> = s
        .engine
        .list_active_facts(None)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.scope_id)
        .collect();
    homes.sort_unstable();
    let mut want = vec![
        s.engine.resolve_scope_path(HOME).unwrap(),
        s.engine.resolve_scope_path(other).unwrap(),
    ];
    want.sort_unstable();
    assert_eq!(homes, want);
}

/// v4-shaped UUIDs from a fixed generator, never from a constant list a reader could tune.
fn uuids(n: usize) -> Vec<String> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..n)
        .map(|_| {
            let (a, b) = (next(), next());
            let v4 = (b & 0x0FFF) | 0x4000;
            let variant = ((b >> 16) & 0x3FFF) | 0x8000;
            format!(
                "{:08x}-{:04x}-{v4:04x}-{variant:04x}-{:012x}",
                a >> 32,
                a & 0xFFFF,
                b >> 32 & 0xFFFF_FFFF_FFFF
            )
        })
        .collect()
}

/// Every event ID shape the readers mint survives the payload scrub, so the payload's
/// `event_id` is the receipt's.
#[tokio::test]
async fn import_ids_survive_the_payload_scrub() {
    for uuid in uuids(2000) {
        for id in [
            format!("bf.{uuid}.p"),
            format!("bf.{uuid}.s"),
            format!("bf.{uuid}.c"),
            format!("bf.{uuid}.t0"),
            format!("bf.{uuid}.t12"),
            uuid,
        ] {
            assert_eq!(memory_engine::redact::scrub(&id).unwrap(), id);
        }
    }
    let memory_id = format!("bf.m.{}", "0123456789abcdef01234567");
    assert_eq!(memory_engine::redact::scrub(&memory_id).unwrap(), memory_id);

    let s = Store::new();
    let uuid = &uuids(1)[0];
    let ids = [format!("bf.{uuid}.p"), format!("bf.{uuid}.t3")];
    let items = vec![
        main_item(&ids[0], 0, prompt("a prompt")),
        main_item(&ids[1], 1, tool(json!({}), "a result", &[])),
        memory(&memory_id, "a rule", at(2)),
    ];
    s.write(&upload(HOME), &items, &[]).await;
    let stored: Vec<String> = s
        .events()
        .await
        .iter()
        .filter_map(|e| e.payload["event_id"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(stored, ids);
    let conn = rusqlite::Connection::open(&s.db).unwrap();
    let mut keys: Vec<String> = conn
        .prepare("SELECT event_id FROM lifecycle_receipts ORDER BY event_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut want = vec![ids[0].clone(), ids[1].clone(), memory_id];
    want.sort();
    keys.sort();
    assert_eq!(keys, want);
}

/// A tool input's keys are content: a key holding the denylist literal or a known secret format
/// is redacted in the stored event and in its activity alike.
#[tokio::test]
async fn a_tool_input_key_is_redacted_in_the_event_and_the_activity() {
    let s = Store::new();
    let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let input: serde_json::Map<String, Value> = [
        ("fixturehost".to_owned(), json!("a")),
        (aws.clone(), json!("b")),
    ]
    .into_iter()
    .collect();
    let items = vec![main_item(
        "bf.1.t0",
        0,
        tool(Value::Object(input), "ok", &[]),
    )];

    s.write(&upload(HOME), &items, &["fixturehost".to_owned()])
        .await;

    let stored = s.events().await[0].payload["input"].to_string();
    let conn = rusqlite::Connection::open(&s.db).unwrap();
    let args: String = conn
        .query_row("SELECT args FROM activities", [], |r| r.get(0))
        .unwrap();
    for text in [&stored, &args] {
        assert!(
            !text.contains("fixturehost") && !text.contains(&aws),
            "{text}"
        );
    }
    assert!(stored.contains("[REDACTED:aws-access-key]"), "{stored}");
}

/// The denylist holds host names, which are machine names: a literal equal to the machine and to
/// the session ID redacts content, never an identifier.
#[tokio::test]
async fn a_denylisted_machine_name_leaves_identifiers_intact() {
    let s = Store::new();
    let session = "fixture-session";
    let denylist = vec!["laptop".to_owned(), session.to_owned()];
    let items = vec![
        item(
            "bf.1.p",
            0,
            session,
            Actor::Main,
            prompt("ssh laptop for fixture-session"),
        ),
        item(
            "bf.2.s",
            1,
            session,
            Actor::Main,
            stop(StopOutcome::Completed, "done on laptop"),
        ),
    ];

    s.write(&upload(HOME), &items, &denylist).await;

    let events = s.events().await;
    assert_eq!(
        events[0].payload["prompt"],
        "ssh [REDACTED:denylist] for [REDACTED:denylist]"
    );
    for event in &events {
        assert_eq!(event.payload["machine"], "laptop");
        assert_eq!(event.session_id.as_deref(), Some(session));
    }
    assert_eq!(events[0].payload["event_id"], "bf.1.p");
    let fact = &s.engine.list_active_facts(None).await.unwrap()[0];
    assert_eq!(fact.content, "done on [REDACTED:denylist]");
    assert_eq!(fact.metadata["machine"], "laptop");
    assert_eq!(fact.metadata["session_id"], session);
}

/// The preview is a digest of every effect: the body, the denylist, a memory's date, a project
/// fact and a transcript's session ID each change it; the time of receipt alone does not.
#[tokio::test]
async fn the_preview_changes_with_every_effect_bearing_input() {
    let s = Store::new();
    let preview = |upload: Upload, items: Vec<ImportItem>, denylist: Vec<String>| {
        let engine = &s.engine;
        async move {
            engine
                .import_backfill(&upload, items, &denylist, BatchMode::DryRun)
                .await
                .unwrap()
                .preview
        }
    };
    let transcript = || an_upload();
    let base = preview(upload(HOME), transcript(), vec![]).await;

    let mut later = upload(HOME);
    later.received_at = received() + Duration::days(3);
    assert_eq!(
        preview(later, transcript(), vec![]).await,
        base,
        "receipt time"
    );

    let mut body = transcript();
    body[0] = main_item("bf.1.p", 0, prompt("Add another flag."));
    assert_ne!(preview(upload(HOME), body, vec![]).await, base, "the body");
    assert_ne!(
        preview(upload(HOME), transcript(), vec!["unrelated".into()]).await,
        base,
        "the denylist"
    );
    let mut fact = upload(HOME);
    fact.provenance = provenance("/work/other");
    assert_ne!(
        preview(fact, transcript(), vec![]).await,
        base,
        "a project fact"
    );
    let resumed: Vec<ImportItem> = transcript()
        .into_iter()
        .map(|i| match i {
            ImportItem::Event(mut e) => {
                e.record.session_id = "another-session".into();
                ImportItem::Event(e)
            }
            other @ ImportItem::Memory(_) => other,
        })
        .collect();
    assert_ne!(
        preview(upload(HOME), resumed, vec![]).await,
        base,
        "the session ID alone"
    );

    let memory_upload = || Upload {
        kind: "memory-file".into(),
        ..upload(HOME)
    };
    let dated = preview(memory_upload(), vec![memory("bf.m.1", "r", at(0))], vec![]).await;
    assert_ne!(
        preview(memory_upload(), vec![memory("bf.m.1", "r", at(60))], vec![]).await,
        dated,
        "a memory's mtime"
    );
}
