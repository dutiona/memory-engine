//! One adapter per harness (ADR-0020 §1): it turns one harness body into one neutral record,
//! and is chosen by a `match` on `ME-Harness`. There is no adapter trait until a second adapter
//! exists.
//!
//! A harness's backfill readers (#1026) live in its adapter too, one per `ME-Backfill-Kind`,
//! chosen by a `match` as well.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Timelike, Utc};
use memory_engine::bootstrap::read_json_lines;
use memory_engine::types::lifecycle::{ImportEvent, ImportItem, NeutralRecord};
use rusqlite::types::ValueRef;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub mod claude_code;

/// What a backfill reader made of one uploaded file: its items, and the census of what it did
/// not import.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mapped {
    /// The items, in file order.
    pub items: Vec<ImportItem>,
    /// The file's non-empty lines.
    pub lines: usize,
    /// The lines that are not a JSON object.
    pub malformed: usize,
    /// What was read and not imported, by reason: a fixed phrase each.
    pub skipped: BTreeMap<&'static str, usize>,
    /// The tool calls whose result is not in the file yet: a later upload imports them.
    pub pending: usize,
    /// The turns with no completed message: interrupted, killed, or copied mid-turn.
    pub incomplete_turns: usize,
}

/// The records one millisecond may hold: `seq` is the millisecond times this plus the rank,
/// so it stays within ADR-0020's six digits.
const PER_MILLISECOND: u32 = 1000;

/// A record, in file order, before its order key.
pub(crate) struct Raw {
    pub(crate) line: usize,
    pub(crate) time: DateTime<Utc>,
    pub(crate) event_id: String,
    pub(crate) record: NeutralRecord,
}

/// `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, ADR-0020 §3's event ID.
pub(crate) fn is_event_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A body's JSON objects, up to the entry limit: the objects with their line, and the
/// lines that are not objects, else the phrase naming the limit.
#[expect(
    clippy::type_complexity,
    reason = "the objects with their lines, and the malformed count"
)]
pub(crate) fn json_objects(
    body: &[u8],
    max_entries: usize,
    over_the_limit: &'static str,
) -> Result<(Vec<(usize, Value)>, usize), &'static str> {
    let (objects, malformed, truncated) =
        read_json_lines(body, memory_engine::BACKFILL_BODY_BYTES as u64, max_entries);
    if truncated {
        return Err(over_the_limit);
    }
    Ok((objects, malformed))
}

/// A reader's records, in file order, as events: the per-millisecond rank map, the
/// "over 1,000 records in one millisecond" skip counted into `skipped`, and each record
/// keyed by its time, its rank and the digest of its paired value.
pub(crate) fn keyed(
    records: Vec<(Raw, Value)>,
    skipped: &mut BTreeMap<&'static str, usize>,
) -> Vec<ImportItem> {
    let mut items = Vec::with_capacity(records.len());
    let mut ranks: HashMap<(i64, u32), u32> = HashMap::new();
    for (raw, value) in records {
        let millisecond = raw.time.timestamp_subsec_millis().min(PER_MILLISECOND - 1);
        let rank = ranks
            .entry((raw.time.timestamp(), millisecond))
            .or_default();
        if *rank >= PER_MILLISECOND {
            *skipped
                .entry("over 1,000 records in one millisecond")
                .or_default() += 1;
            continue;
        }
        let seq = millisecond * PER_MILLISECOND + *rank;
        *rank += 1;
        let canonical = value.to_string();
        items.push(ImportItem::Event(ImportEvent {
            event_id: raw.event_id,
            time: raw.time.with_nanosecond(0).unwrap_or(raw.time),
            seq,
            body_sha256: Sha256::digest(canonical.as_bytes()).into(),
            body_size: canonical.len() as u64,
            line: raw.line,
            record: raw.record,
        }));
    }
    items
}

/// One project's rows of a plugin store (#1117): one upload's body, one JSON object per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    /// The project its rows belong to; `None` for rows of no project.
    pub project: Option<String>,
    /// Its distinct sessions.
    pub sessions: usize,
    /// Its lines.
    pub rows: usize,
    /// The JSON Lines body.
    pub body: Vec<u8>,
}

/// Why a plugin store could not be exported (#1117).
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// A query failed; SQLite's message names a missing table or column.
    #[error("the store could not be read: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A value is a BLOB, which no line can carry.
    #[error("{table}.{column} holds a BLOB")]
    Blob {
        /// The table read.
        table: &'static str,
        /// The column or member name.
        column: String,
    },
    /// A project holds a tab or a line break, which `index.tsv` cannot carry.
    #[error("a project holds a tab or a line break")]
    ControlInProject,
    /// A project's rows are over the upload bound: never split, so refused whole.
    #[error("a project's rows are over the 256 MiB upload bound")]
    OverTheBound,
}

/// A member that is a string or `null`, as text: `null` is the empty string, and a member
/// missing or of another type is `None`.
pub(crate) fn nullable<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    match object.get(name)? {
        Value::Null => Some(""),
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// A row of a plugin store: its project, its session, and its line.
pub(crate) type Row = (Option<String>, String, Map<String, Value>);

/// The rows `sql` selects from `table`: its first column is the row's project, every other one
/// a member named as its column, `session` the member naming its session, and `table` the
/// line's tag. TEXT is a string (invalid UTF-8 replaced), INTEGER and REAL numbers, NULL
/// `null`.
pub(crate) fn query(
    store: &rusqlite::Connection,
    table: &'static str,
    session: &str,
    sql: &str,
) -> Result<Vec<Row>, ExportError> {
    let mut stmt = store.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut line = Map::new();
        line.insert("table".to_owned(), Value::from(table));
        let mut project = None;
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => Value::from(n),
                ValueRef::Real(x) => Value::from(x),
                ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(_) => {
                    return Err(ExportError::Blob {
                        table,
                        column: name.clone(),
                    });
                }
            };
            if i == 0 {
                project = value.as_str().map(str::to_owned);
            } else {
                line.insert(name.clone(), value);
            }
        }
        let session = line
            .get(session)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        out.push((project, session, line));
    }
    Ok(out)
}

/// Group `rows` by project, in their order, into one slice each, by project.
///
/// # Errors
///
/// [`ExportError::ControlInProject`] for a project holding a tab or a line break,
/// [`ExportError::OverTheBound`] for a slice over [`memory_engine::BACKFILL_BODY_BYTES`].
pub(crate) fn slices(rows: Vec<Row>) -> Result<Vec<Slice>, ExportError> {
    slices_capped(rows, memory_engine::BACKFILL_BODY_BYTES as u64)
}

/// [`slices`] with an explicit bound on a slice's bytes (test seam).
fn slices_capped(rows: Vec<Row>, bound: u64) -> Result<Vec<Slice>, ExportError> {
    let mut by_project: BTreeMap<Option<String>, (BTreeSet<String>, Slice)> = BTreeMap::new();
    for (project, session, line) in rows {
        if project
            .as_deref()
            .is_some_and(|p| p.contains(['\t', '\n', '\r']))
        {
            return Err(ExportError::ControlInProject);
        }
        let (sessions, slice) = by_project.entry(project.clone()).or_insert_with(|| {
            (
                BTreeSet::new(),
                Slice {
                    project,
                    sessions: 0,
                    rows: 0,
                    body: Vec::new(),
                },
            )
        });
        if !session.is_empty() {
            sessions.insert(session);
        }
        slice.rows += 1;
        slice
            .body
            .extend(Value::Object(line).to_string().into_bytes());
        slice.body.push(b'\n');
        if slice.body.len() as u64 > bound {
            return Err(ExportError::OverTheBound);
        }
    }
    Ok(by_project
        .into_values()
        .map(|(sessions, mut slice)| {
            slice.sessions = sessions.len();
            slice
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::claude_code::{claude_mem, claude_memory};
    use super::*;

    const CLAUDE_MEM: &str = include_str!("../../tests/fixtures/plugin-stores/claude-mem.sql");
    const CLAUDE_MEMORY: &str =
        include_str!("../../tests/fixtures/plugin-stores/claude-memory.sql");

    fn store(sql: &str) -> rusqlite::Connection {
        let store = rusqlite::Connection::open_in_memory().unwrap();
        store.execute_batch(sql).unwrap();
        store
    }

    type Export = fn(&rusqlite::Connection) -> Result<Vec<Slice>, ExportError>;
    type Read = fn(&[u8]) -> Result<Mapped, &'static str>;

    const STORES: [(&str, Export, Read); 2] = [
        (CLAUDE_MEM, claude_mem::export, claude_mem::read),
        (CLAUDE_MEMORY, claude_memory::export, claude_memory::read),
    ];

    /// Every item's key, as a later upload meets it: an event's ID, order key and digest, a
    /// memory's ID and digest.
    fn keys(store: &rusqlite::Connection, export: Export, read: Read) -> BTreeSet<String> {
        export(store)
            .unwrap()
            .iter()
            .flat_map(|s| read(&s.body).unwrap().items)
            .map(|item| match item {
                ImportItem::Event(e) => {
                    format!("{} {} {} {:?}", e.event_id, e.time, e.seq, e.body_sha256)
                }
                ImportItem::Memory(m) => format!("{} {:?}", m.event_id, m.body_sha256),
            })
            .collect()
    }

    #[test]
    fn an_export_holds_only_the_rows_the_reader_reads() {
        for (sql, export, _) in STORES {
            let all = store(sql);
            for slice in export(&all).unwrap() {
                let body = String::from_utf8(slice.body).unwrap();
                assert!(!body.contains("NEVER-EXPORTED"), "{body}");
                assert_eq!(body.lines().count(), slice.rows);
            }
        }
    }

    #[test]
    fn a_session_stays_in_its_projects_slice() {
        let memory = claude_memory::export(&store(CLAUDE_MEMORY)).unwrap();
        let sessions = |slice: &Slice| -> BTreeSet<String> {
            String::from_utf8_lossy(&slice.body)
                .lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap()["session"].to_string())
                .collect()
        };
        let alpha = memory
            .iter()
            .find(|s| s.project.as_deref() == Some("/work/fixture/alpha"))
            .unwrap();
        assert_eq!(
            sessions(alpha),
            BTreeSet::from([
                "\"a5b6c7d\"".to_owned(),
                "\"eef5665a-2f30-497d-87e7-ef64be1076a7\"".to_owned()
            ]),
            "the subagent in its main session's slice"
        );
        assert_eq!(alpha.sessions, 2);
        let none = memory.iter().find(|s| s.project.is_none()).unwrap();
        assert_eq!(
            sessions(none),
            BTreeSet::from(["\"6abf0af7-d5d4-4183-97b4-14142808dffb\"".to_owned()])
        );
        let unfiled = store(CLAUDE_MEMORY);
        unfiled
            .execute_batch("UPDATE sessions SET project_id = NULL WHERE id = 1;")
            .unwrap();
        let unfiled = claude_memory::export(&unfiled).unwrap();
        let none = unfiled.iter().find(|s| s.project.is_none()).unwrap();
        assert!(
            sessions(none).contains("\"a5b6c7d\""),
            "a subagent stays with its main session, which has no project"
        );
        assert!(
            unfiled
                .iter()
                .all(|s| s.project.as_deref() != Some("/work/fixture/alpha"))
        );
        let mem = claude_mem::export(&store(CLAUDE_MEM)).unwrap();
        let orphan = mem.iter().find(|s| s.project.is_none()).unwrap();
        assert_eq!((orphan.sessions, orphan.rows), (0, 1));
    }

    #[test]
    fn store_keys_are_stable_across_a_grown_copy() {
        let mem = store(CLAUDE_MEM);
        let before = keys(&mem, claude_mem::export, claude_mem::read);
        mem.execute_batch(
            "INSERT INTO user_prompts (content_session_id, prompt_number, prompt_text, created_at,
               created_at_epoch) VALUES ('eef5665a-2f30-497d-87e7-ef64be1076a7', 3, 'One more.',
               '2026-03-25T11:00:00.000Z', 1774436400000);
             INSERT INTO observations (memory_session_id, project, type, title, facts, concepts,
               created_at, created_at_epoch) VALUES ('mem-fixture-a', 'alpha', 'change', 'More',
               '[]', '[]', '2026-03-25T11:01:00.000Z', 1774436460000);",
        )
        .unwrap();
        let grown = keys(&mem, claude_mem::export, claude_mem::read);
        assert!(grown.is_superset(&before));
        let added: Vec<&String> = grown.difference(&before).collect();
        assert_eq!(added.len(), 2, "{added:?}");

        let memory = store(CLAUDE_MEMORY);
        let before = keys(&memory, claude_memory::export, claude_memory::read);
        memory
            .execute_batch(
                "INSERT INTO messages (id, session_id, uuid, timestamp, role, content) VALUES
                   (50, 2, '0f1e2d3c-4b5a-4697-8877-665544332211', '2026-03-28T14:02:00.000Z',
                    'user', 'And the lints?'),
                   (51, 2, '1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d', '2026-03-28T14:02:30.000Z',
                    'assistant', 'They pass too.');",
            )
            .unwrap();
        let grown = keys(&memory, claude_memory::export, claude_memory::read);
        assert!(grown.is_superset(&before), "a new turn re-sends every key");
        assert_eq!(grown.difference(&before).count(), 2);

        memory
            .execute_batch(
                "INSERT INTO messages (id, session_id, uuid, timestamp, role, content) VALUES
                   (52, 2, '2b3c4d5e-6f7a-4b8c-9d0e-1f2a3b4c5d6e', '2026-03-28T14:03:00.000Z',
                    'assistant', 'And so do the docs.');",
            )
            .unwrap();
        let longer = keys(&memory, claude_memory::export, claude_memory::read);
        let gone: Vec<&String> = grown.difference(&longer).collect();
        let new: Vec<&String> = longer.difference(&grown).collect();
        assert!(
            gone.len() == 1
                && gone[0].starts_with("bf.claude-memory.1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d.s "),
            "the old last line is narration now: {gone:?}"
        );
        assert!(
            new.len() == 1
                && new[0].starts_with("bf.claude-memory.2b3c4d5e-6f7a-4b8c-9d0e-1f2a3b4c5d6e.s "),
            "its turn's new last line is the stop: {new:?}"
        );

        let renumbered = store(CLAUDE_MEMORY);
        renumbered
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 UPDATE messages SET id = id + 100, session_id = session_id + 100;
                 UPDATE sessions SET id = id + 100, parent_session_id = parent_session_id + 100;
                 UPDATE projects SET id = id + 100;
                 UPDATE sessions SET project_id = project_id + 100;",
            )
            .unwrap();
        assert_eq!(
            keys(&renumbered, claude_memory::export, claude_memory::read),
            keys(
                &store(CLAUDE_MEMORY),
                claude_memory::export,
                claude_memory::read
            ),
            "row ids are not keys"
        );
    }

    #[test]
    fn store_ids_follow_the_grammar_and_survive_the_scrub() {
        for (sql, export, read) in STORES {
            for slice in export(&store(sql)).unwrap() {
                for item in read(&slice.body).unwrap().items {
                    let (id, session) = match &item {
                        ImportItem::Event(e) => (e.event_id.clone(), e.record.session_id.clone()),
                        ImportItem::Memory(m) => (
                            m.event_id.clone(),
                            m.item.metadata["session_id"].as_str().unwrap().to_owned(),
                        ),
                    };
                    assert!(is_event_id(&id), "{id}");
                    assert!(
                        id.starts_with("bf.claude-mem.") || id.starts_with("bf.claude-memory."),
                        "disjoint from bf.<uuid>.<tag> and bf.m.: {id}"
                    );
                    for identifier in [&id, &session] {
                        assert_eq!(
                            &memory_engine::redact::scrub(identifier).unwrap(),
                            identifier
                        );
                    }
                }
            }
        }
    }

    /// A BLOB no line can carry, a project `index.tsv` cannot carry, and a project over the upload
    /// bound each refuse the whole export, naming what failed.
    #[test]
    fn an_export_refuses_what_no_upload_can_carry() {
        let blob = store(CLAUDE_MEMORY);
        blob.execute_batch("UPDATE messages SET content = x'00ff' WHERE id = 1;")
            .unwrap();
        assert!(matches!(
            claude_memory::export(&blob),
            Err(ExportError::Blob { table: "messages", column }) if column == "content"
        ));
        let tab = store(CLAUDE_MEMORY);
        tab.execute_batch(
            "UPDATE projects SET path = '/work/fixture/al' || char(9) || 'pha' WHERE id = 1;",
        )
        .unwrap();
        assert!(matches!(
            claude_memory::export(&tab),
            Err(ExportError::ControlInProject)
        ));
        let rows = query(
            &store(CLAUDE_MEMORY),
            "messages",
            "session",
            "SELECT NULL, content FROM messages",
        )
        .unwrap();
        let largest = slices(rows.clone()).unwrap()[0].body.len() as u64;
        assert!(slices_capped(rows.clone(), largest).is_ok(), "at the bound");
        assert!(matches!(
            slices_capped(rows, largest - 1),
            Err(ExportError::OverTheBound)
        ));
    }

    #[test]
    fn a_body_without_a_store_row_is_refused() {
        let mem = claude_mem::export(&store(CLAUDE_MEM)).unwrap();
        let memory = claude_memory::export(&store(CLAUDE_MEMORY)).unwrap();
        assert_eq!(
            claude_mem::read(&memory[0].body).unwrap_err(),
            "no claude-mem row"
        );
        assert_eq!(
            claude_memory::read(&mem[1].body).unwrap_err(),
            "no claude-memory row"
        );
        for read in [claude_mem::read, claude_memory::read] {
            assert!(read(b"").is_err());
            assert!(read(b"[1]\n{\"table\":\"pending_messages\"}\n").is_err());
        }
    }
}
