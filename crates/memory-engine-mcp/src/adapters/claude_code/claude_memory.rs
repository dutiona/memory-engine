//! The claude-memory plugin's store (#1117): a `conversations.db` exported as JSON Lines, one
//! object per message, read into the prompts and stops live capture would have recorded.
//!
//! What the reader makes of each row is the table of the fixture README
//! (`tests/fixtures/plugin-stores/README.md`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use memory_engine::bootstrap::parse_timestamp;
use memory_engine::types::lifecycle::{Actor, Content, NeutralRecord, StopOutcome};
use serde_json::{Map, Value, json};

use super::INTERRUPTED;
use crate::adapters::{
    ExportError, Mapped, Raw, Slice, is_event_id, json_objects, keyed, nullable, query, slices,
};

/// The kind, the `table` of every line, and the prefix of every event ID.
const KIND: &str = "claude-memory";
const TABLE: &str = "messages";

/// Why an export of more rows than ADR-0009's importer keeps is refused whole.
const OVER_THE_LIMIT: &str = "the export is over the 1,000,000-row limit";

/// Every message of every session, with its session's identity, in its main session's project,
/// none included, so a subagent's rows never land in another slice than its main session's.
const MESSAGES: &str =
    "SELECT CASE WHEN ps.id IS NULL THEN p.path ELSE pp.path END, m.id, m.uuid, s.uuid AS session,
            ps.uuid AS parent_session, s.cwd, m.timestamp, m.role, m.content, m.is_notification
       FROM messages m
       LEFT JOIN sessions s ON s.id = m.session_id
       LEFT JOIN projects p ON p.id = s.project_id
       LEFT JOIN sessions ps ON ps.id = s.parent_session_id
       LEFT JOIN projects pp ON pp.id = ps.project_id
      ORDER BY s.uuid, m.timestamp, m.id";

/// Export a claude-memory store's messages, one slice per project: a subagent session's in its
/// main session's, so no session spans two slices. Every other table is left unread.
///
/// # Errors
///
/// [`ExportError::Sqlite`] when a table or column is missing (SQLite's message names it),
/// [`ExportError::Blob`] for a BLOB value, and the slices' own errors.
///
/// # Examples
///
/// ```
/// use memory_engine_mcp::adapters::claude_code::claude_memory::{export, read};
///
/// let store = rusqlite::Connection::open_in_memory()?;
/// store.execute_batch(include_str!("../../../tests/fixtures/plugin-stores/claude-memory.sql"))?;
/// let slices = export(&store)?;
/// assert_eq!(slices.len(), 3);
/// let alpha = slices.iter().find(|s| s.project.as_deref() == Some("/work/fixture/alpha")).unwrap();
/// assert_eq!((alpha.sessions, alpha.rows), (2, 17));
/// assert_eq!(read(&alpha.body)?.skipped.get("narration"), Some(&2));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn export(store: &rusqlite::Connection) -> Result<Vec<Slice>, ExportError> {
    slices(query(store, TABLE, "session", MESSAGES)?)
}

/// One message line's members.
struct Message<'a> {
    line: usize,
    id: i64,
    uuid: &'a str,
    session: &'a str,
    parent: Option<&'a str>,
    cwd: Option<&'a str>,
    time: DateTime<Utc>,
    user: bool,
    content: &'a str,
    notification: bool,
    /// The content members its digest is taken over.
    digest: Value,
}

impl Message<'_> {
    /// The record this message makes: a subagent's in its main session.
    fn record(&self, content: Content) -> NeutralRecord {
        let (session_id, actor) = self.parent.map_or((self.session, Actor::Main), |parent| {
            let id = Some(self.session.to_owned());
            (parent, Actor::Subagent { id, kind: None })
        });
        NeutralRecord {
            session_id: session_id.to_owned(),
            actor,
            cwd: self.cwd.map(str::to_owned),
            harness_version: None,
            content,
        }
    }

    /// Its record with its event ID, and the content members its digest is taken over.
    fn raw(&self, tag: char, content: Content) -> (Raw, Value) {
        let raw = Raw {
            line: self.line,
            time: self.time,
            event_id: event_id(self.uuid, tag),
            record: self.record(content),
        };
        (raw, self.digest.clone())
    }
}

fn event_id(uuid: &str, tag: char) -> String {
    format!("bf.{KIND}.{uuid}.{tag}")
}

/// A `messages` line with every member the reader needs, else `None`.
fn message(line: usize, object: &Map<String, Value>) -> Option<Message<'_>> {
    let text = |name: &str| object.get(name).and_then(Value::as_str);
    let uuid = text("uuid").filter(|u| is_event_id(&event_id(u, 's')))?;
    let role = text("role")?;
    let user = match role {
        "user" => true,
        "assistant" => false,
        _ => return None,
    };
    let flag = object.get("is_notification").and_then(Value::as_i64)?;
    let notification = match flag {
        0 => false,
        1 => true,
        _ => return None,
    };
    let (timestamp, content) = (text("timestamp")?, text("content")?);
    Some(Message {
        line,
        id: object.get("id").and_then(Value::as_i64)?,
        uuid,
        session: text("session").filter(|s| !s.is_empty())?,
        parent: Some(nullable(object, "parent_session")?).filter(|p| !p.is_empty()),
        cwd: Some(nullable(object, "cwd")?).filter(|c| !c.is_empty()),
        time: parse_timestamp(timestamp)?,
        user,
        content,
        notification,
        digest: json!({
            "role": role, "content": content, "timestamp": timestamp, "is_notification": flag,
        }),
    })
}

/// Read a claude-memory export (#1117) into the prompts and stops live capture would have
/// recorded.
///
/// A session's messages are read by time, then by `id`. Every `user` message ends the current
/// turn, and so does the end of the session. A turn's last `assistant` message is its `stop`,
/// `completed`, unless an interrupt marker ended the turn, which live capture closes with no
/// stop (`interrupted turn`); the other assistant messages are the narration between tool
/// calls, which live capture never records (`narration`). A `user` message is a `prompt`,
/// unless it is a task notification (`notification`), an interrupt marker (`interrupt`) or
/// empty (`empty message`). A subagent session's records are its main session's, with the
/// subagent as their actor. A record's event ID is `bf.claude-memory.<message uuid>.p` or `.s`,
/// its digest the SHA-256 of the canonical JSON of the message's `role`, `content`,
/// `timestamp` and `is_notification`, and its order key the transcript reader's.
///
/// # Errors
///
/// `"no claude-memory row"` when no line is a message with a session and a role; a fixed phrase
/// naming the row limit when the body holds more than 1,000,000 lines.
///
/// # Examples
///
/// ```
/// use memory_engine::types::lifecycle::{Content, ImportItem};
/// use memory_engine_mcp::adapters::claude_code::claude_memory::read;
///
/// let body = br#"{"table":"messages","id":1,"uuid":"u1","session":"s1","parent_session":null,"cwd":null,"timestamp":"2026-03-25T10:00:00.120Z","role":"user","content":"Add a flag.","is_notification":0}
/// {"table":"messages","id":2,"uuid":"u2","session":"s1","parent_session":null,"cwd":null,"timestamp":"2026-03-25T10:00:09.000Z","role":"assistant","content":"Added it.","is_notification":0}
/// "#;
/// let mapped = read(body)?;
/// let ImportItem::Event(stop) = &mapped.items[1] else { panic!("an event") };
/// assert_eq!(stop.event_id, "bf.claude-memory.u2.s");
/// assert!(matches!(&stop.record.content, Content::Stop { message: Some(m), .. } if m == "Added it."));
/// assert_eq!(read(b"{}\n").unwrap_err(), "no claude-memory row");
/// # Ok::<(), &'static str>(())
/// ```
pub fn read(body: &[u8]) -> Result<Mapped, &'static str> {
    let max_entries = memory_engine::bootstrap::BootstrapConfig::default().max_entries;
    let (objects, malformed) = json_objects(body, max_entries, OVER_THE_LIMIT)?;
    if !objects
        .iter()
        .any(|(_, v)| v.get("session").is_some_and(Value::is_string) && v.get("role").is_some())
    {
        return Err("no claude-memory row");
    }
    let mut skipped: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut sessions: BTreeMap<&str, Vec<Message<'_>>> = BTreeMap::new();
    for (line, value) in &objects {
        let Some(object) = value.as_object() else {
            continue;
        };
        if object.get("table").and_then(Value::as_str) != Some(TABLE) {
            *skipped.entry("unknown table").or_default() += 1;
            continue;
        }
        match message(*line, object) {
            Some(message) => sessions.entry(message.session).or_default().push(message),
            None => *skipped.entry("incomplete row").or_default() += 1,
        }
    }
    let mut records = Vec::new();
    for messages in sessions.values_mut() {
        messages.sort_by_key(|m| (m.time, m.id));
        turns(messages, &mut records, &mut skipped);
    }
    records.sort_by_key(|(raw, _): &(Raw, Value)| raw.line);
    let items = keyed(records, &mut skipped);
    Ok(Mapped {
        items,
        lines: objects.len() + malformed,
        malformed,
        skipped,
        pending: 0,
        incomplete_turns: 0,
    })
}

/// Walk one session's messages, in order, turn by turn.
fn turns(
    messages: &[Message<'_>],
    records: &mut Vec<(Raw, Value)>,
    skipped: &mut BTreeMap<&'static str, usize>,
) {
    let mut skip = |reason| *skipped.entry(reason).or_default() += 1;
    // The current turn's last assistant message.
    let mut last: Option<&Message<'_>> = None;
    for message in messages {
        if !message.user {
            if last.replace(message).is_some() {
                skip("narration");
            }
            continue;
        }
        let interrupt = message.content.starts_with(INTERRUPTED);
        if let Some(stop) = last.take() {
            if interrupt {
                skip("interrupted turn");
            } else {
                records.push(stop.raw('s', final_message(stop)));
            }
        }
        if message.notification {
            skip("notification");
        } else if interrupt {
            skip("interrupt");
        } else if message.content.trim().is_empty() {
            skip("empty message");
        } else {
            let text = Some(message.content.to_owned());
            records.push(message.raw('p', Content::Prompt { text }));
        }
    }
    if let Some(stop) = last {
        records.push(stop.raw('s', final_message(stop)));
    }
}

fn final_message(message: &Message<'_>) -> Content {
    Content::Stop {
        outcome: StopOutcome::Completed,
        message: Some(message.content.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use memory_engine::types::lifecycle::{ImportEvent, ImportItem};

    use super::*;

    fn store() -> rusqlite::Connection {
        let store = rusqlite::Connection::open_in_memory().unwrap();
        store
            .execute_batch(include_str!(
                "../../../tests/fixtures/plugin-stores/claude-memory.sql"
            ))
            .unwrap();
        store
    }

    /// Every slice of `store`, read: its project and what the reader made of it.
    fn read_all(store: &rusqlite::Connection) -> Vec<(Option<String>, Mapped)> {
        export(store)
            .unwrap()
            .into_iter()
            .map(|s| (s.project, read(&s.body).unwrap()))
            .collect()
    }

    fn events(mapped: &Mapped) -> Vec<&ImportEvent> {
        mapped
            .items
            .iter()
            .map(|item| match item {
                ImportItem::Event(event) => event,
                ImportItem::Memory(_) => panic!("claude-memory makes no memory"),
            })
            .collect()
    }

    /// An event as the README's table names it: its message uuid's first 8 digits, its tag, its
    /// kind, its actor and its session.
    fn row(event: &ImportEvent) -> (String, &'static str, String, String) {
        let id = event.event_id.trim_start_matches("bf.claude-memory.");
        let actor = match &event.record.actor {
            Actor::Main => "main".to_owned(),
            Actor::Subagent { id, kind } => {
                assert_eq!(kind, &None);
                format!("subagent {}", id.as_deref().unwrap_or_default())
            }
        };
        (
            format!("{}{}", &id[..8], &id[id.len() - 2..]),
            event.record.content.kind().as_str(),
            actor,
            event.record.session_id[..8].to_owned(),
        )
    }

    fn table(rows: &[(&str, &str, &str, &str)]) -> Vec<(String, &'static str, String, String)> {
        rows.iter()
            .map(|(id, kind, actor, session)| {
                let kind = match *kind {
                    "prompt" => "prompt",
                    "stop" => "stop",
                    other => panic!("{other}"),
                };
                (
                    (*id).to_owned(),
                    kind,
                    (*actor).to_owned(),
                    (*session).to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn the_claude_memory_store_maps_by_the_readme_table() {
        let slices = read_all(&store());
        let projects: Vec<Option<&str>> = slices.iter().map(|(p, _)| p.as_deref()).collect();
        assert_eq!(
            projects,
            [
                None,
                Some("/work/fixture/alpha"),
                Some("/work/fixture/beta")
            ]
        );
        let (_, none) = &slices[0];
        let (_, alpha) = &slices[1];
        let (_, beta) = &slices[2];
        assert_eq!(
            alpha.items.len() + beta.items.len() + none.items.len(),
            16,
            "8 prompts and 8 stops"
        );
        let a = "eef5665a";
        let sub = "subagent a5b6c7d";
        assert_eq!(
            events(alpha).iter().map(|e| row(e)).collect::<Vec<_>>(),
            table(&[
                ("3e3cec8b.p", "prompt", sub, a),
                ("87537b77.s", "stop", sub, a),
                ("2a30e5fe.p", "prompt", sub, a),
                ("cdc6c2ea.s", "stop", sub, a),
                ("f373ecaf.p", "prompt", "main", a),
                ("4fc05562.s", "stop", "main", a),
                ("9656039c.s", "stop", "main", a),
                ("214eba6d.p", "prompt", "main", a),
                ("75d52698.s", "stop", "main", a),
                ("9b153fff.p", "prompt", "main", a),
                ("02ad0bec.p", "prompt", "main", a),
                ("27cbe3e2.s", "stop", "main", a),
            ])
        );
        assert_eq!(
            events(beta).iter().map(|e| row(e)).collect::<Vec<_>>(),
            table(&[
                ("53a9e0f6.s", "stop", "main", "b14d1330"),
                ("35678402.p", "prompt", "main", "b14d1330"),
                ("247bb874.s", "stop", "main", "b14d1330"),
            ])
        );
        assert_eq!(
            events(none).iter().map(|e| row(e)).collect::<Vec<_>>(),
            table(&[("25e03919.p", "prompt", "main", "6abf0af7")])
        );
        let first = events(alpha)[4];
        assert!(
            matches!(&first.record.content, Content::Prompt { text: Some(t) } if t == "Add a retry flag to the fetch command.")
        );
        assert_eq!(first.record.cwd.as_deref(), Some("/work/fixture/alpha"));
        assert_eq!(first.record.harness_version, None);
        assert_eq!(first.seq, 120_000);
        let mut skipped = BTreeMap::new();
        for (_, mapped) in &slices {
            for (reason, n) in &mapped.skipped {
                *skipped.entry(*reason).or_insert(0) += n;
            }
        }
        assert_eq!(
            skipped,
            BTreeMap::from([
                ("empty message", 1),
                ("incomplete row", 1),
                ("interrupt", 1),
                ("interrupted turn", 1),
                ("narration", 2),
                ("notification", 1),
            ])
        );
        assert_eq!(
            (alpha.lines, beta.lines, none.lines, alpha.malformed),
            (17, 3, 3, 0)
        );
    }

    /// Each fixture session's stops, by turn: a turn's last assistant message, unless an
    /// interrupt marker ends it; the subagent's in its main session.
    #[test]
    fn a_completed_turn_gives_one_stop() {
        let slices = read_all(&store());
        let mut got: Vec<(String, String)> = slices
            .iter()
            .flat_map(|(_, m)| events(m))
            .filter(|e| e.record.content.kind().as_str() == "stop")
            .map(|e| {
                let (id, _, actor, session) = row(e);
                let message = match &e.record.content {
                    Content::Stop {
                        outcome: StopOutcome::Completed,
                        message: Some(m),
                    } => m.clone(),
                    other => panic!("{other:?}"),
                };
                (format!("{session} {actor} {id}"), message)
            })
            .collect();
        let want = [
            (
                "eef5665a main 4fc05562.s",
                "The fetch command now takes a retry flag.",
            ),
            ("eef5665a main 9656039c.s", "The build finished cleanly."),
            (
                "eef5665a main 75d52698.s",
                "Stopped. Waiting for directions.",
            ),
            (
                "eef5665a main 27cbe3e2.s",
                "Added a short section with an example.",
            ),
            (
                "eef5665a subagent a5b6c7d 87537b77.s",
                "Three callers found.",
            ),
            (
                "eef5665a subagent a5b6c7d cdc6c2ea.s",
                "They sit in the cli module.",
            ),
            (
                "b14d1330 main 53a9e0f6.s",
                "Resuming from the earlier summary.",
            ),
            ("b14d1330 main 247bb874.s", "All tests pass."),
        ];
        got.sort();
        let mut want: Vec<(String, String)> = want
            .iter()
            .map(|(w, m)| ((*w).to_owned(), (*m).to_owned()))
            .collect();
        want.sort();
        assert_eq!(got, want);
        let interrupted: usize = slices
            .iter()
            .filter_map(|(_, m)| m.skipped.get("interrupted turn"))
            .sum();
        assert_eq!(interrupted, 1);
    }
}
