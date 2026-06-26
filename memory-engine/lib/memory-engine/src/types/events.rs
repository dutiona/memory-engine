use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Type of event in the append-only log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    Interaction,
    ToolCall,
    MemoryOp,
    SystemEvent,
    /// Outcome feedback signal for a fact (positive, negative, or neutral).
    /// Payload carries `{"fact_id": i64, "outcome": "Positive"|"Negative"|"Neutral"}`.
    OutcomeSignal,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interaction => write!(f, "interaction"),
            Self::ToolCall => write!(f, "tool_call"),
            Self::MemoryOp => write!(f, "memory_op"),
            Self::SystemEvent => write!(f, "system_event"),
            Self::OutcomeSignal => write!(f, "outcome_signal"),
        }
    }
}

/// Error returned when a string does not name a known [`EventType`] variant.
///
/// Carries the offending token so callers can surface an actionable message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown event type: {0}")]
pub struct ParseEventTypeError(pub String);

impl FromStr for EventType {
    type Err = ParseEventTypeError;

    /// Canonical, case-insensitive parse of the `EventType` variant names.
    ///
    /// This is the **single source of truth** for the string→`EventType` mapping
    /// shared by every consumer surface (the MCP server's `ingest` / `replay`
    /// tool parameters). Mirroring [`FactType::from_str`](crate::types::FactType::from_str),
    /// it is intentionally lenient on casing so it accepts both wire conventions
    /// present in the codebase: [`Display`] emits `snake_case` (`"interaction"`),
    /// while serde-derive and the MCP JSON-schema enums use `PascalCase`
    /// (`"Interaction"`). Parsing reconciles both to one canonical enum;
    /// [`EventType::to_string`] remains the canonical output.
    ///
    /// It accepts **all** variants, including [`EventType::OutcomeSignal`] — a
    /// complete parser that round-trips with `Display`. Surfaces that must reject
    /// system-generated types (e.g. the MCP `ingest` tool, whose JSON schema
    /// advertises only the four user-ingestible types) apply their own gate after
    /// this parse rather than relying on an incomplete mapping.
    ///
    /// Note: this is orthogonal to the serde `Deserialize` derive (which stays
    /// `PascalCase`) and to the strict, exact-match DB serialization in
    /// `store::events` — that path must round-trip bytes verbatim, so it does not
    /// share this lenient casing.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Zero-allocation, case-insensitive match. The multi-word variants admit
        // two spellings — the `PascalCase` wire form (`"ToolCall"`, the JSON-schema
        // / serde-derive convention) and the `snake_case` [`Display`] form
        // (`"tool_call"`) — so both casings AND `Display::to_string` round-trip.
        if s.eq_ignore_ascii_case("Interaction") {
            Ok(Self::Interaction)
        } else if s.eq_ignore_ascii_case("ToolCall") || s.eq_ignore_ascii_case("tool_call") {
            Ok(Self::ToolCall)
        } else if s.eq_ignore_ascii_case("MemoryOp") || s.eq_ignore_ascii_case("memory_op") {
            Ok(Self::MemoryOp)
        } else if s.eq_ignore_ascii_case("SystemEvent") || s.eq_ignore_ascii_case("system_event") {
            Ok(Self::SystemEvent)
        } else if s.eq_ignore_ascii_case("OutcomeSignal")
            || s.eq_ignore_ascii_case("outcome_signal")
        {
            Ok(Self::OutcomeSignal)
        } else {
            Err(ParseEventTypeError(s.to_owned()))
        }
    }
}

/// Outcome of using a fact — consumer-supplied feedback signal.
///
/// Stored as an [`EventType::OutcomeSignal`] event in the append-only log.
/// `DreamCycle` queries outcome history to adjust importance scores:
/// consistently negative outcomes decrease importance, positive ones increase it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Outcome {
    Positive,
    Negative,
    Neutral,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Positive => write!(f, "positive"),
            Self::Negative => write!(f, "negative"),
            Self::Neutral => write!(f, "neutral"),
        }
    }
}

/// Error returned when a string does not name a known [`Outcome`] variant.
///
/// Carries the offending token so callers can surface an actionable message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown outcome: {0}")]
pub struct ParseOutcomeError(pub String);

impl FromStr for Outcome {
    type Err = ParseOutcomeError;

    /// Canonical, case-insensitive parse of the `Outcome` variant names.
    ///
    /// The **single source of truth** for the string→`Outcome` mapping shared by
    /// every consumer surface (the MCP server's `record_outcome` tool parameter).
    /// Mirroring [`FactType::from_str`](crate::types::FactType::from_str), it is
    /// lenient on casing so it accepts both the `PascalCase` wire form the MCP
    /// JSON schema advertises (`"Positive"`) and the `snake_case`/lowercase
    /// [`Display`] form (`"positive"`); [`Outcome::to_string`] round-trips through
    /// it. Parsing reconciles every casing to one canonical enum.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Zero-allocation case-insensitive match (no temporary lowercased String).
        if s.eq_ignore_ascii_case("positive") {
            Ok(Self::Positive)
        } else if s.eq_ignore_ascii_case("negative") {
            Ok(Self::Negative)
        } else if s.eq_ignore_ascii_case("neutral") {
            Ok(Self::Neutral)
        } else {
            Err(ParseOutcomeError(s.to_owned()))
        }
    }
}

/// Aggregated outcome counts for a single fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutcomeCounts {
    pub positive: u32,
    pub negative: u32,
    pub neutral: u32,
}

/// An event in the append-only log (source of truth).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub payload: serde_json::Value,
    pub source: String,
    pub session_id: Option<String>,
    pub scope_id: i64,
    /// Node that originated this event (for future multi-node sync).
    #[serde(default = "default_origin_node_id")]
    pub origin_node_id: String,
    /// Monotonic sequence within the origin node (for ordering/dedup in sync).
    #[serde(default)]
    pub sequence_id: i64,
    /// When the event was ingested into this node's store (ingest-time).
    pub created_at: Option<DateTime<Utc>>,
    /// Schema revision of the event payload (for upcasting at read time).
    #[serde(default = "default_event_revision")]
    pub event_revision: u16,
}

// serde default fns (module-private — called only by the derive machinery here).
const fn default_event_revision() -> u16 {
    1
}

fn default_origin_node_id() -> String {
    "local".to_string()
}

/// Event to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub payload: serde_json::Value,
    pub source: String,
    pub session_id: Option<String>,
    pub scope_id: i64,
    pub origin_node_id: String,
    pub sequence_id: i64,
    pub created_at: Option<DateTime<Utc>>,
}

/// Filter for querying the append-only event log.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub event_type: Option<EventType>,
    pub source: Option<String>,
    pub limit: Option<usize>,
    pub id_min: Option<i64>,
    pub id_max: Option<i64>,
    pub order_by_id: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_display() {
        assert_eq!(Outcome::Positive.to_string(), "positive");
        assert_eq!(Outcome::Negative.to_string(), "negative");
        assert_eq!(Outcome::Neutral.to_string(), "neutral");
    }

    // --- EventType FromStr (canonical string parse, shared by MCP surfaces) ---

    #[test]
    fn event_type_from_str_accepts_pascal_case() {
        // PascalCase is the serde-derive / MCP-JSON-schema wire form.
        assert_eq!(
            "Interaction".parse::<EventType>().unwrap(),
            EventType::Interaction
        );
        assert_eq!(
            "ToolCall".parse::<EventType>().unwrap(),
            EventType::ToolCall
        );
        assert_eq!(
            "MemoryOp".parse::<EventType>().unwrap(),
            EventType::MemoryOp
        );
        assert_eq!(
            "SystemEvent".parse::<EventType>().unwrap(),
            EventType::SystemEvent
        );
        // The parser is complete: it accepts the system-generated variant too.
        // Surfaces that must reject it (the MCP `ingest` tool) gate after parsing.
        assert_eq!(
            "OutcomeSignal".parse::<EventType>().unwrap(),
            EventType::OutcomeSignal
        );
    }

    #[test]
    fn event_type_from_str_accepts_snake_case_display_form() {
        // The Display form (snake_case) must also parse so to_string round-trips.
        assert_eq!(
            "tool_call".parse::<EventType>().unwrap(),
            EventType::ToolCall
        );
        assert_eq!(
            "memory_op".parse::<EventType>().unwrap(),
            EventType::MemoryOp
        );
        assert_eq!(
            "system_event".parse::<EventType>().unwrap(),
            EventType::SystemEvent
        );
        assert_eq!(
            "outcome_signal".parse::<EventType>().unwrap(),
            EventType::OutcomeSignal
        );
    }

    #[test]
    fn event_type_from_str_is_case_insensitive() {
        assert_eq!(
            "INTERACTION".parse::<EventType>().unwrap(),
            EventType::Interaction
        );
        assert_eq!(
            "toolcall".parse::<EventType>().unwrap(),
            EventType::ToolCall
        );
        assert_eq!(
            "MEMORY_OP".parse::<EventType>().unwrap(),
            EventType::MemoryOp
        );
    }

    #[test]
    fn event_type_from_str_rejects_unknown_preserving_token() {
        let err = "WisdomOp".parse::<EventType>().unwrap_err();
        // The unknown token is preserved in the error for actionable messages.
        assert!(err.to_string().contains("WisdomOp"));
    }

    #[test]
    fn event_type_from_str_rejects_surrounding_whitespace() {
        // The parser is intentionally strict on whitespace — it does not trim.
        assert!(" Interaction".parse::<EventType>().is_err());
        assert!("ToolCall ".parse::<EventType>().is_err());
        assert!("".parse::<EventType>().is_err());
    }

    #[test]
    fn event_type_display_round_trips_through_from_str() {
        for et in [
            EventType::Interaction,
            EventType::ToolCall,
            EventType::MemoryOp,
            EventType::SystemEvent,
            EventType::OutcomeSignal,
        ] {
            assert_eq!(et.to_string().parse::<EventType>().unwrap(), et);
        }
    }

    // --- Outcome FromStr (canonical string parse, shared by MCP) ---

    #[test]
    fn outcome_from_str_accepts_pascal_case() {
        // PascalCase is the MCP-JSON-schema wire form.
        assert_eq!("Positive".parse::<Outcome>().unwrap(), Outcome::Positive);
        assert_eq!("Negative".parse::<Outcome>().unwrap(), Outcome::Negative);
        assert_eq!("Neutral".parse::<Outcome>().unwrap(), Outcome::Neutral);
    }

    #[test]
    fn outcome_from_str_accepts_lowercase_display_form() {
        assert_eq!("positive".parse::<Outcome>().unwrap(), Outcome::Positive);
        assert_eq!("negative".parse::<Outcome>().unwrap(), Outcome::Negative);
        assert_eq!("neutral".parse::<Outcome>().unwrap(), Outcome::Neutral);
    }

    #[test]
    fn outcome_from_str_is_case_insensitive() {
        assert_eq!("POSITIVE".parse::<Outcome>().unwrap(), Outcome::Positive);
        assert_eq!("nEgAtIvE".parse::<Outcome>().unwrap(), Outcome::Negative);
    }

    #[test]
    fn outcome_from_str_rejects_unknown_preserving_token() {
        let err = "mixed".parse::<Outcome>().unwrap_err();
        assert!(err.to_string().contains("mixed"));
    }

    #[test]
    fn outcome_from_str_rejects_surrounding_whitespace() {
        assert!(" positive".parse::<Outcome>().is_err());
        assert!("Neutral ".parse::<Outcome>().is_err());
        assert!("".parse::<Outcome>().is_err());
    }

    #[test]
    fn outcome_display_round_trips_through_from_str() {
        for o in [Outcome::Positive, Outcome::Negative, Outcome::Neutral] {
            assert_eq!(o.to_string().parse::<Outcome>().unwrap(), o);
        }
    }

    #[test]
    fn event_round_trip_json() {
        let event = Event {
            id: 1,
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"key": "value"}),
            source: "test".into(),
            session_id: Some("sess-1".into()),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
            event_revision: 1,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }
}
