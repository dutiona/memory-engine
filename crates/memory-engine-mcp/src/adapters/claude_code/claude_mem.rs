//! The claude-mem plugin's store (#1117): its prompts, observations and session summaries,
//! exported as JSON Lines, read into prompts and decayable Episodic memories.
//!
//! What the reader makes of each row is the table of the fixture README
//! (`tests/fixtures/plugin-stores/README.md`). claude-mem keeps no assistant message outside
//! `pending_messages`, which is never exported, so it gives no stop.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use memory_engine::bootstrap::parse_timestamp;
use memory_engine::types::FactType;
use memory_engine::types::lifecycle::{
    Actor, Content, ImportItem, ImportMemory, MemoryItem, NeutralRecord,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::adapters::{
    ExportError, Mapped, Raw, Slice, is_event_id, json_objects, keyed, nullable, query, slices,
};

/// The kind, and the prefix of every event ID.
const KIND: &str = "claude-mem";

/// The concepts that mark an observation as a how-to note (#1026 decision 2): it starts at
/// [`HOW_TO_IMPORTANCE`], still decayable, as a nudge for ME's promotion cycle.
pub const HOW_TO: [&str; 3] = ["problem-solution", "pattern", "gotcha"];

/// A how-to note's starting importance: a hand-written memory file's, never above it.
const HOW_TO_IMPORTANCE: f64 = 0.6;

/// Every other observation's and summary's: the builder's default, a final message's.
const IMPORTANCE: f64 = 0.5;

/// Why an export of more rows than ADR-0009's importer keeps is refused whole.
const OVER_THE_LIMIT: &str = "the export is over the 1,000,000-row limit";

/// A summary's fields and the label of each line.
const SUMMARY: [(&str, &str); 6] = [
    ("request", "Request"),
    ("investigated", "Investigated"),
    ("learned", "Learned"),
    ("completed", "Completed"),
    ("next_steps", "Next steps"),
    ("notes", "Notes"),
];

const PROMPTS: &str = "SELECT s.project, u.content_session_id, u.prompt_number, u.prompt_text,
            u.created_at
       FROM user_prompts u
       LEFT JOIN sdk_sessions s ON s.content_session_id = u.content_session_id
      ORDER BY u.content_session_id, u.created_at, u.id";

const OBSERVATIONS: &str = "SELECT s.project, o.id, s.content_session_id, o.type, o.title,
            o.subtitle, o.text, o.narrative, o.facts, o.concepts, o.created_at
       FROM observations o
       LEFT JOIN sdk_sessions s ON s.memory_session_id = o.memory_session_id
      ORDER BY s.content_session_id, o.created_at, o.id";

const SUMMARIES: &str = "SELECT s.project, m.id, s.content_session_id, m.request, m.investigated,
            m.learned, m.completed, m.next_steps, m.notes, m.created_at
       FROM session_summaries m
       LEFT JOIN sdk_sessions s ON s.memory_session_id = m.memory_session_id
      ORDER BY s.content_session_id, m.created_at, m.id";

/// Export a claude-mem store's prompts, observations and summaries, one slice per
/// `sdk_sessions.project`. `pending_messages` and every other column are left unread.
///
/// # Errors
///
/// [`ExportError::Sqlite`] when a table or column is missing (SQLite's message names it),
/// [`ExportError::Blob`] for a BLOB value, and the slices' own errors.
///
/// # Examples
///
/// ```
/// use memory_engine_mcp::adapters::claude_code::claude_mem::{export, read};
///
/// let store = rusqlite::Connection::open_in_memory()?;
/// store.execute_batch(include_str!("../../../tests/fixtures/plugin-stores/claude-mem.sql"))?;
/// let slices = export(&store)?;
/// let alpha = slices.iter().find(|s| s.project.as_deref() == Some("alpha")).unwrap();
/// assert_eq!((alpha.sessions, alpha.rows), (2, 9));
/// assert_eq!(read(&alpha.body)?.skipped.get("empty observation"), Some(&1));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn export(store: &rusqlite::Connection) -> Result<Vec<Slice>, ExportError> {
    let mut rows = query(store, "user_prompts", "content_session_id", PROMPTS)?;
    rows.extend(query(
        store,
        "observations",
        "content_session_id",
        OBSERVATIONS,
    )?);
    rows.extend(query(
        store,
        "session_summaries",
        "content_session_id",
        SUMMARIES,
    )?);
    slices(rows)
}

/// What one line comes to.
#[expect(
    clippy::large_enum_variant,
    reason = "one row lives at a time, then moves into its list"
)]
enum Row {
    Event(Raw, Value),
    Memory(ImportMemory),
    Skipped(&'static str),
}

/// Read a claude-mem export (#1117).
///
/// A non-empty prompt is a main `prompt`, in full, at its `created_at`, with event ID
/// `bf.claude-mem.<session>.p<prompt number>`. An observation with a title, subtitle, text,
/// narrative or fact is one Episodic memory, `bf.claude-mem.o<id>`, its starting importance
/// 0.6 when its `concepts` holds one of [`HOW_TO`], else 0.5; a session summary with a
/// non-empty field is one Episodic memory, `bf.claude-mem.s<id>`, of importance 0.5. Every
/// memory is dated by its row and carries `source`, `memory_type` and `session_id` in its
/// metadata, an observation its `observation_type` and `concepts` too. A digest is the SHA-256
/// of the canonical JSON of the line's content members, row ids and sessions left out. Skipped
/// rows are counted by reason: `empty prompt`, `empty observation`, `empty summary`,
/// `incomplete row` (a member missing, of another type, or a time that does not parse) and
/// `unknown table`.
///
/// # Errors
///
/// `"no claude-mem row"` when no line is a row of a table it reads; a fixed phrase naming the
/// row limit when the body holds more than 1,000,000 lines.
///
/// # Examples
///
/// ```
/// use memory_engine::types::lifecycle::ImportItem;
/// use memory_engine_mcp::adapters::claude_code::claude_mem::read;
///
/// let body = br#"{"table":"observations","id":7,"content_session_id":"s1","type":"bugfix","title":"Timeout raised","subtitle":null,"text":null,"narrative":"The retry waited too little.","facts":"[]","concepts":"[\"gotcha\"]","created_at":"2026-03-25T10:04:00.000Z"}
/// "#;
/// let mapped = read(body)?;
/// let ImportItem::Memory(memory) = &mapped.items[0] else { panic!("a memory") };
/// assert_eq!(memory.event_id, "bf.claude-mem.o7");
/// assert_eq!(memory.item.text, "Timeout raised\n\nThe retry waited too little.");
/// assert!((memory.item.importance - 0.6).abs() < f64::EPSILON);
/// assert_eq!(read(b"{}\n").unwrap_err(), "no claude-mem row");
/// # Ok::<(), &'static str>(())
/// ```
pub fn read(body: &[u8]) -> Result<Mapped, &'static str> {
    let max_entries = memory_engine::bootstrap::BootstrapConfig::default().max_entries;
    let (objects, malformed) = json_objects(body, max_entries, OVER_THE_LIMIT)?;
    let table = |v: &Value| v.get("table").and_then(Value::as_str).map(str::to_owned);
    if !objects.iter().any(|(_, v)| {
        matches!(
            table(v).as_deref(),
            Some("user_prompts" | "observations" | "session_summaries")
        )
    }) {
        return Err("no claude-mem row");
    }
    let mut skipped: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut events, mut memories) = (Vec::new(), Vec::new());
    for (line, value) in &objects {
        let Some(object) = value.as_object() else {
            continue;
        };
        let row = match object.get("table").and_then(Value::as_str) {
            Some("user_prompts") => prompt(*line, object),
            Some("observations") => observation(object),
            Some("session_summaries") => summary(object),
            _ => Some(Row::Skipped("unknown table")),
        };
        match row.unwrap_or(Row::Skipped("incomplete row")) {
            Row::Event(raw, digest) => events.push((raw, digest)),
            Row::Memory(memory) => memories.push(ImportItem::Memory(memory)),
            Row::Skipped(reason) => *skipped.entry(reason).or_default() += 1,
        }
    }
    let mut items = keyed(events, &mut skipped);
    items.extend(memories);
    Ok(Mapped {
        items,
        lines: objects.len() + malformed,
        malformed,
        skipped,
        pending: 0,
        incomplete_turns: 0,
    })
}

/// A string member, or `None` for a member missing or of another type.
fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

fn session(object: &Map<String, Value>) -> Option<&str> {
    text(object, "content_session_id").filter(|s| !s.is_empty())
}

fn time(object: &Map<String, Value>) -> Option<DateTime<Utc>> {
    text(object, "created_at").and_then(parse_timestamp)
}

/// The canonical JSON of `names`' members of `object`.
fn members(object: &Map<String, Value>, names: &[&str]) -> Value {
    let picked: Map<String, Value> = names
        .iter()
        .map(|&n| (n.to_owned(), object.get(n).cloned().unwrap_or(Value::Null)))
        .collect();
    Value::Object(picked)
}

fn prompt(line: usize, object: &Map<String, Value>) -> Option<Row> {
    let session = session(object)?;
    let number = object.get("prompt_number").and_then(Value::as_i64)?;
    let text = text(object, "prompt_text")?;
    let time = time(object)?;
    let event_id = format!("bf.{KIND}.{session}.p{number}");
    if !is_event_id(&event_id) {
        return None;
    }
    if text.trim().is_empty() {
        return Some(Row::Skipped("empty prompt"));
    }
    let raw = Raw {
        line,
        time,
        event_id,
        record: NeutralRecord {
            session_id: session.to_owned(),
            actor: Actor::Main,
            cwd: None,
            harness_version: None,
            content: Content::Prompt {
                text: Some(text.to_owned()),
            },
        },
    };
    Some(Row::Event(
        raw,
        members(object, &["prompt_text", "created_at"]),
    ))
}

/// A JSON array of strings held as text, else `None`.
fn strings(text: &str) -> Option<Vec<String>> {
    serde_json::from_str(text).ok()
}

fn non_empty(text: &str) -> Option<&str> {
    (!text.trim().is_empty()).then_some(text)
}

fn observation(object: &Map<String, Value>) -> Option<Row> {
    let id = object.get("id").and_then(Value::as_i64)?;
    let session = session(object)?;
    let kind = text(object, "type")?;
    let [title, subtitle, body, narrative, facts, concepts] = [
        "title",
        "subtitle",
        "text",
        "narrative",
        "facts",
        "concepts",
    ]
    .map(|name| nullable(object, name));
    let time = time(object)?;
    let facts: Vec<String> = strings(facts?).unwrap_or_default();
    let concepts = strings(concepts?);
    let heading: Vec<&str> = [title?, subtitle?]
        .into_iter()
        .filter_map(non_empty)
        .collect();
    let listed: Vec<String> = facts
        .iter()
        .filter(|f| !f.trim().is_empty())
        .map(|f| format!("- {f}"))
        .collect();
    let parts: Vec<String> = [
        heading.join("\n"),
        non_empty(body?).unwrap_or_default().to_owned(),
        non_empty(narrative?).unwrap_or_default().to_owned(),
        listed.join("\n"),
    ]
    .into_iter()
    .filter(|p| !p.is_empty())
    .collect();
    if parts.is_empty() {
        return Some(Row::Skipped("empty observation"));
    }
    let how_to = concepts
        .as_ref()
        .is_some_and(|c| c.iter().any(|c| HOW_TO.contains(&c.as_str())));
    let mut metadata = json!({
        "source": KIND,
        "memory_type": "observation",
        "session_id": session,
        "observation_type": kind,
    });
    if let Some(concepts) = concepts {
        metadata["concepts"] = json!(concepts);
    }
    let digest = members(
        object,
        &[
            "type",
            "title",
            "subtitle",
            "text",
            "narrative",
            "facts",
            "concepts",
            "created_at",
        ],
    );
    Some(memory(
        format!("bf.{KIND}.o{id}"),
        &digest,
        MemoryItem {
            text: parts.join("\n\n"),
            fact_type: FactType::Episodic,
            importance: if how_to {
                HOW_TO_IMPORTANCE
            } else {
                IMPORTANCE
            },
            time,
            metadata,
        },
    ))
}

fn summary(object: &Map<String, Value>) -> Option<Row> {
    let id = object.get("id").and_then(Value::as_i64)?;
    let session = session(object)?;
    let time = time(object)?;
    let mut lines = Vec::new();
    for (name, label) in SUMMARY {
        if let Some(value) = non_empty(nullable(object, name)?) {
            lines.push(format!("{label}: {value}"));
        }
    }
    if lines.is_empty() {
        return Some(Row::Skipped("empty summary"));
    }
    let mut names: Vec<&str> = SUMMARY.iter().map(|(name, _)| *name).collect();
    names.push("created_at");
    Some(memory(
        format!("bf.{KIND}.s{id}"),
        &members(object, &names),
        MemoryItem {
            text: lines.join("\n"),
            fact_type: FactType::Episodic,
            importance: IMPORTANCE,
            time,
            metadata: json!({
                "source": KIND,
                "memory_type": "session_summary",
                "session_id": session,
            }),
        },
    ))
}

fn memory(event_id: String, digest: &Value, item: MemoryItem) -> Row {
    let canonical = digest.to_string();
    Row::Memory(ImportMemory {
        event_id,
        body_sha256: Sha256::digest(canonical.as_bytes()).into(),
        body_size: canonical.len() as u64,
        item,
    })
}

#[cfg(test)]
mod tests {
    use memory_engine::types::lifecycle::ImportEvent;

    use super::*;

    fn store() -> rusqlite::Connection {
        let store = rusqlite::Connection::open_in_memory().unwrap();
        store
            .execute_batch(include_str!(
                "../../../tests/fixtures/plugin-stores/claude-mem.sql"
            ))
            .unwrap();
        store
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "the README's table, row by row")]
    fn the_claude_mem_store_maps_by_the_readme_table() {
        let slices = export(&store()).unwrap();
        let projects: Vec<Option<&str>> = slices.iter().map(|s| s.project.as_deref()).collect();
        assert_eq!(projects, [None, Some("alpha"), Some("beta")]);
        let mapped: Vec<Mapped> = slices.iter().map(|s| read(&s.body).unwrap()).collect();
        let (none, alpha, beta) = (&mapped[0], &mapped[1], &mapped[2]);

        let prompts = |m: &Mapped| -> Vec<(String, u32, String)> {
            m.items
                .iter()
                .filter_map(|i| match i {
                    ImportItem::Event(ImportEvent {
                        event_id,
                        seq,
                        record,
                        ..
                    }) => {
                        assert_eq!(record.actor, Actor::Main);
                        assert_eq!(
                            (record.cwd.as_ref(), record.harness_version.as_ref()),
                            (None, None)
                        );
                        let Content::Prompt { text: Some(text) } = &record.content else {
                            panic!("{:?}", record.content)
                        };
                        Some((event_id.clone(), *seq, text.clone()))
                    }
                    ImportItem::Memory(_) => None,
                })
                .collect()
        };
        let a = "bf.claude-mem.eef5665a-2f30-497d-87e7-ef64be1076a7";
        assert_eq!(
            prompts(alpha),
            [
                (
                    "bf.claude-mem.e4bff46f-24ef-4ab9-b58d-b26d091cfc13.p1".to_owned(),
                    0,
                    "What does the retry flag default to?".to_owned()
                ),
                (
                    format!("{a}.p1"),
                    120_000,
                    "Add a retry flag to the fetch command.".to_owned()
                ),
                (
                    format!("{a}.p2"),
                    120_001,
                    "Now document the flag.".to_owned()
                ),
            ]
        );
        assert_eq!(prompts(beta).len(), 1);
        assert_eq!(prompts(none), Vec::new());

        let memories = |m: &Mapped| -> Vec<(String, String, f64, Value)> {
            m.items
                .iter()
                .filter_map(|i| match i {
                    ImportItem::Memory(memory) => {
                        assert_eq!(memory.item.fact_type, FactType::Episodic);
                        Some((
                            memory.event_id.clone(),
                            memory.item.text.clone(),
                            memory.item.importance,
                            memory.item.metadata.clone(),
                        ))
                    }
                    ImportItem::Event(_) => None,
                })
                .collect()
        };
        let session = "eef5665a-2f30-497d-87e7-ef64be1076a7";
        let observation = |kind: &str, concepts: Option<&[&str]>| {
            let mut metadata = json!({
                "source": "claude-mem", "memory_type": "observation", "session_id": session,
                "observation_type": kind,
            });
            if let Some(concepts) = concepts {
                metadata["concepts"] = json!(concepts);
            }
            metadata
        };
        assert_eq!(
            memories(alpha),
            [
                (
                    "bf.claude-mem.o1".to_owned(),
                    "Retry flag parsing\nHow the fetch command reads its flags\n\n\
                     The fetch command reads every flag before its first request.\n\n\
                     - The flags are parsed once, at start.\n- The retry count defaults to three."
                        .to_owned(),
                    0.6,
                    observation("discovery", Some(&["how-it-works", "pattern"])),
                ),
                (
                    "bf.claude-mem.o2".to_owned(),
                    "Retry flag added\n\nAdded the flag and its help line.\n\n- fetch takes a retry flag."
                        .to_owned(),
                    0.5,
                    observation("change", Some(&["what-changed"])),
                ),
                (
                    "bf.claude-mem.o4".to_owned(),
                    "Timeout raised\n\nThe second try waited too little.".to_owned(),
                    0.6,
                    observation("bugfix", Some(&["gotcha"])),
                ),
                (
                    "bf.claude-mem.s1".to_owned(),
                    "Request: Add a retry flag.\nInvestigated: How fetch parses its flags.\n\
                     Learned: Flags are read once.\nCompleted: The flag and its help line.\n\
                     Next steps: Document the flag.\nNotes: The default stays three."
                        .to_owned(),
                    0.5,
                    json!({ "source": "claude-mem", "memory_type": "session_summary", "session_id": session }),
                ),
            ]
        );
        let beta_memories = memories(beta);
        assert_eq!(
            beta_memories
                .iter()
                .map(|(id, text, importance, _)| (id.as_str(), text.as_str(), *importance))
                .collect::<Vec<_>>(),
            [
                (
                    "bf.claude-mem.o5",
                    "Keep the default\nThree tries stay the default\n\n\
                     Changing it would surprise the scripts that call fetch.",
                    0.5
                ),
                (
                    "bf.claude-mem.s2",
                    "Request: Decide the default.\nCompleted: Kept three tries.",
                    0.5
                ),
            ]
        );
        assert_eq!(beta_memories[0].3.get("concepts"), None, "not a JSON list");
        let skipped = |m: &Mapped| m.skipped.clone();
        assert_eq!(
            skipped(alpha),
            BTreeMap::from([("empty observation", 1), ("empty summary", 1)])
        );
        assert_eq!(skipped(beta), BTreeMap::from([("empty prompt", 1)]));
        assert_eq!(skipped(none), BTreeMap::from([("incomplete row", 1)]));
    }
}
