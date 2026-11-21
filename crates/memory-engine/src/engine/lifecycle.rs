//! The neutral lifecycle core (#1025, ADR-0020 §2, §5, §6): apply one neutral record under the
//! capture contract (redact, then cut tool I/O to 512 bytes, stub sensitive paths), and let the
//! store commit it with its receipt in one transaction.
//!
//! The core knows no tool by name and no harness field by name: an adapter hands it a
//! [`NeutralRecord`].

use std::path::Path;

use chrono::SecondsFormat;
use serde_json::{Map, Value, json};

use crate::error::{MemoryError, Result, StorageError};
use crate::limits::LIFECYCLE_BODY_BYTES;
use crate::redact;
use crate::types::lifecycle::{
    Actor, Applied, BodyFault, CaptureWrite, Content, Delivery, DeliveryWrite, IDLE, Kind,
    NeutralRecord, OrderKey, ParseClass, Recorded, StopOutcome, ToolOutcome,
};
use crate::types::{EventType, FactType, NewActivity, NewEvent, NewFact, OutcomeClass};

use super::activity::args_hash;
use super::embed_queue::open_status_connection;
use super::{MemoryEngine, spawn_join_err};

/// The longest session ID, in bytes.
const MAX_SESSION_ID_BYTES: usize = 256;

/// The start sources that begin a new run (ADR-0020 §5, rule 2).
const BOUNDARY_SOURCES: [&str; 4] = ["startup", "resume", "clear", "fork"];

/// What separates an imported capture from a live one (#1026): the author's denylist, which
/// live capture does not take, and where the event came from.
pub(super) struct Capture<'a> {
    pub(super) denylist: &'a [String],
    pub(super) origin: Origin,
}

impl Capture<'static> {
    /// Live capture: signatures only, labelled `lifecycle`, its memory dated by its receipt.
    pub(super) const LIVE: Self = Self {
        denylist: &[],
        origin: Origin::Live,
    };
}

/// Where a captured event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// `POST /lifecycle/events`.
    Live,
    /// An uploaded copy of the harness's own record (#1026): labelled `backfill`, so it never
    /// counts as live, and its memory dated by the event (ADR-0009 D2).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the backfill import constructs it in the next commit"
        )
    )]
    Backfill,
}

impl Origin {
    /// The event's `source` and `origin_node_id`.
    const fn label(self) -> &'static str {
        match self {
            Self::Live => "lifecycle",
            Self::Backfill => "backfill",
        }
    }
}

/// What one delivery comes to before the store sees it.
enum Plan {
    /// The engine cannot apply it: the store only says whether the key is held.
    Refuse(&'static str),
    /// Write this, with the secrets it lost and the stubs it made.
    Write {
        write: DeliveryWrite,
        found: u64,
        stubbed: u64,
    },
}

impl MemoryEngine {
    /// Apply one neutral lifecycle record under the capture contract, and commit it with its
    /// receipt in one transaction: ADR-0020 §5's delivery, which the route acknowledges only
    /// after this returns.
    ///
    /// The `(machine, event ID)` key is read first. A held key answers
    /// [`Recorded::Duplicate`] (the same body) or [`Recorded::Collision`] (another) and writes
    /// nothing, whatever `body` holds. Otherwise:
    ///
    /// - a body that did not parse ([`BodyFault::Parse`]) is held as metadata only
    ///   ([`Recorded::Quarantined`]);
    /// - a body the adapter cannot map ([`BodyFault::Unmappable`]), an identifier holding a
    ///   machine token, or a record off the engine's invariants (a session ID of 0 or over 256
    ///   bytes, an empty compaction summary, a delivery or a text over
    ///   [`LIFECYCLE_BODY_BYTES`]) is [`Recorded::Refused`], and leaves no key;
    /// - a record is redacted, bounded and stored: its event, the activity of a tool call the
    ///   adapter did not ignore, the memory a main final message or compaction summary becomes
    ///   (queued for its vector, so capture succeeds with the embedder down; skipped when a
    ///   tombstone blocks its text), and its session's segment step.
    ///
    /// Prompts, final messages and summaries are kept in full after redaction: the body limit
    /// of the route is the one size bound. Tool inputs and results are cut to 512 bytes, and a
    /// call that touches a sensitive path is a stub.
    ///
    /// # Errors
    ///
    /// `MemoryError::ReadOnly` on a read-only engine; `Conflict(Redaction)` for a text that does
    /// not settle; `Storage(Unavailable)` when the store cannot take the write now and the same
    /// call can work later; any other storage failure. Nothing is written on any error.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use memory_engine::types::lifecycle::{Actor, Content, Delivery, NeutralRecord, Recorded};
    ///
    /// let engine = memory_engine::MemoryEngine::builder(4).build()?;
    /// let now = chrono::Utc::now();
    /// let delivery = Delivery {
    ///     machine: "laptop".into(),
    ///     event_id: "e1".into(),
    ///     time: now,
    ///     seq: 1,
    ///     harness: "claude-code".into(),
    ///     client_version: "1.0.0".into(),
    ///     body_sha256: [0; 32],
    ///     body_size: 42,
    ///     received_at: now,
    ///     home: "user:alice/project:demo".into(),
    ///     provenance: serde_json::json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
    /// };
    /// let record = NeutralRecord {
    ///     session_id: "s1".into(),
    ///     actor: Actor::Main,
    ///     cwd: None,
    ///     harness_version: None,
    ///     content: Content::Prompt { text: Some("hello".into()) },
    /// };
    /// assert_eq!(
    ///     engine.record_lifecycle_event(&delivery, Ok(record.clone())).await?,
    ///     Recorded::Stored { fact: None }
    /// );
    /// assert_eq!(
    ///     engine.record_lifecycle_event(&delivery, Ok(record)).await?,
    ///     Recorded::Duplicate
    /// );
    /// # Ok::<(), memory_engine::MemoryError>(())
    /// # }).unwrap();
    /// ```
    pub async fn record_lifecycle_event(
        &self,
        delivery: &Delivery,
        body: std::result::Result<NeutralRecord, BodyFault>,
    ) -> Result<Recorded> {
        self.ensure_open()?;
        self.ensure_writable()?;
        let plan = plan(delivery, body).await?;
        let (write, refusal, found, stubbed) = match plan {
            Plan::Refuse(reason) => (DeliveryWrite::Check, Some(reason), 0, 0),
            Plan::Write {
                write,
                found,
                stubbed,
            } => (write, None, found, stubbed),
        };
        let applied = self
            .storage
            .record_delivery(
                &delivery.machine,
                &delivery.event_id,
                &delivery.body_sha256,
                delivery.received_at,
                write,
            )
            .await?;
        match (applied, refusal) {
            (Applied::Absent, Some(reason)) => Ok(Recorded::Refused(reason)),
            (Applied::Duplicate, _) => Ok(Recorded::Duplicate),
            (Applied::Collision, _) => Ok(Recorded::Collision),
            (Applied::Quarantined, _) => Ok(Recorded::Quarantined),
            (Applied::Stored { fact, chain, .. }, _) => {
                me_index::insert_scope_chain(&self.scope_tree, chain);
                if fact.is_some() {
                    self.embed_work.notify_one();
                }
                self.tally(found, stubbed).await;
                Ok(Recorded::Stored { fact })
            }
            (Applied::Absent, None) => Err(MemoryError::Internal(
                "the store found no key for a write it was asked to make".into(),
            )),
        }
    }

    /// End, as idle, every open segment whose session the server has heard nothing of for 30
    /// minutes before `now` (ADR-0020 §5, rule 4): the only rule that reads the server's clock,
    /// and it compares it with the server's own times of receipt. Returns how many segments
    /// ended; a second call ends none of them again.
    ///
    /// # Errors
    ///
    /// `MemoryError::ReadOnly` on a read-only engine; `MemoryError::Storage` on a write
    /// failure, `Unavailable` when the same call can work later.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let engine = memory_engine::MemoryEngine::builder(4).build()?;
    /// assert_eq!(engine.end_idle_segments(chrono::Utc::now()).await?, 0);
    /// # Ok::<(), memory_engine::MemoryError>(())
    /// # }).unwrap();
    /// ```
    pub async fn end_idle_segments(&self, now: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        self.ensure_open()?;
        self.ensure_writable()?;
        self.storage.end_idle_segments(now - IDLE, now).await
    }
}

/// The number of quarantine records the store holds (ADR-0020 §3).
///
/// A quarantine record is a delivery whose body did not parse. The count is read through one
/// read-only connection, like [`read_redaction_counts`](crate::read_redaction_counts): no
/// engine, no store lock, no migration.
///
/// # Errors
///
/// Returns `MemoryError::Storage` if the file cannot be opened read-only, and
/// `MemoryError::Migration` if its schema version is not this binary's.
///
/// # Examples
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("memory.db");
/// let mut engine = memory_engine::MemoryEngine::builder(4).path(&db).build()?;
/// engine.close().await?;
///
/// assert_eq!(memory_engine::read_quarantined(&db)?, 0);
/// # Ok::<(), memory_engine::MemoryError>(())
/// # }).unwrap();
/// ```
pub fn read_quarantined(db: &Path) -> Result<u64> {
    let conn = open_status_connection(db)?;
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::backend)?;
    crate::store::schema::validate_schema_version(&tx)?;
    crate::store::lifecycle::count_quarantined(&tx)
}

/// Decide what the store is asked to write. Redacting a body of up to 16 MiB is blocking work.
async fn plan(
    delivery: &Delivery,
    body: std::result::Result<NeutralRecord, BodyFault>,
) -> Result<Plan> {
    if let Some(reason) = delivery_off_invariants(delivery) {
        return Ok(Plan::Refuse(reason));
    }
    match body {
        Err(BodyFault::Unmappable(reason)) => Ok(Plan::Refuse(reason)),
        Err(BodyFault::Parse(failure)) => Ok(Plan::Write {
            write: DeliveryWrite::Quarantine(json!({
                "client_time": delivery.time.to_rfc3339_opts(SecondsFormat::Secs, true),
                "harness": delivery.harness,
                "size": delivery.body_size,
                "error": {
                    "class": parse_class(failure.class),
                    "line": failure.line,
                    "column": failure.column,
                },
            })),
            found: 0,
            stubbed: 0,
        }),
        Ok(record) => {
            if let Some(reason) = record_off_invariants(&record) {
                return Ok(Plan::Refuse(reason));
            }
            let delivery = delivery.clone();
            tokio::task::spawn_blocking(move || prepare(&delivery, record, &Capture::LIVE))
                .await
                .map_err(spawn_join_err)?
        }
    }
}

const fn parse_class(class: ParseClass) -> &'static str {
    match class {
        ParseClass::NotUtf8 => "not_utf8",
        ParseClass::NotJson => "not_json",
        ParseClass::NotObject => "not_object",
    }
}

/// An identifier the machine-token rule would change: it would put a token in a key.
fn holds_a_token(identifier: &str) -> bool {
    redact::scrub_provenance(identifier) != identifier
}

fn delivery_off_invariants(delivery: &Delivery) -> Option<&'static str> {
    if usize::try_from(delivery.body_size).is_ok_and(|size| size <= LIFECYCLE_BODY_BYTES) {
        holds_a_token(&delivery.event_id).then_some("the event ID holds a machine token")
    } else {
        Some("the body is over the lifecycle limit")
    }
}

fn record_off_invariants(record: &NeutralRecord) -> Option<&'static str> {
    if record.session_id.is_empty() || record.session_id.len() > MAX_SESSION_ID_BYTES {
        return Some("the session ID is not 1 to 256 bytes");
    }
    if holds_a_token(&record.session_id) {
        return Some("the session ID holds a machine token");
    }
    if let Actor::Subagent { id: Some(id), .. } = &record.actor
        && holds_a_token(id)
    {
        return Some("the agent ID holds a machine token");
    }
    if matches!(&record.content, Content::Compact { summary, .. } if summary.is_empty()) {
        return Some("the compaction summary is empty");
    }
    (text_len(record) > LIFECYCLE_BODY_BYTES).then_some("the text is over the lifecycle limit")
}

/// The bytes of every text a record holds, before redaction: never more than the body it was
/// decoded from. A tool call's paths are not counted: they are the adapter's copies of strings
/// of its input, not text the body held a second time.
fn text_len(record: &NeutralRecord) -> usize {
    let opt = |s: &Option<String>| s.as_ref().map_or(0, String::len);
    let actor = match &record.actor {
        Actor::Main => 0,
        Actor::Subagent { id, kind } => opt(id) + opt(kind),
    };
    let content = match &record.content {
        Content::SessionStart { source } => opt(source),
        Content::Prompt { text } => opt(text),
        Content::ToolUse {
            tool,
            input,
            result,
            ..
        } => tool.len() + input.to_string().len() + opt(result),
        Content::Stop { message, .. } => opt(message),
        Content::Compact { trigger, summary } => opt(trigger) + summary.len(),
        Content::SessionEnd { reason } => opt(reason),
    };
    record.session_id.len() + opt(&record.cwd) + actor + content
}

const fn event_type(kind: Kind) -> EventType {
    match kind {
        Kind::ToolUse => EventType::ToolCall,
        Kind::Prompt | Kind::Stop => EventType::Interaction,
        Kind::SessionStart | Kind::Compact | Kind::SessionEnd => EventType::SystemEvent,
    }
}

const fn tool_outcome(outcome: ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Success => "success",
        ToolOutcome::Failure => "failure",
        ToolOutcome::Interrupted => "interrupted",
    }
}

const fn stop_outcome(outcome: StopOutcome) -> &'static str {
    match outcome {
        StopOutcome::Completed => "completed",
        StopOutcome::ApiError => "api_error",
    }
}

/// What a record adds to the event beside its kind's content, before the payload is scrubbed.
#[derive(Default)]
struct Extras {
    activity: Option<(NewActivity, i64)>,
    call: Option<String>,
    boundary: bool,
    /// The payload member a memory is made from, and what the memory is.
    memory: Option<(&'static str, &'static str)>,
    found: u64,
    stubbed: u64,
}

/// The kind's own content, as payload members, with the tool I/O already captured.
fn content_members(
    delivery: &Delivery,
    record: &NeutralRecord,
    main: bool,
    denylist: &[String],
    payload: &mut Map<String, Value>,
) -> Result<Extras> {
    let mut extras = Extras::default();
    match &record.content {
        Content::SessionStart { source } => {
            extras.boundary = main
                && source
                    .as_deref()
                    .is_some_and(|s| BOUNDARY_SOURCES.contains(&s));
            payload.insert("source".to_owned(), json!(source));
        }
        Content::Prompt { text } => {
            payload.insert("prompt".to_owned(), json!(text));
        }
        Content::ToolUse {
            tool,
            input,
            result,
            outcome,
            paths,
            call,
            dedup_window_secs,
        } => {
            let class = match outcome {
                ToolOutcome::Success => OutcomeClass::Success,
                ToolOutcome::Failure => OutcomeClass::Error,
                ToolOutcome::Interrupted => OutcomeClass::Other("interrupted".to_owned()),
            };
            let capture = redact::capture_tool_use(
                input,
                result.as_deref(),
                class,
                paths,
                record.cwd.as_deref(),
                denylist,
            )?;
            extras.found = capture.input_found + capture.result_found;
            extras.stubbed = u64::from(capture.stub);
            // A tool name is content: the payload's copy is scrubbed below, the activity's here.
            let (tool_name, _) = redact::scrub_with(tool, denylist)?;
            payload.insert("tool".to_owned(), json!(tool));
            payload.insert("input".to_owned(), capture.args.clone());
            payload.insert("result".to_owned(), json!(capture.result));
            payload.insert("outcome".to_owned(), json!(tool_outcome(*outcome)));
            extras.call.clone_from(call);
            extras.activity = dedup_window_secs.map(|window| {
                let activity = NewActivity {
                    session_id: record.session_id.clone(),
                    tool_name: tool_name.clone(),
                    args_hash: args_hash(&capture.hash_input),
                    args: capture.args,
                    result_summary: capture.result,
                    outcome_class: capture.outcome,
                    timestamp: delivery.time,
                    scope_id: 1,
                    machine: Some(delivery.machine.clone()),
                };
                (activity, window)
            });
        }
        Content::Stop { outcome, message } => {
            payload.insert("outcome".to_owned(), json!(stop_outcome(*outcome)));
            payload.insert("message".to_owned(), json!(message));
            if main
                && *outcome == StopOutcome::Completed
                && message.as_deref().is_some_and(|m| !m.is_empty())
            {
                extras.memory = Some(("message", "final_message"));
            }
        }
        Content::Compact { trigger, summary } => {
            payload.insert("trigger".to_owned(), json!(trigger));
            payload.insert("summary".to_owned(), json!(summary));
            if main {
                extras.memory = Some(("summary", "compaction_summary"));
            }
        }
        Content::SessionEnd { reason } => {
            payload.insert("reason".to_owned(), json!(reason));
        }
    }
    Ok(extras)
}

/// Redact and bound one record, and build what the store writes for it (decisions 9 to 11 of
/// #1025): the payload takes [`redact::scrub_payload`], so identifiers are columns as sent and
/// every text is redacted, with no size check beyond the wire's. `capture` says whether it is
/// live or imported (#1026), which sets its labels, its memory's date and the denylist.
fn prepare(delivery: &Delivery, record: NeutralRecord, capture: &Capture<'_>) -> Result<Plan> {
    let main = matches!(record.actor, Actor::Main);
    let kind = record.content.kind();
    let mut payload = Map::new();
    let (subagent, subagent_type) = match &record.actor {
        Actor::Main => (None, None),
        Actor::Subagent { id, kind } => (id.as_deref(), kind.as_deref()),
    };
    for (name, value) in [
        ("machine", json!(delivery.machine)),
        ("event_id", json!(delivery.event_id)),
        (
            "time",
            json!(delivery.time.to_rfc3339_opts(SecondsFormat::Secs, true)),
        ),
        ("seq", json!(delivery.seq)),
        ("harness", json!(delivery.harness)),
        ("client_version", json!(delivery.client_version)),
        ("harness_version", json!(record.harness_version)),
        ("kind", json!(kind.as_str())),
        ("actor", json!(if main { "main" } else { "subagent" })),
        ("agent_id", json!(subagent)),
        ("agent_type", json!(subagent_type)),
        ("cwd", json!(record.cwd)),
        (redact::PROVENANCE_KEY, delivery.provenance.clone()),
    ] {
        payload.insert(name.to_owned(), value);
    }
    let extras = content_members(delivery, &record, main, capture.denylist, &mut payload)?;
    let mut payload = Value::Object(payload);
    let found = extras.found + redact::scrub_payload(&mut payload, capture.denylist)?;

    let created = match capture.origin {
        Origin::Live => delivery.received_at,
        Origin::Backfill => delivery.time,
    };
    let fact = extras.memory.and_then(|(member, capture)| {
        let text = payload.get(member)?.as_str()?;
        Some(
            NewFact::builder(text, Vec::new(), FactType::Episodic)
                .t_created(created)
                .t_valid(delivery.time.min(delivery.received_at))
                .metadata(json!({
                    "machine": delivery.machine,
                    "capture": capture,
                    "session_id": record.session_id,
                }))
                .build(),
        )
    });
    let event = NewEvent {
        timestamp: delivery.time,
        event_type: event_type(kind),
        payload,
        source: capture.origin.label().to_owned(),
        session_id: Some(record.session_id.clone()),
        scope_id: 1,
        origin_node_id: capture.origin.label().to_owned(),
        sequence_id: 0,
        created_at: Some(delivery.received_at),
    };
    let write = CaptureWrite {
        home: delivery.home.clone(),
        event,
        session_id: record.session_id,
        key: OrderKey {
            time: delivery.time,
            seq: delivery.seq,
            event_id: delivery.event_id.clone(),
        },
        kind,
        main_turn: main && matches!(kind, Kind::Prompt | Kind::Stop),
        boundary: extras.boundary,
        final_message: extras.memory.is_some_and(|(_, c)| c == "final_message"),
        activity: extras.activity,
        fact,
        call: extras.call,
    };
    Ok(Plan::Write {
        write: DeliveryWrite::Capture(Box::new(write)),
        found,
        stubbed: extras.stubbed,
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use super::*;

    fn delivery() -> Delivery {
        let time = Utc.with_ymd_and_hms(2026, 3, 19, 10, 0, 0).unwrap();
        Delivery {
            machine: "laptop".into(),
            event_id: "bf.e.s".into(),
            time,
            seq: 120_000,
            harness: "claude-code".into(),
            client_version: "0.1.0".into(),
            body_sha256: [1; 32],
            body_size: 10,
            received_at: time + Duration::days(200),
            home: "user:u/project:p".into(),
            provenance: json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
        }
    }

    fn final_message(harness_version: Option<&str>) -> NeutralRecord {
        NeutralRecord {
            session_id: "s".into(),
            actor: Actor::Main,
            cwd: None,
            harness_version: harness_version.map(str::to_owned),
            content: Content::Stop {
                outcome: StopOutcome::Completed,
                message: Some("done".into()),
            },
        }
    }

    fn captured(delivery: &Delivery, record: NeutralRecord, capture: &Capture<'_>) -> CaptureWrite {
        match prepare(delivery, record, capture).unwrap() {
            Plan::Write {
                write: DeliveryWrite::Capture(capture),
                ..
            } => *capture,
            _ => panic!("a capture"),
        }
    }

    /// An imported event is labelled `backfill` so it never counts as live, and its memory is
    /// dated by the event, not by its receipt; a live one keeps today's labels and dates.
    #[test]
    fn a_backfill_capture_is_labelled_and_backdated() {
        let delivery = delivery();
        let backfill = Capture {
            denylist: &[],
            origin: Origin::Backfill,
        };
        let imported = captured(&delivery, final_message(Some("2.1.296")), &backfill);
        assert_eq!(
            (
                imported.event.source.as_str(),
                imported.event.origin_node_id.as_str()
            ),
            ("backfill", "backfill")
        );
        assert_eq!(imported.event.timestamp, delivery.time);
        assert_eq!(imported.event.created_at, Some(delivery.received_at));
        assert_eq!(imported.event.payload["harness_version"], "2.1.296");
        let fact = imported.fact.unwrap();
        assert_eq!(fact.t_created, delivery.time);
        assert_eq!(fact.last_accessed, delivery.time);
        assert_eq!(fact.t_valid, Some(delivery.time));

        let live = captured(&delivery, final_message(None), &Capture::LIVE);
        assert_eq!(
            (
                live.event.source.as_str(),
                live.event.origin_node_id.as_str()
            ),
            ("lifecycle", "lifecycle")
        );
        assert!(live.event.payload["harness_version"].is_null());
        assert_eq!(live.fact.unwrap().t_created, delivery.received_at);
    }

    /// The denylist reaches the prompt, never the identifiers.
    #[test]
    fn a_capture_scrubs_its_content_with_the_denylist() {
        let delivery = delivery();
        let denylist = ["laptop".to_owned()];
        let record = NeutralRecord {
            content: Content::Prompt {
                text: Some("on laptop".into()),
            },
            ..final_message(None)
        };
        let capture = Capture {
            denylist: &denylist,
            origin: Origin::Backfill,
        };
        let prompt = captured(&delivery, record, &capture);
        assert_eq!(prompt.event.payload["prompt"], "on [REDACTED:denylist]");
        assert_eq!(prompt.event.payload["machine"], "laptop");
    }
}
