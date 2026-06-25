use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
