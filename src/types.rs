use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- Enums ---

/// Type of event in the append-only log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    Interaction,
    ToolCall,
    MemoryOp,
    SystemEvent,
}

/// Type of fact (`CoALA` mapping: Episodic, Semantic, Procedural).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FactType {
    Episodic,
    Semantic,
    Procedural,
}

impl fmt::Display for FactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Episodic => write!(f, "episodic"),
            Self::Semantic => write!(f, "semantic"),
            Self::Procedural => write!(f, "procedural"),
        }
    }
}

/// Consolidation level for summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsolidationLevel {
    Local,
    Cluster,
    Global,
}

impl fmt::Display for ConsolidationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Cluster => write!(f, "cluster"),
            Self::Global => write!(f, "global"),
        }
    }
}

// --- Full structs (with id, as returned from DB) ---

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
}

/// A bi-temporal fact derived from events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub id: i64,
    pub content: String,
    pub content_hash: String,
    pub embedding: Vec<f32>,
    pub fact_type: FactType,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub t_valid: Option<DateTime<Utc>>,
    pub t_invalid: Option<DateTime<Utc>>,
    pub source_event_id: Option<i64>,
    pub importance: f64,
    pub access_count: i64,
    pub last_accessed: DateTime<Utc>,
    pub metadata: serde_json::Value,
    pub scope_id: i64,
}

/// A graph edge between two facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: i64,
    pub source_fact_id: i64,
    pub target_fact_id: i64,
    pub relation_type: String,
    pub weight: f64,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub scope_id: i64,
}

/// A consolidation summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: i64,
    pub content: String,
    pub embedding: Vec<f32>,
    pub level: ConsolidationLevel,
    pub source_fact_ids: Vec<i64>,
    pub created_at: DateTime<Utc>,
    pub scope_id: i64,
}

// --- Scope types ---

/// A node in the hierarchical scope tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeNode {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub label: String,
    pub depth: i64,
}

/// How to resolve scopes for a search query.
/// Paths are consumer-facing strings (e.g., "user:michael/project:demo").
/// The engine resolves them to internal integer IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeQuery {
    /// Facts at exactly this scope path.
    Exact(String),
    /// Facts at this scope path and all descendants.
    Subtree(String),
    /// Facts at this scope path and all ancestors up to root.
    Ancestors(String),
    /// Facts at ancestors + at this scope path's subtree (full inherited context).
    Inherited(String),
}

// --- Options ---

/// Optional parameters for [`crate::engine::MemoryEngine::add_fact`].
///
/// All fields default to `None`, which uses the engine's defaults
/// (importance=0.5, metadata={}, no temporal bounds).
#[derive(Debug, Clone, Default)]
pub struct AddFactOptions {
    /// Override default importance (0.5). Must be in [0, 1].
    pub importance: Option<f64>,
    /// Override default metadata (empty object).
    pub metadata: Option<serde_json::Value>,
    /// Set the real-world validity start time.
    pub t_valid: Option<DateTime<Utc>>,
    /// Set the real-world validity end time.
    pub t_invalid: Option<DateTime<Utc>>,
}

// --- New* structs (without id, for insertion) ---

/// Event to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub payload: serde_json::Value,
    pub source: String,
    pub session_id: Option<String>,
    pub scope_id: i64,
}

/// Fact to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewFact {
    pub content: String,
    pub content_hash: String,
    pub embedding: Vec<f32>,
    pub fact_type: FactType,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub t_valid: Option<DateTime<Utc>>,
    pub t_invalid: Option<DateTime<Utc>>,
    pub source_event_id: Option<i64>,
    pub importance: f64,
    pub access_count: i64,
    pub last_accessed: DateTime<Utc>,
    pub metadata: serde_json::Value,
    pub scope_id: i64,
}

/// Edge to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEdge {
    pub source_fact_id: i64,
    pub target_fact_id: i64,
    pub relation_type: String,
    pub weight: f64,
    pub t_created: DateTime<Utc>,
    pub t_expired: Option<DateTime<Utc>>,
    pub scope_id: i64,
}

/// Summary to insert (DB assigns id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewSummary {
    pub content: String,
    pub embedding: Vec<f32>,
    pub level: ConsolidationLevel,
    pub source_fact_ids: Vec<i64>,
    pub created_at: DateTime<Utc>,
    pub scope_id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn fact_defaults_none_temporals() {
        let fact = Fact {
            id: 1,
            content: "test".into(),
            content_hash: "abc".into(),
            embedding: vec![0.1; 768],
            fact_type: FactType::Episodic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: Some(1),
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            scope_id: 1,
        };
        assert!(fact.t_expired.is_none());
        assert!(fact.t_valid.is_none());
    }
}
