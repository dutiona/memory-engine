//! Claude Code transcripts (#1026), read into neutral records keyed for the backfill route.
//!
//! A session file, or one of its `subagents/agent-<id>.jsonl` files, is read line by line as JSON
//! objects, by field name.
//!
//! A field of an unexpected type drops that field, never the line: a harvest spans many Claude
//! Code versions. What the reader makes of each entry is the table of the fixture README
//! (`tests/fixtures/claude-code-transcripts/README.md`).

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use memory_engine::bootstrap::{parse_timestamp, read_json_lines};
use memory_engine::types::lifecycle::{
    Actor, Content, ImportEvent, ImportItem, NeutralRecord, StopOutcome, ToolOutcome,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{result_text, tool_content};
use crate::adapters::Mapped;

/// The text Claude Code puts at the start of what an interruption leaves.
const INTERRUPTED: &str = "[Request interrupted by user";

/// The model of the messages Claude Code writes itself.
const SYNTHETIC: &str = "<synthetic>";

/// Why a transcript of more entries than ADR-0009's importer keeps is refused whole.
const OVER_THE_LIMIT: &str = "the transcript is over the 1,000,000-entry limit";

/// The records one millisecond may hold: `seq` is the millisecond times this plus the rank,
/// so it stays within ADR-0020's six digits.
const PER_MILLISECOND: u32 = 1000;

/// `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, ADR-0020 §3's event ID.
fn is_event_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

fn flag(object: &Map<String, Value>, name: &str) -> bool {
    object.get(name).and_then(Value::as_bool) == Some(true)
}

/// The members every imported entry needs.
struct Entry<'a> {
    object: &'a Map<String, Value>,
    uuid: &'a str,
    session: &'a str,
    time: DateTime<Utc>,
    actor: Actor,
}

impl Entry<'_> {
    /// The record this entry makes, as of its own time and fields.
    fn record(&self, content: Content) -> NeutralRecord {
        NeutralRecord {
            session_id: self.session.to_owned(),
            actor: self.actor.clone(),
            cwd: text(self.object, "cwd").map(str::to_owned),
            harness_version: text(self.object, "version").map(str::to_owned),
            content,
        }
    }

    /// The turn this entry belongs to: its session and actor.
    fn turn(&self) -> (String, Option<String>) {
        let actor = match &self.actor {
            Actor::Main => None,
            Actor::Subagent { id, .. } => Some(id.clone().unwrap_or_default()),
        };
        (self.session.to_owned(), actor)
    }
}

/// A `user` or `assistant` entry with a `uuid`, a `sessionId` and a `timestamp`, else why not.
fn entry(object: &Map<String, Value>) -> Result<Entry<'_>, &'static str> {
    let skipped = match text(object, "type") {
        Some("user" | "assistant") => None,
        Some("system") => Some("system"),
        Some("attachment") => Some("attachment"),
        Some("progress") => Some("progress"),
        Some("queue-operation") => Some("queue-operation"),
        Some("file-history-snapshot") => Some("file-history-snapshot"),
        Some("last-prompt") => Some("last-prompt"),
        _ => Some("unknown type"),
    };
    if let Some(reason) = skipped {
        return Err(reason);
    }
    let missing = "no uuid, sessionId or timestamp";
    let uuid = text(object, "uuid")
        .filter(|u| !u.is_empty())
        .ok_or(missing)?;
    let session = text(object, "sessionId")
        .filter(|s| !s.is_empty())
        .ok_or(missing)?;
    let time = text(object, "timestamp")
        .and_then(parse_timestamp)
        .ok_or(missing)?;
    if !is_event_id(&format!("bf.{uuid}.t0")) {
        return Err("a uuid off the event-ID grammar");
    }
    let actor = if flag(object, "isSidechain") {
        Actor::Subagent {
            id: text(object, "agentId").map(str::to_owned),
            kind: None,
        }
    } else {
        Actor::Main
    };
    Ok(Entry {
        object,
        uuid,
        session,
        time,
        actor,
    })
}

fn message(object: &Map<String, Value>) -> Option<&Map<String, Value>> {
    object.get("message").and_then(Value::as_object)
}

/// A message's content as blocks: a string is one text block.
fn blocks(object: &Map<String, Value>) -> Vec<Value> {
    match message(object).and_then(|m| m.get("content")) {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type").and_then(Value::as_str)
}

/// The text blocks of `blocks`, joined by line breaks; `None` when there is none.
fn joined(blocks: &[Value]) -> Option<String> {
    let texts: Vec<&str> = blocks
        .iter()
        .filter(|b| block_type(b) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// What the message an assistant entry belongs to comes to, over all its entries.
#[derive(Default)]
struct Message {
    /// The index of its last entry, in file order.
    last: usize,
    /// Whether one of its entries has `stop_reason` `end_turn` or `stop_sequence`.
    completed: bool,
    texts: Vec<String>,
}

/// A `tool_use` block waiting for its result.
struct Call {
    uuid: String,
    k: usize,
    tool: String,
    input: Value,
}

/// A record, in file order, before its order key.
struct Raw {
    line: usize,
    time: DateTime<Utc>,
    event_id: String,
    record: NeutralRecord,
}

/// The message an assistant entry belongs to: its session, its actor, and its `message.id`,
/// else the entry itself. A fork repeats a message under its own session, so each session's
/// copy is its own message and maps to the original's record, never to one text of both.
type MessageKey = ((String, Option<String>), String);

fn message_key(entry: &Entry<'_>) -> MessageKey {
    let id = message(entry.object)
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .map_or_else(|| format!("entry {}", entry.uuid), str::to_owned);
    (entry.turn(), id)
}

fn synthetic(object: &Map<String, Value>) -> bool {
    message(object)
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        == Some(SYNTHETIC)
}

/// A message's entries, by message key: where it ends and whether it completed.
fn messages(objects: &[(usize, Value)]) -> HashMap<MessageKey, Message> {
    let mut messages: HashMap<MessageKey, Message> = HashMap::new();
    for (index, (_, value)) in objects.iter().enumerate() {
        let Some(entry) = value.as_object().and_then(|o| entry(o).ok()) else {
            continue;
        };
        if text(entry.object, "type") != Some("assistant")
            || flag(entry.object, "isApiErrorMessage")
            || synthetic(entry.object)
        {
            continue;
        }
        let message_entry = messages.entry(message_key(&entry)).or_default();
        message_entry.last = index;
        let stop = message(entry.object)
            .and_then(|m| m.get("stop_reason"))
            .and_then(Value::as_str);
        message_entry.completed |= matches!(stop, Some("end_turn" | "stop_sequence"));
    }
    messages
}

/// Read a Claude Code transcript into neutral records keyed for the backfill route (#1026).
///
/// Every prompt, finished tool call, completed final message, API error and compaction summary
/// becomes one [`ImportEvent`], in file order; everything else is counted in the census by
/// reason. A record's event ID is `bf.<entry uuid>.<tag>` (`p`, `s`, `c`, or `t<k>` for the
/// `k`-th `tool_use` block of an entry, from 0), and its digest the SHA-256 of its canonical
/// JSON without its session ID and working directory, so a forked session repeating an entry
/// meets it as a duplicate. Its order key is its entry's time to the second, and
/// `seq = millisecond × 1,000 + rank`, the rank in file order among the file's records of that
/// millisecond: the 1,001st record of a millisecond is refused and counted.
///
/// # Errors
///
/// `"no session"` when no line is an object with a `sessionId`; a fixed phrase naming the entry
/// limit when the body holds more than ADR-0009's 1,000,000 entries, so no prefix of it is
/// imported.
///
/// # Examples
///
/// ```
/// use memory_engine::types::lifecycle::{Content, ImportItem};
/// use memory_engine_mcp::adapters::claude_code::transcript::read;
///
/// let body = br#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-03-19T10:00:00.120Z","message":{"content":"Add a flag."}}
/// {"type":"progress","uuid":"u2","sessionId":"s1"}
/// "#;
/// let mapped = read(body)?;
/// let ImportItem::Event(prompt) = &mapped.items[0] else { panic!("an event") };
/// assert_eq!((prompt.event_id.as_str(), prompt.seq), ("bf.u1.p", 120_000));
/// assert!(matches!(&prompt.record.content, Content::Prompt { text: Some(t) } if t == "Add a flag."));
/// assert_eq!(mapped.skipped.get("progress"), Some(&1));
/// assert_eq!(read(b"[1]\n").unwrap_err(), "no session");
/// # Ok::<(), &'static str>(())
/// ```
pub fn read(body: &[u8]) -> Result<Mapped, &'static str> {
    read_capped(
        body,
        memory_engine::bootstrap::BootstrapConfig::default().max_entries,
    )
}

/// [`read`] with an explicit entry limit (test seam).
fn read_capped(body: &[u8], max_entries: usize) -> Result<Mapped, &'static str> {
    let (objects, malformed, truncated) =
        read_json_lines(body, memory_engine::BACKFILL_BODY_BYTES as u64, max_entries);
    if truncated {
        return Err(OVER_THE_LIMIT);
    }
    if !objects
        .iter()
        .any(|(_, v)| v.get("sessionId").is_some_and(Value::is_string))
    {
        return Err("no session");
    }
    let mut reader = Reader {
        messages: messages(&objects),
        ..Reader::default()
    };
    for (index, (line, value)) in objects.iter().enumerate() {
        if let Some(object) = value.as_object() {
            reader.entry(index, *line, object);
        }
    }
    let mut mapped = Mapped {
        items: Vec::new(),
        lines: objects.len() + malformed,
        malformed,
        pending: reader.calls.len(),
        incomplete_turns: reader.incomplete + reader.turns.len(),
        skipped: BTreeMap::new(),
    };
    let mut ranks: HashMap<(i64, u32), u32> = HashMap::new();
    for raw in reader.records {
        let millisecond = raw.time.timestamp_subsec_millis().min(PER_MILLISECOND - 1);
        let rank = ranks
            .entry((raw.time.timestamp(), millisecond))
            .or_default();
        if *rank >= PER_MILLISECOND {
            *reader
                .skipped
                .entry("over 1,000 records in one millisecond")
                .or_default() += 1;
            continue;
        }
        let seq = millisecond * PER_MILLISECOND + *rank;
        *rank += 1;
        let canonical = canonical(&raw).to_string();
        mapped.items.push(ImportItem::Event(ImportEvent {
            event_id: raw.event_id,
            time: raw.time.with_nanosecond(0).unwrap_or(raw.time),
            seq,
            body_sha256: Sha256::digest(canonical.as_bytes()).into(),
            body_size: canonical.len() as u64,
            line: raw.line,
            record: raw.record,
        }));
    }
    mapped.skipped = reader.skipped;
    Ok(mapped)
}

/// The walk over a file's entries, in file order.
#[derive(Default)]
struct Reader {
    messages: HashMap<MessageKey, Message>,
    calls: HashMap<String, Call>,
    /// The open turns, by session and actor.
    turns: HashSet<(String, Option<String>)>,
    incomplete: usize,
    /// The trigger of the latest `compact_boundary`, for the summary that follows it.
    trigger: Option<String>,
    records: Vec<Raw>,
    skipped: BTreeMap<&'static str, usize>,
}

impl Reader {
    fn skip(&mut self, reason: &'static str) {
        *self.skipped.entry(reason).or_default() += 1;
    }

    fn push(&mut self, line: usize, entry: &Entry<'_>, tag: &str, content: Content) {
        self.records.push(Raw {
            line,
            time: entry.time,
            event_id: format!("bf.{}.{tag}", entry.uuid),
            record: entry.record(content),
        });
    }

    fn entry(&mut self, index: usize, line: usize, object: &Map<String, Value>) {
        if text(object, "type") == Some("system")
            && text(object, "subtype") == Some("compact_boundary")
        {
            self.trigger = object
                .get("compactMetadata")
                .and_then(|m| m.get("trigger"))
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        let entry = match entry(object) {
            Ok(entry) => entry,
            Err(reason) => return self.skip(reason),
        };
        if text(object, "type") == Some("user") {
            self.user(line, &entry);
        } else {
            self.assistant(index, line, &entry);
        }
    }

    fn user(&mut self, line: usize, entry: &Entry<'_>) {
        let blocks = blocks(entry.object);
        if flag(entry.object, "isCompactSummary") {
            let summary = joined(&blocks).unwrap_or_default();
            let trigger = self.trigger.take();
            return self.push(line, entry, "c", Content::Compact { trigger, summary });
        }
        if flag(entry.object, "isMeta") {
            return self.skip("meta");
        }
        for _ in blocks.iter().filter(|b| block_type(b) == Some("image")) {
            self.skip("image");
        }
        let results: Vec<&Value> = blocks
            .iter()
            .filter(|b| block_type(b) == Some("tool_result"))
            .collect();
        if !results.is_empty() {
            for block in results {
                self.result(line, entry, block);
            }
            return;
        }
        let Some(text) = joined(&blocks) else {
            return;
        };
        if text.starts_with(INTERRUPTED) {
            return self.skip("interrupt");
        }
        if !self.turns.insert(entry.turn()) {
            self.incomplete += 1;
        }
        self.push(line, entry, "p", Content::Prompt { text: Some(text) });
    }

    /// A `tool_result` block: the record of the call it names, at this entry.
    fn result(&mut self, line: usize, entry: &Entry<'_>, block: &Value) {
        let Some(call) = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .and_then(|id| self.calls.remove(id))
        else {
            return self.skip("orphan tool_result");
        };
        let tool_use_result = entry.object.get("toolUseResult");
        let block_text = block.get("content").and_then(result_text);
        let result = tool_use_result
            .and_then(result_text)
            .or_else(|| block_text.clone());
        let interrupted = [&result, &block_text]
            .iter()
            .any(|t| t.as_deref().is_some_and(|t| t.starts_with(INTERRUPTED)))
            || tool_use_result.is_some_and(|r| r.get("interrupted") == Some(&Value::Bool(true)));
        let outcome = if interrupted {
            ToolOutcome::Interrupted
        } else if block.get("is_error") == Some(&Value::Bool(true)) {
            ToolOutcome::Failure
        } else {
            ToolOutcome::Success
        };
        self.records.push(Raw {
            line,
            time: entry.time,
            event_id: format!("bf.{}.t{}", call.uuid, call.k),
            record: entry.record(tool_content(call.tool, call.input, result, outcome)),
        });
    }

    fn assistant(&mut self, index: usize, line: usize, entry: &Entry<'_>) {
        let blocks = blocks(entry.object);
        if flag(entry.object, "isApiErrorMessage") {
            self.turns.remove(&entry.turn());
            let message = Some(joined(&blocks).unwrap_or_default());
            return self.push(
                line,
                entry,
                "s",
                Content::Stop {
                    outcome: StopOutcome::ApiError,
                    message,
                },
            );
        }
        if synthetic(entry.object) {
            return self.skip("synthetic");
        }
        let key = message_key(entry);
        let mut k = 0;
        for block in &blocks {
            match block_type(block) {
                Some("text") => {
                    if let (Some(text), Some(message)) = (
                        block.get("text").and_then(Value::as_str),
                        self.messages.get_mut(&key),
                    ) {
                        message.texts.push(text.to_owned());
                    }
                }
                Some("thinking" | "redacted_thinking") => self.skip("thinking"),
                Some("image") => self.skip("image"),
                Some("tool_use") => {
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        let tool = block.get("name").and_then(Value::as_str).unwrap_or("");
                        self.calls.insert(
                            id.to_owned(),
                            Call {
                                uuid: entry.uuid.to_owned(),
                                k,
                                tool: tool.to_owned(),
                                input: block.get("input").cloned().unwrap_or(Value::Null),
                            },
                        );
                    }
                    k += 1;
                }
                _ => {}
            }
        }
        let Some(message) = self.messages.get(&key) else {
            return;
        };
        if message.last == index && message.completed {
            let text = message.texts.join("\n");
            self.turns.remove(&entry.turn());
            self.push(
                line,
                entry,
                "s",
                Content::Stop {
                    outcome: StopOutcome::Completed,
                    message: Some(text),
                },
            );
        }
    }
}

/// A record as canonical JSON (`serde_json` sorts object keys), without its session ID and
/// working directory: what its digest is taken over.
fn canonical(raw: &Raw) -> Value {
    let actor = match &raw.record.actor {
        Actor::Main => json!("main"),
        Actor::Subagent { id, kind } => json!({ "subagent": id, "kind": kind }),
    };
    let content = match &raw.record.content {
        Content::Prompt { text } => json!({ "prompt": text }),
        Content::ToolUse {
            tool,
            input,
            result,
            outcome,
            ..
        } => json!({
            "tool": tool, "input": input, "result": result, "outcome": format!("{outcome:?}"),
        }),
        Content::Stop { outcome, message } => {
            json!({ "stop": format!("{outcome:?}"), "message": message })
        }
        Content::Compact { trigger, summary } => {
            json!({ "compact": summary, "trigger": trigger })
        }
        Content::SessionStart { source } => json!({ "session_start": source }),
        Content::SessionEnd { reason } => json!({ "session_end": reason }),
    };
    json!({
        "time": raw.time.to_rfc3339_opts(SecondsFormat::Millis, true),
        "actor": actor,
        "harness_version": raw.record.harness_version,
        "content": content,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use memory_engine::types::lifecycle::{Actor, Content, ImportEvent, ImportItem};
    use serde_json::{Value, json};

    use super::*;

    const SESSION: &str = "52c6c7b7-c890-43ec-81a5-a1cf99d3cc84";
    const FORK: &str = "23373885-a3be-4158-a1ab-d7e505bf6c04";

    fn fixture(path: &str) -> Vec<u8> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude-code-transcripts/-work-fixture-p");
        std::fs::read(dir.join(path)).unwrap()
    }

    fn main_file() -> Vec<u8> {
        fixture(&format!("{SESSION}.jsonl"))
    }

    fn subagent_file() -> Vec<u8> {
        fixture(&format!("{SESSION}/subagents/agent-a3c91f2.jsonl"))
    }

    /// The first `n` lines of `body`.
    fn head(body: &[u8], n: usize) -> Vec<u8> {
        let text = std::str::from_utf8(body).unwrap();
        let mut out: String = text.lines().take(n).collect::<Vec<_>>().join("\n");
        out.push('\n');
        out.into_bytes()
    }

    fn events(mapped: &Mapped) -> Vec<&ImportEvent> {
        mapped
            .items
            .iter()
            .map(|item| match item {
                ImportItem::Event(event) => event,
                ImportItem::Memory(_) => panic!("a transcript holds no memory"),
            })
            .collect()
    }

    fn event_at(mapped: &Mapped, line: usize) -> &ImportEvent {
        events(mapped)
            .into_iter()
            .find(|e| e.line == line)
            .unwrap_or_else(|| panic!("no record at line {line}"))
    }

    /// Each record's line, kind and what tells it apart: the tool and outcome of a call, the
    /// outcome of a stop, the trigger of a compaction.
    fn table(mapped: &Mapped) -> Vec<(usize, &'static str, String)> {
        events(mapped)
            .into_iter()
            .map(|e| {
                let what = match &e.record.content {
                    Content::ToolUse { tool, outcome, .. } => format!("{tool} {outcome:?}"),
                    Content::Stop { outcome, .. } => format!("{outcome:?}"),
                    Content::Compact { trigger, .. } => trigger.clone().unwrap_or_default(),
                    _ => String::new(),
                };
                (e.line, e.record.content.kind().as_str(), what)
            })
            .collect()
    }

    fn line(value: &Value) -> String {
        value.to_string()
    }

    /// An entry of `session` at `time` with `uuid`, its other members `rest`.
    fn entry(kind: &str, uuid: &str, time: &str, rest: &Value) -> String {
        let mut entry = json!({
            "type": kind, "uuid": uuid, "timestamp": time, "sessionId": "s",
            "cwd": "/work/p", "version": "2.1.310", "isSidechain": false,
        });
        entry
            .as_object_mut()
            .unwrap()
            .extend(rest.as_object().unwrap().clone());
        line(&entry)
    }

    fn user_text(uuid: &str, time: &str, text: &str) -> String {
        entry(
            "user",
            uuid,
            time,
            &json!({ "message": { "role": "user", "content": text } }),
        )
    }

    fn assistant(uuid: &str, time: &str, id: &str, blocks: &Value, stop: Option<&str>) -> String {
        entry(
            "assistant",
            uuid,
            time,
            &json!({ "message": {
                "id": id, "role": "assistant", "model": "claude-sonnet-4-5",
                "content": blocks, "stop_reason": stop,
            } }),
        )
    }

    fn result(uuid: &str, time: &str, tool_use_id: &str, text: &str, is_error: bool) -> String {
        entry(
            "user",
            uuid,
            time,
            &json!({ "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": tool_use_id, "content": text, "is_error": is_error },
            ] } }),
        )
    }

    fn body(lines: &[String]) -> Vec<u8> {
        let mut text = lines.join("\n");
        text.push('\n');
        text.into_bytes()
    }

    #[test]
    #[allow(clippy::too_many_lines)] // the README's table, row by row, then the rows' details
    fn the_transcript_fixtures_map_by_the_readme_table() {
        let mapped = read(&main_file()).unwrap();
        let rows = [
            (2, "prompt", ""),
            (6, "tool_use", "Bash Success"),
            (8, "tool_use", "Read Failure"),
            (10, "tool_use", "Read Success"),
            (12, "tool_use", "mcp__me__recall Success"),
            (14, "stop", "Completed"),
            (18, "prompt", ""),
            (20, "tool_use", "Bash Interrupted"),
            (22, "prompt", ""),
            (24, "tool_use", "Grep Success"),
            (25, "prompt", ""),
            (26, "stop", "ApiError"),
            (27, "prompt", ""),
            (29, "stop", "Completed"),
            (32, "compact", "manual"),
            (34, "prompt", ""),
            (35, "stop", "Completed"),
            (36, "prompt", ""),
            (43, "prompt", ""),
            (45, "tool_use", "Task Success"),
            (46, "stop", "Completed"),
            (47, "prompt", ""),
        ];
        let want: Vec<(usize, &str, String)> =
            rows.iter().map(|&(l, k, w)| (l, k, w.to_owned())).collect();
        assert_eq!(table(&mapped), want);

        assert_eq!((mapped.lines, mapped.malformed), (48, 1));
        let skipped: Vec<(&str, usize)> = mapped.skipped.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(
            skipped,
            [
                ("attachment", 1),
                ("file-history-snapshot", 1),
                ("image", 1),
                ("interrupt", 1),
                ("last-prompt", 1),
                ("meta", 1),
                ("no uuid, sessionId or timestamp", 1),
                ("orphan tool_result", 1),
                ("progress", 1),
                ("queue-operation", 1),
                ("synthetic", 1),
                ("system", 2),
                ("thinking", 2),
                ("unknown type", 1),
            ]
        );
        assert_eq!((mapped.pending, mapped.incomplete_turns), (1, 4));

        assert!(
            events(&mapped)
                .iter()
                .all(|e| e.record.actor == Actor::Main)
        );
        let version = |l| event_at(&mapped, l).record.harness_version.as_deref();
        assert_eq!(
            (version(2), version(25)),
            (Some("2.1.296"), Some("2.1.310"))
        );
        let first = event_at(&mapped, 2);
        assert_eq!(first.event_id, "bf.b241ff2b-30f3-45c1-8a6d-1d32a35e3b4f.p");
        assert_eq!(first.record.cwd.as_deref(), Some("/work/fixture/p"));
        assert_eq!(
            event_at(&mapped, 6).event_id,
            "bf.50f12489-8453-4bab-b851-2190862d1797.t0",
            "a call is keyed by its tool_use block"
        );
        let Content::Stop { message, .. } = &event_at(&mapped, 14).record.content else {
            panic!("a stop");
        };
        assert_eq!(
            message.as_deref(),
            Some("Added the --verbose flag to src/cli.rs.\nThe help text lists it.")
        );
        assert_eq!(
            event_at(&mapped, 14).event_id,
            "bf.06d823fe-bf99-4fb4-828b-4e7f0b84465a.s",
            "a stop is keyed by its message's last entry"
        );
        let Content::Stop { message, .. } = &event_at(&mapped, 26).record.content else {
            panic!("an API error");
        };
        assert_eq!(message.as_deref(), Some("API Error: Connection error."));
        let Content::ToolUse { call, .. } = &event_at(&mapped, 12).record.content else {
            panic!("a call");
        };
        assert_eq!(call.as_deref(), Some("518b327349710550"));
        let Content::ToolUse { paths, result, .. } = &event_at(&mapped, 10).record.content else {
            panic!("a call");
        };
        assert!(
            paths.contains(&"/work/fixture/p/.env".to_owned()),
            "{paths:?}"
        );
        assert!(
            result
                .as_deref()
                .is_some_and(|r| r.contains("FIXTURE_MODE"))
        );
        let Content::ToolUse { result, .. } = &event_at(&mapped, 8).record.content else {
            panic!("a call");
        };
        assert_eq!(result.as_deref(), Some("Error: File does not exist."));
        let Content::Compact { summary, .. } = &event_at(&mapped, 32).record.content else {
            panic!("a compaction");
        };
        assert!(summary.starts_with("This session is being continued"));
        let Content::Prompt { text } = &event_at(&mapped, 34).record.content else {
            panic!("a prompt");
        };
        assert_eq!(text.as_deref(), Some("What does this screenshot show?"));

        let fork = event_at(&mapped, 36);
        assert_eq!(fork.record.session_id, FORK);
        assert_eq!(
            (fork.event_id.as_str(), fork.body_sha256),
            (first.event_id.as_str(), first.body_sha256),
            "a repeated entry in another session keeps its key and digest"
        );
    }

    #[test]
    fn results_pair_by_tool_use_id() {
        let calls = json!([
            { "type": "tool_use", "id": "toolu_a", "name": "Read", "input": { "file_path": "a" } },
            { "type": "tool_use", "id": "toolu_b", "name": "Grep", "input": { "pattern": "b" } },
        ]);
        let mapped = read(&body(&[
            user_text("u1", "2026-03-19T10:00:00.000Z", "go"),
            assistant(
                "u2",
                "2026-03-19T10:00:01.000Z",
                "m1",
                &calls,
                Some("tool_use"),
            ),
            result(
                "u3",
                "2026-03-19T10:00:02.000Z",
                "toolu_b",
                "b found",
                false,
            ),
            result(
                "u4",
                "2026-03-19T10:00:03.000Z",
                "toolu_a",
                "a failed",
                true,
            ),
        ]))
        .unwrap();

        let calls: Vec<(String, String, String)> = events(&mapped)
            .into_iter()
            .filter_map(|e| match &e.record.content {
                Content::ToolUse {
                    tool,
                    result,
                    outcome,
                    ..
                } => Some((
                    e.event_id.clone(),
                    format!("{tool} {outcome:?}"),
                    result.clone().unwrap_or_default(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            [
                ("bf.u2.t1".into(), "Grep Success".into(), "b found".into()),
                ("bf.u2.t0".into(), "Read Failure".into(), "a failed".into()),
            ]
        );
    }

    #[test]
    fn a_stop_needs_a_completed_message() {
        let file = main_file();
        // Cut inside line 14's message: its first part is there, its `end_turn` is not.
        let cut = read(&head(&file, 13)).unwrap();
        assert!(
            table(&cut).iter().all(|(_, kind, _)| *kind != "stop"),
            "{:?}",
            table(&cut)
        );
        assert_eq!(cut.incomplete_turns, 1, "a copy taken mid-turn");
        let grown = read(&file).unwrap();
        assert_eq!(table(&grown)[5], (14, "stop", "Completed".to_owned()));

        // Line 18's turn was interrupted, line 22's ended in `tool_use`: no stop for either.
        let turns = read(&head(&file, 25)).unwrap();
        let stops: Vec<usize> = table(&turns)
            .into_iter()
            .filter(|(_, kind, _)| *kind == "stop")
            .map(|(l, _, _)| l)
            .collect();
        assert_eq!(stops, [14]);
        assert_eq!(
            turns.incomplete_turns, 3,
            "18 and 22, and 25 cut before its stop"
        );

        // A turn that ends in a message whose last `stop_reason` is `tool_use` is not complete,
        // whatever text came before.
        let text = json!([{ "type": "text", "text": "running it" }]);
        let tool = json!([{ "type": "tool_use", "id": "t", "name": "Bash", "input": {} }]);
        let mapped = read(&body(&[
            user_text("u1", "2026-03-19T10:00:00.000Z", "go"),
            assistant("u2", "2026-03-19T10:00:01.000Z", "m1", &text, None),
            assistant(
                "u3",
                "2026-03-19T10:00:01.500Z",
                "m1",
                &tool,
                Some("tool_use"),
            ),
        ]))
        .unwrap();
        assert_eq!(table(&mapped).len(), 1, "the prompt only");
        assert_eq!((mapped.incomplete_turns, mapped.pending), (1, 1));
    }

    #[test]
    fn meta_synthetic_and_interrupt_entries_are_skipped() {
        let synthetic = entry(
            "assistant",
            "u3",
            "2026-03-19T10:00:02.000Z",
            &json!({ "message": {
                "id": "m2", "model": "<synthetic>", "role": "assistant",
                "content": [{ "type": "text", "text": "No response requested." }],
                "stop_reason": "stop_sequence",
            } }),
        );
        let meta = entry(
            "user",
            "u2",
            "2026-03-19T10:00:01.000Z",
            &json!({ "isMeta": true, "message": { "role": "user", "content": "Caveat: local commands." } }),
        );
        let mapped = read(&body(&[
            user_text("u1", "2026-03-19T10:00:00.000Z", "a prompt"),
            meta,
            synthetic,
            user_text(
                "u4",
                "2026-03-19T10:00:03.000Z",
                "[Request interrupted by user]",
            ),
        ]))
        .unwrap();

        assert_eq!(table(&mapped), [(1, "prompt", String::new())]);
        let skipped: Vec<(&str, usize)> = mapped.skipped.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(skipped, [("interrupt", 1), ("meta", 1), ("synthetic", 1)]);
    }

    #[test]
    fn an_unpaired_tool_use_is_pending() {
        let file = main_file();
        assert_eq!(read(&file).unwrap().pending, 1, "line 48's call");
        let cut = read(&head(&file, 7)).unwrap();
        assert_eq!(cut.pending, 1, "line 7's call, its result not copied yet");
        assert!(
            !table(&cut).contains(&(8, "tool_use", "Read Failure".to_owned())),
            "a call is recorded once it finished"
        );
        let grown = read(&head(&file, 8)).unwrap();
        assert_eq!(grown.pending, 0);
        assert!(table(&grown).contains(&(8, "tool_use", "Read Failure".to_owned())));
    }

    #[test]
    fn ids_and_digests_are_stable_across_a_grown_copy() {
        let file = main_file();
        let keys = |mapped: &Mapped| {
            events(mapped)
                .into_iter()
                .map(|e| (e.event_id.clone(), e.time, e.seq, e.body_sha256))
                .collect::<Vec<_>>()
        };
        let short = keys(&read(&head(&file, 30)).unwrap());
        let long = keys(&read(&file).unwrap());
        assert!(short.len() > 10);
        for key in &short {
            assert!(long.contains(key), "{key:?}");
        }
        assert_eq!(keys(&read(&file).unwrap()), long, "reading twice");
    }

    #[test]
    fn seq_keeps_file_order_within_a_millisecond() {
        let mapped = read(&body(&[
            user_text("u1", "2026-03-19T10:00:00.500Z", "first"),
            user_text("u2", "2026-03-19T10:00:00.500Z", "second"),
            user_text("u3", "2026-03-19T10:00:01Z", "no milliseconds"),
            user_text("u4", "2026-03-19T10:00:01Z", "no milliseconds either"),
        ]))
        .unwrap();
        let seqs: Vec<(String, u32)> = events(&mapped)
            .into_iter()
            .map(|e| (e.time.to_rfc3339(), e.seq))
            .collect();
        assert_eq!(
            seqs,
            [
                ("2026-03-19T10:00:00+00:00".to_owned(), 500_000),
                ("2026-03-19T10:00:00+00:00".to_owned(), 500_001),
                ("2026-03-19T10:00:01+00:00".to_owned(), 0),
                ("2026-03-19T10:00:01+00:00".to_owned(), 1),
            ]
        );

        // A parent and its subagent in one second keep their real order, read from either file.
        let parent = read(&main_file()).unwrap();
        let child = read(&subagent_file()).unwrap();
        let (p, c) = (event_at(&parent, 43), event_at(&child, 1));
        assert_eq!(p.time, c.time);
        assert!(p.seq < c.seq, "{} < {}", p.seq, c.seq);

        // One limit for every precision: the 1,001st record of a millisecond is refused.
        let mut lines: Vec<String> = (0..1000)
            .map(|i| user_text(&format!("w{i}"), "2026-03-19T10:00:00Z", "whole"))
            .collect();
        lines.insert(
            500,
            user_text("ms", "2026-03-19T10:00:00.001Z", "a millisecond"),
        );
        lines.push(user_text(
            "over",
            "2026-03-19T10:00:00.000Z",
            "one too many",
        ));
        let mapped = read(&body(&lines)).unwrap();
        assert_eq!(events(&mapped).len(), 1001);
        assert_eq!(
            mapped.skipped.get("over 1,000 records in one millisecond"),
            Some(&1)
        );
        let seq = |id: &str| {
            events(&mapped)
                .into_iter()
                .find(|e| e.event_id == id)
                .map(|e| e.seq)
        };
        assert_eq!(seq("bf.w999.p"), Some(999));
        assert_eq!(
            seq("bf.ms.p"),
            Some(1000),
            "after every whole-second record"
        );
        assert_eq!(seq("bf.over.p"), None);
    }

    /// A fork repeats an assistant message under its own session: each session's copy is its own
    /// message, so both map to the original's event ID, text and digest, never one doubled text.
    #[test]
    fn a_forks_repeated_message_is_the_originals() {
        let one = json!([{ "type": "text", "text": "part one" }]);
        let two = json!([{ "type": "text", "text": "part two" }]);
        let session = |line: String, id: &str| {
            let mut value: Value = serde_json::from_str(&line).unwrap();
            value["sessionId"] = json!(id);
            value.to_string()
        };
        let original = [
            user_text("u1", "2026-03-19T10:00:00.000Z", "a prompt"),
            assistant("a1", "2026-03-19T10:00:01.000Z", "m1", &one, None),
            assistant(
                "a2",
                "2026-03-19T10:00:02.000Z",
                "m1",
                &two,
                Some("end_turn"),
            ),
        ];
        let mut lines: Vec<String> = original.to_vec();
        lines.extend(original.iter().map(|l| session(l.clone(), "a-fork")));

        let mapped = read(&body(&lines)).unwrap();

        let stops: Vec<&ImportEvent> = events(&mapped)
            .into_iter()
            .filter(|e| e.record.content.kind().as_str() == "stop")
            .collect();
        assert_eq!(stops.len(), 2, "one per session");
        for stop in &stops {
            assert_eq!(stop.event_id, "bf.a2.s");
            let Content::Stop { message, .. } = &stop.record.content else {
                panic!("a stop");
            };
            assert_eq!(message.as_deref(), Some("part one\npart two"));
        }
        assert_eq!(stops[0].body_sha256, stops[1].body_sha256);
        let alone = read(&body(&original)).unwrap();
        let first = events(&alone)
            .into_iter()
            .find(|e| e.event_id == "bf.a2.s")
            .unwrap();
        assert_eq!(
            first.body_sha256, stops[0].body_sha256,
            "the original's digest"
        );
    }

    #[test]
    fn thinking_is_not_captured() {
        let blocks = json!([
            { "type": "thinking", "thinking": "a private chain of thought" },
            { "type": "redacted_thinking", "data": "opaque" },
            { "type": "text", "text": "the answer" },
        ]);
        let mapped = read(&body(&[
            user_text("u1", "2026-03-19T10:00:00.000Z", "a question"),
            assistant(
                "u2",
                "2026-03-19T10:00:01.000Z",
                "m1",
                &blocks,
                Some("end_turn"),
            ),
        ]))
        .unwrap();
        let Content::Stop { message, .. } = &event_at(&mapped, 2).record.content else {
            panic!("a stop");
        };
        assert_eq!(message.as_deref(), Some("the answer"));
        assert_eq!(mapped.skipped.get("thinking"), Some(&2));
        assert!(
            !format!("{:?}", mapped.items).contains("chain of thought"),
            "no record holds the thinking"
        );
    }

    #[test]
    fn a_subagent_file_is_the_subagents() {
        let mapped = read(&subagent_file()).unwrap();
        assert_eq!(
            table(&mapped),
            [
                (1, "prompt", String::new()),
                (3, "tool_use", "Read Success".to_owned()),
                (4, "stop", "Completed".to_owned()),
            ]
        );
        for event in events(&mapped) {
            assert_eq!(
                event.record.actor,
                Actor::Subagent {
                    id: Some("a3c91f2".into()),
                    kind: None
                }
            );
            assert_eq!(
                event.record.session_id, SESSION,
                "it joins its parent's session"
            );
        }
        let Content::Prompt { text } = &event_at(&mapped, 1).record.content else {
            panic!("a prompt");
        };
        assert_eq!(
            text.as_deref(),
            Some("Read src/lib.rs and report what parse does.")
        );
        assert_eq!((mapped.incomplete_turns, mapped.pending), (0, 0));
    }

    /// A body over the entry limit is refused whole, never imported as its first entries.
    #[test]
    fn a_transcript_over_the_entry_limit_is_refused() {
        let lines: Vec<String> = (0..4)
            .map(|i| user_text(&format!("u{i}"), "2026-03-19T10:00:00Z", "a prompt"))
            .collect();
        assert_eq!(read_capped(&body(&lines), 3).unwrap_err(), OVER_THE_LIMIT);
        assert_eq!(read_capped(&body(&lines[..3]), 3).unwrap().items.len(), 3);
        assert_eq!(
            memory_engine::bootstrap::BootstrapConfig::default().max_entries,
            1_000_000,
            "the phrase names the limit"
        );
    }

    #[test]
    fn a_body_with_no_session_is_refused() {
        assert_eq!(read(b"").unwrap_err(), "no session");
        assert_eq!(read(b"not json\n[1]\n").unwrap_err(), "no session");
        let none = line(&json!({ "type": "user", "uuid": "u", "message": { "content": "x" } }));
        assert_eq!(read(none.as_bytes()).unwrap_err(), "no session");
    }
}
