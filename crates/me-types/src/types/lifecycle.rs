//! The harness-neutral lifecycle record and ADR-0020 §5's segment rule.
//!
//! A harness adapter turns one hook body into one [`NeutralRecord`](crate::types::lifecycle::NeutralRecord);
//! the engine applies it. Nothing here names a tool or a harness field (ADR-0020 §1).

use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use serde_json::Value;
use thiserror::Error;

use crate::types::{FactType, NewActivity, NewEvent, NewFact, ScopeNode};

/// The idle threshold of ADR-0020 §5: a client gap longer than this, or a session the server heard
/// nothing of for this long, ends a segment.
pub const IDLE: TimeDelta = TimeDelta::minutes(30);

const TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// The order key of ADR-0020 §5: `(ME-Event-Time, ME-Event-Seq as a number, ME-Event-Id)`.
///
/// The derived [`Ord`] compares the fields in that order. The time has the second resolution of
/// the envelope; the server never compares it with its own clock.
///
/// # Examples
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use me_types::types::lifecycle::OrderKey;
///
/// let t = Utc.with_ymd_and_hms(2026, 11, 21, 9, 30, 0).unwrap();
/// let nine = OrderKey { time: t, seq: 9, event_id: "b".into() };
/// let ten = OrderKey { time: t, seq: 10, event_id: "a".into() };
/// assert!(nine < ten, "the seq is a number, not text");
/// assert_eq!(ten.to_text(), "2026-11-21T09:30:00Z|000010|a");
/// assert_eq!(OrderKey::from_text(&ten.to_text()), Some(ten));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrderKey {
    /// The client's time, UTC.
    pub time: DateTime<Utc>,
    /// The tie-break within one second, 1 to 6 digits on the wire.
    pub seq: u32,
    /// The event ID, the last tie-break.
    pub event_id: String,
}

impl OrderKey {
    /// The key as fixed-width text whose byte order is the key's order, for a column a query can
    /// compare: `"%Y-%m-%dT%H:%M:%SZ|{seq:06}|{event_id}"`.
    #[must_use]
    pub fn to_text(&self) -> String {
        format!(
            "{}|{:06}|{}",
            self.time.format(TIME_FORMAT),
            self.seq,
            self.event_id
        )
    }

    /// The inverse of [`to_text`](Self::to_text); `None` for any other text.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let (time, rest) = text.split_once('|')?;
        let (seq, event_id) = rest.split_once('|')?;
        Some(Self {
            time: NaiveDateTime::parse_from_str(time, TIME_FORMAT)
                .ok()?
                .and_utc(),
            seq: seq.parse().ok()?,
            event_id: event_id.to_owned(),
        })
    }
}

/// The six kinds of a neutral record (ADR-0020 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A session starts.
    SessionStart,
    /// A prompt, or a subagent turn starting.
    Prompt,
    /// A tool call.
    ToolUse,
    /// A turn ends.
    Stop,
    /// A conversation was compacted.
    Compact,
    /// A session ends.
    SessionEnd,
}

impl Kind {
    /// The name stored in the event payload and in a segment's counts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::Prompt => "prompt",
            Self::ToolUse => "tool_use",
            Self::Stop => "stop",
            Self::Compact => "compact",
            Self::SessionEnd => "session_end",
        }
    }
}

/// Who acted: the main agent, whose events bound segments and become memories, or a subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// The main agent.
    Main,
    /// A subagent, with the identifier and type the harness reported.
    Subagent {
        /// The subagent's identifier.
        id: Option<String>,
        /// The subagent's type.
        kind: Option<String>,
    },
}

/// How a tool call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    /// The call ran and returned.
    Success,
    /// The call failed.
    Failure,
    /// The user interrupted it.
    Interrupted,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The agent gave its final message.
    Completed,
    /// The turn ended on an API error; its message is the error text.
    ApiError,
}

/// What one kind of record carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// A session starts.
    SessionStart {
        /// Why: `startup`, `resume`, `clear`, `fork` or `compact` for Claude Code.
        source: Option<String>,
    },
    /// A prompt, in full; `None` for a subagent turn starting.
    Prompt {
        /// The prompt.
        text: Option<String>,
    },
    /// A tool call, as the adapter read it.
    ToolUse {
        /// The tool's name; data, never a condition of the core.
        tool: String,
        /// The tool's input.
        input: Value,
        /// The result text, or the error text of a failure.
        result: Option<String>,
        /// How the call ended.
        outcome: ToolOutcome,
        /// The paths the call touches, as it spelled them.
        paths: Vec<String>,
        /// The ME call ID the result names, read before any cut.
        call: Option<String>,
        /// The adapter's activity duplicate window in seconds; `None` keeps the call out of the
        /// activity stream.
        dedup_window_secs: Option<i64>,
    },
    /// A turn ends.
    Stop {
        /// How it ended.
        outcome: StopOutcome,
        /// The final message, in full.
        message: Option<String>,
    },
    /// A conversation was compacted.
    Compact {
        /// What triggered it.
        trigger: Option<String>,
        /// The summary, in full; never absent, so every main compact becomes a memory.
        summary: String,
    },
    /// A session ends.
    SessionEnd {
        /// Why.
        reason: Option<String>,
    },
}

impl Content {
    /// The record's kind.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        match self {
            Self::SessionStart { .. } => Kind::SessionStart,
            Self::Prompt { .. } => Kind::Prompt,
            Self::ToolUse { .. } => Kind::ToolUse,
            Self::Stop { .. } => Kind::Stop,
            Self::Compact { .. } => Kind::Compact,
            Self::SessionEnd { .. } => Kind::SessionEnd,
        }
    }
}

/// One harness event, neutral: what an adapter hands the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeutralRecord {
    /// The session's identifier.
    pub session_id: String,
    /// Who acted.
    pub actor: Actor,
    /// The working directory the harness reported; provenance, and a base for relative paths.
    pub cwd: Option<String>,
    /// The harness's own version, when the adapter can read one from the body (ADR-0020 §3):
    /// a transcript entry carries it, a hook input does not.
    pub harness_version: Option<String>,
    /// The kind's own content.
    pub content: Content,
}

/// One native memory file as read, before any redaction (ADR-0009): the text it becomes, its
/// type, importance, date and metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryItem {
    /// The body, trimmed.
    pub text: String,
    /// The fact type its memory type routes to.
    pub fact_type: FactType,
    /// The base importance of a hand-authored memory.
    pub importance: f64,
    /// Its date: the frontmatter's, else the file name's, else the caller's fallback.
    pub time: DateTime<Utc>,
    /// Where it came from and what its frontmatter says.
    pub metadata: Value,
}

/// How a body failed to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseClass {
    /// Not UTF-8.
    NotUtf8,
    /// Not JSON.
    NotJson,
    /// JSON, but not an object.
    NotObject,
}

/// A parse failure: a class and a position, never a byte of the body or the parser's message,
/// which can quote it (ADR-0020 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("body does not parse ({class:?}) at line {line}, column {column}")]
pub struct ParseFailure {
    /// What went wrong.
    pub class: ParseClass,
    /// The 1-based line.
    pub line: u64,
    /// The 1-based column.
    pub column: u64,
}

/// Why an adapter could not hand over a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BodyFault {
    /// The body does not parse: it is quarantined as metadata (ADR-0020 §3).
    #[error(transparent)]
    Parse(ParseFailure),
    /// The body parses and the adapter cannot map it; the reason is a fixed phrase, never a
    /// quote. It answers 422 and the event stays whole on the client.
    #[error("body cannot be mapped: {0}")]
    Unmappable(&'static str),
}

/// What the route knows of one delivery before it asks the engine to apply it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The machine the token names.
    pub machine: String,
    /// `ME-Event-Id`, as sent.
    pub event_id: String,
    /// `ME-Event-Time`.
    pub time: DateTime<Utc>,
    /// `ME-Event-Seq`.
    pub seq: u32,
    /// `ME-Harness`.
    pub harness: String,
    /// `ME-Client-Version`, as sent.
    pub client_version: String,
    /// The SHA-256 of the body.
    pub body_sha256: [u8; 32],
    /// The body's size in bytes.
    pub body_size: u64,
    /// When the server received it.
    pub received_at: DateTime<Utc>,
    /// The scope path the event is filed under.
    pub home: String,
    /// The project facts as decoded with the resolved key and the rules version.
    pub provenance: Value,
}

/// What the engine did with a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// Committed; the id of the memory it became, when it became one.
    Stored {
        /// The fact, if the event became one.
        fact: Option<i64>,
    },
    /// The key is held with this body: nothing was written.
    Duplicate,
    /// The key is held with another body: nothing was written.
    Collision,
    /// The body did not parse and is held as metadata only.
    Quarantined,
    /// The engine cannot apply this record; nothing was written and no key is held.
    Refused(&'static str),
}

/// What the engine hands the store for one delivery, beside the key (ADR-0020 §3, §5).
#[derive(Debug, Clone)]
pub enum DeliveryWrite {
    /// Only the key: answers whether it is held, and writes nothing. For a body that cannot be
    /// applied, so that a refusal leaves no key behind.
    Check,
    /// A body that did not parse, held as metadata only: the receipt's quarantine record.
    Quarantine(Value),
    /// A captured event and its effects.
    Capture(Box<CaptureWrite>),
}

/// One captured event and everything that commits with it. The store replaces the `scope_id` of
/// the event, the activity and the fact by the leaf of `home`, and the fact's `source_event_id`
/// by the event's row.
#[derive(Debug, Clone)]
pub struct CaptureWrite {
    /// The scope path the event is filed under; created when missing.
    pub home: String,
    /// The event, already redacted.
    pub event: NewEvent,
    /// The session's identifier.
    pub session_id: String,
    /// The event's order key.
    pub key: OrderKey,
    /// The event's kind, counted in its segment; a session end also ends the segment it is in.
    pub kind: Kind,
    /// Whether it is a main-agent prompt or stop, which makes its segment a checkpoint.
    pub main_turn: bool,
    /// Whether it is a main-agent session start that begins a new run.
    pub boundary: bool,
    /// Whether it is a main-agent final message: the segment's final key and fact follow it.
    pub final_message: bool,
    /// The activity row and its duplicate window in seconds; `None` writes none.
    pub activity: Option<(NewActivity, i64)>,
    /// The memory the event becomes, queued for its vector; skipped when a tombstone blocks its
    /// text.
    pub fact: Option<NewFact>,
    /// The call ID the event's result names: linked to the call record of the same machine.
    pub call: Option<String>,
}

/// What the store did with a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// A [`DeliveryWrite::Check`] found the key free; nothing was written.
    Absent,
    /// The key is held with this body; nothing was written.
    Duplicate,
    /// The key is held with another body; nothing was written.
    Collision,
    /// The quarantine record is held.
    Quarantined,
    /// The event and its effects are committed.
    Stored {
        /// The event's row.
        event: i64,
        /// The fact it became, if one was stored.
        fact: Option<i64>,
        /// The scope nodes from the home up to the root's child, for the in-memory tree.
        chain: Vec<ScopeNode>,
    },
}

/// The state of a segment that [`step`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentState {
    /// The key of the segment's first event.
    pub start: OrderKey,
    /// The key of its last event.
    pub last: OrderKey,
    /// Whether it has ended.
    pub ended: bool,
}

/// How a segment ended (ADR-0020 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedBy {
    /// A session end.
    SessionEnd,
    /// The next session start opened a new segment.
    SessionStart,
    /// A client gap of more than [`IDLE`].
    Gap,
    /// The server heard nothing of the session for [`IDLE`].
    Idle,
}

impl EndedBy {
    /// The name stored in the segment's `ended_by` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionEnd => "session_end",
            Self::SessionStart => "session_start",
            Self::Gap => "gap",
            Self::Idle => "idle",
        }
    }
}

/// What an event does to its session's segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Older than the latest segment: stored, no segment changes.
    Late,
    /// Joins the latest segment.
    Join,
    /// Opens a segment; `ends_latest` says how the open latest segment ends, if it does.
    Open {
        /// How the latest segment ends at this event.
        ends_latest: Option<EndedBy>,
    },
}

/// What an event with this key does to the session's latest segment (ADR-0020 §5, rules 1 and 2).
///
/// `boundary` is a main-agent session start whose source starts a new run (`startup`, `resume`,
/// `clear` or `fork` for Claude Code). A session end ending its own segment (rule 3) is the
/// caller's step.
///
/// # Examples
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use me_types::types::lifecycle::{OrderKey, SegmentState, Step, step};
///
/// let key = |s: i64, seq: u32| OrderKey {
///     time: Utc.timestamp_opt(1_800_000_000 + s, 0).unwrap(),
///     seq,
///     event_id: "e".into(),
/// };
/// assert_eq!(step(None, &key(0, 1), false), Step::Open { ends_latest: None });
///
/// let open = SegmentState { start: key(0, 1), last: key(10, 1), ended: false };
/// assert_eq!(step(Some(&open), &key(20, 1), false), Step::Join);
/// assert_eq!(step(Some(&open), &key(-1, 1), false), Step::Late);
/// ```
#[must_use]
pub fn step(latest: Option<&SegmentState>, key: &OrderKey, boundary: bool) -> Step {
    let Some(latest) = latest else {
        return Step::Open { ends_latest: None };
    };
    if *key < latest.start || (latest.ended && *key <= latest.last) {
        Step::Late
    } else if latest.ended {
        Step::Open { ends_latest: None }
    } else if *key > latest.last && boundary {
        Step::Open {
            ends_latest: Some(EndedBy::SessionStart),
        }
    } else if key.time - latest.last.time > IDLE {
        Step::Open {
            ends_latest: Some(EndedBy::Gap),
        }
    } else {
        Step::Join
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(secs: i64, seq: u32, id: &str) -> OrderKey {
        OrderKey {
            time: Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap(),
            seq,
            event_id: id.into(),
        }
    }

    #[test]
    fn order_keys_compare_time_then_seq_as_a_number_then_id() {
        let ordered = [at(0, 9, "b"), at(0, 10, "a"), at(0, 10, "b"), at(1, 0, "a")];
        for pair in ordered.windows(2) {
            assert!(pair[0] < pair[1], "{:?} < {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn the_text_sorts_like_the_key() {
        let ordered = [
            at(0, 9, "b"),
            at(0, 10, "a"),
            at(0, 10, "b"),
            at(1, 0, "a"),
            at(3600 * 24 * 400, 0, "a"),
        ];
        for pair in ordered.windows(2) {
            assert!(pair[0].to_text() < pair[1].to_text());
        }
        for key in &ordered {
            assert_eq!(OrderKey::from_text(&key.to_text()).as_ref(), Some(key));
        }
        assert_eq!(OrderKey::from_text("2026-11-21T09:30:00Z|000010"), None);
        assert_eq!(OrderKey::from_text("nonsense|000010|a"), None);
    }

    fn segment(ended: bool) -> SegmentState {
        SegmentState {
            start: at(100, 1, "x"),
            last: at(200, 1, "x"),
            ended,
        }
    }

    #[test]
    fn step_follows_adr_0020_section_5() {
        let (open, ended) = (segment(false), segment(true));
        let go = |latest: Option<&SegmentState>, secs: i64, seq: u32, boundary: bool| {
            step(latest, &at(secs, seq, "x"), boundary)
        };
        let gap = IDLE.num_seconds();
        let ends = |by| Step::Open {
            ends_latest: Some(by),
        };
        let none = Step::Open { ends_latest: None };

        assert_eq!(go(None, 0, 1, false), none, "the first event");
        assert_eq!(
            go(Some(&open), 99, 1, false),
            Step::Late,
            "before the start"
        );
        assert_eq!(go(Some(&open), 99, 1, true), Step::Late, "a late resume");
        assert_eq!(
            go(Some(&ended), 200, 1, false),
            Step::Late,
            "at an ended last"
        );
        assert_eq!(
            go(Some(&ended), 150, 1, false),
            Step::Late,
            "before an ended last"
        );
        assert_eq!(
            go(Some(&ended), 150, 1, true),
            Step::Late,
            "a late resume, ended"
        );
        assert_eq!(go(Some(&ended), 200, 2, false), none, "after an ended last");
        assert_eq!(
            go(Some(&ended), 300, 1, true),
            none,
            "a resume after an end"
        );
        assert_eq!(
            go(Some(&open), 200, 2, true),
            ends(EndedBy::SessionStart),
            "a boundary"
        );
        assert_eq!(
            go(Some(&open), 150, 1, true),
            Step::Join,
            "a boundary inside the range"
        );
        assert_eq!(
            go(Some(&open), 201, 1, false),
            Step::Join,
            "an event after last"
        );
        assert_eq!(
            go(Some(&open), 200 + gap, 1, false),
            Step::Join,
            "a gap of 30 min"
        );
        assert_eq!(
            go(Some(&open), 200 + gap + 1, 1, false),
            ends(EndedBy::Gap),
            "30 min 1 s"
        );
        assert_eq!(
            go(Some(&open), 120, 1, false),
            Step::Join,
            "out of order inside"
        );
    }
}
