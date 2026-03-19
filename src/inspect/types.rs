use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{ConsolidationLevel, Edge, Event, EventType, Fact, ScopeNode, Summary};

// ---------------------------------------------------------------------------
// Fact explanation
// ---------------------------------------------------------------------------

/// Full explanation of a single fact's current state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactExplanation {
    pub fact_id: i64,
    pub state: FactState,
    pub provenance: FactProvenance,
    pub graph_context: GraphContext,
    pub scope_path: String,
}

/// Current lifecycle state of a fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FactState {
    Active,
    Expired {
        reason: ExpiredReason,
    },
    Pinned,
    Invalidated {
        t_invalid: DateTime<Utc>,
    },
    Due {
        t_valid: DateTime<Utc>,
        surfaced: bool,
    },
}

/// Best-effort reason why a fact is expired.
///
/// Forgetting, conflict resolution, and deduplication do not currently emit
/// `MemoryOp` events, so most expired facts will return [`ExpiredReason::Unknown`]
/// until an event-based audit trail is added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExpiredReason {
    Forgotten,
    ConflictResolved { superseded_by: Option<i64> },
    Deduplicated { canonical_id: Option<i64> },
    Unknown,
}

/// Provenance metadata for a fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactProvenance {
    pub source_event_id: Option<i64>,
    pub source_event: Option<Event>,
    pub importance: f64,
    pub importance_score: f64,
    pub is_pinned: bool,
    pub access_count: i64,
}

/// Graph neighbourhood context for a fact.
///
/// For expired facts, this reflects the current graph state (active edges only).
/// Historical connectivity requires replaying events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphContext {
    pub degree: usize,
    pub neighbor_ids: Vec<i64>,
    pub component_size: usize,
}

// ---------------------------------------------------------------------------
// Temporal history
// ---------------------------------------------------------------------------

/// Temporal history of a single fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactHistory {
    pub fact_id: i64,
    pub timeline: Vec<FactHistoryEntry>,
}

/// A single entry in a fact's history timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactHistoryEntry {
    pub timestamp: DateTime<Utc>,
    pub kind: HistoryEventKind,
}

/// Kind of temporal event in a fact's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HistoryEventKind {
    Created,
    BecameValid,
    BecameInvalid,
    Expired,
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// Filter criteria for event replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayFilter {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub id_range: Option<(i64, i64)>,
    pub session_id: Option<String>,
    pub event_type: Option<EventType>,
    pub limit: Option<usize>,
    pub upcast: bool,
    pub order: ReplayOrder,
}

impl Default for ReplayFilter {
    fn default() -> Self {
        Self {
            since: None,
            until: None,
            id_range: None,
            session_id: None,
            event_type: None,
            limit: None,
            upcast: false,
            order: ReplayOrder::default(),
        }
    }
}

/// Ordering for event replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum ReplayOrder {
    #[default]
    InsertionOrder,
    TimestampOrder,
}

// ---------------------------------------------------------------------------
// Dump
// ---------------------------------------------------------------------------

/// Output format for a full engine dump.
#[derive(Debug, Clone, PartialEq)]
pub enum DumpFormat {
    Json(PathBuf),
    Sqlite(PathBuf),
}

/// Complete snapshot of engine state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSnapshot {
    pub schema_version: u32,
    pub storage_epoch: u16,
    pub embed_dim: usize,
    pub facts: Vec<Fact>,
    pub edges: Vec<Edge>,
    pub summaries: Vec<Summary>,
    pub scopes: Vec<ScopeNode>,
    pub events: Vec<Event>,
    pub config: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Aggregate statistics for the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineStatistics {
    pub facts: FactStats,
    pub edges: EdgeStats,
    pub summaries: SummaryStats,
    pub scopes: ScopeStats,
    pub events: EventStats,
    pub storage: StorageStats,
}

/// Fact-level statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactStats {
    pub total: i64,
    pub active: i64,
    pub expired: i64,
    pub pinned: i64,
    pub due: i64,
}

/// Edge-level statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeStats {
    pub total: i64,
    pub active: i64,
    pub expired: i64,
}

/// Summary-level statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryStats {
    pub total: i64,
    pub by_level: HashMap<ConsolidationLevel, i64>,
}

/// Scope tree statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeStats {
    pub total: i64,
    pub max_depth: i64,
}

/// Event log statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventStats {
    pub total: i64,
}

/// Storage-level statistics.
///
/// `main_db_bytes` excludes WAL/SHM sidecars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageStats {
    pub page_count: i64,
    pub page_size: i64,
    pub main_db_bytes: i64,
    pub file_path: Option<String>,
}
