use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{
    ConsolidationLevel, Edge, Event, EventType, Fact, LineageSnapshotEntry, ScopeNode, Summary,
};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        surfaced_at: Option<DateTime<Utc>>,
    },
}

/// Best-effort reason why a fact is expired.
///
/// Forgetting, conflict resolution, and deduplication do not currently emit
/// `MemoryOp` events, so most expired facts will return [`ExpiredReason::Unknown`]
/// until an event-based audit trail is added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The originating event, fetched via upcasted read when `source_event_id`
    /// is `Some`. `None` for facts created without a `source_event_id`.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphContext {
    pub degree: usize,
    pub neighbor_ids: Vec<i64>,
    pub component_size: usize,
}

// ---------------------------------------------------------------------------
// Temporal history
// ---------------------------------------------------------------------------

/// Temporal history of a single fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactHistory {
    pub fact_id: i64,
    pub timeline: Vec<FactHistoryEntry>,
}

/// A single entry in a fact's history timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactHistoryEntry {
    pub timestamp: DateTime<Utc>,
    pub kind: HistoryEventKind,
}

/// Kind of temporal event in a fact's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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

/// Ordering for event replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReplayOrder {
    #[default]
    InsertionOrder,
    TimestampOrder,
}

// ---------------------------------------------------------------------------
// Dump
// ---------------------------------------------------------------------------

/// Output format for a full engine dump.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DumpFormat {
    /// Plain JSON (uncompressed).
    Json(PathBuf),
    /// Gzip-compressed JSON. Requires the `compress-gzip` feature.
    JsonGzip(PathBuf),
    /// Zstandard-compressed JSON. Requires the `compress-zstd` feature.
    JsonZstd(PathBuf),
    /// Atomic `SQLite` backup via `VACUUM INTO` (file-backed and in-memory engines).
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
    /// Lineage records for promoted wisdom facts (Phase 5a).
    /// Absent in pre-v8 snapshots — defaults to empty.
    #[serde(default)]
    pub lineage: Vec<LineageSnapshotEntry>,
    pub config: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Aggregate statistics for the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStatistics {
    pub facts: FactStats,
    pub edges: EdgeStats,
    pub summaries: SummaryStats,
    pub scopes: ScopeStats,
    pub events: EventStats,
    pub storage: StorageStats,
}

/// Fact-level statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactStats {
    pub total: i64,
    pub active: i64,
    pub expired: i64,
    pub pinned: i64,
    pub due: i64,
}

/// Edge-level statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeStats {
    pub total: i64,
    pub active: i64,
    pub expired: i64,
}

/// Summary-level statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryStats {
    pub total: i64,
    pub by_level: BTreeMap<ConsolidationLevel, i64>,
}

/// Scope tree statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeStats {
    pub total: i64,
    pub max_depth: i64,
}

/// Event log statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStats {
    pub total: i64,
}

/// Storage-level statistics.
///
/// `main_db_bytes` excludes WAL/SHM sidecars.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageStats {
    pub page_count: i64,
    pub page_size: i64,
    pub main_db_bytes: i64,
    pub file_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    use crate::types::{FactType, PromotionProvenance};

    /// `serialize → deserialize` must round-trip to a value equal to the original.
    /// These types are the engine's import/export wire format (`EngineSnapshot`
    /// and friends), so a serde drift here silently corrupts dump/restore.
    fn roundtrip<T>(value: &T) -> T
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let json = serde_json::to_string(value).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    /// Assert `serialize → deserialize == original` for a `PartialEq` type.
    fn assert_roundtrip_eq<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        assert_eq!(&roundtrip(value), value);
    }

    /// Assert the JSON is stable across a `serialize → deserialize → serialize`
    /// cycle. Used for the few snapshot types that are not `PartialEq`
    /// (`EngineSnapshot`, `EngineStatistics`) but still must survive the wire.
    fn assert_roundtrip_json_stable<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let json1 = serde_json::to_value(value).expect("serialize #1");
        let back: T = serde_json::from_value(json1.clone()).expect("deserialize");
        let json2 = serde_json::to_value(&back).expect("serialize #2");
        assert_eq!(json1, json2);
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    fn sample_fact() -> Fact {
        Fact {
            id: 7,
            content: "a representative fact".into(),
            content_hash: "deadbeefcafebabe0011223344556677".into(),
            embedding: vec![0.1, -0.2, 0.3, 0.4],
            fact_type: FactType::Semantic,
            t_created: ts(1_000),
            t_expired: Some(ts(2_000)),
            t_valid: Some(ts(1_500)),
            t_invalid: None,
            source_event_id: Some(3),
            importance: 0.75,
            access_count: 12,
            last_accessed: ts(1_800),
            metadata: serde_json::json!({"k": "v", "n": 42}),
            scope_id: 4,
            is_pinned: true,
            importance_score: 0.66,
            surfaced_at: Some(ts(1_900)),
        }
    }

    fn sample_edge() -> Edge {
        Edge {
            id: 9,
            source_fact_id: 7,
            target_fact_id: 8,
            relation_type: "supports".into(),
            weight: 0.42,
            t_created: ts(1_000),
            t_expired: None,
            scope_id: 4,
        }
    }

    fn sample_event() -> Event {
        Event {
            id: 11,
            timestamp: ts(1_234),
            event_type: EventType::OutcomeSignal,
            payload: serde_json::json!({"fact_id": 7, "outcome": "Positive"}),
            source: "agent".into(),
            session_id: Some("sess-1".into()),
            scope_id: 4,
            origin_node_id: "node-a".into(),
            sequence_id: 5,
            created_at: Some(ts(1_240)),
            event_revision: 2,
        }
    }

    fn sample_summary() -> Summary {
        Summary {
            id: 13,
            content: "cluster summary".into(),
            embedding: vec![0.5, 0.5, 0.5, 0.5],
            level: ConsolidationLevel::Cluster,
            source_fact_ids: vec![7, 8, 9],
            created_at: ts(2_000),
            scope_id: 4,
        }
    }

    fn sample_scope() -> ScopeNode {
        ScopeNode {
            id: 4,
            parent_id: Some(1),
            label: "project:demo".into(),
            depth: 2,
        }
    }

    fn sample_lineage() -> LineageSnapshotEntry {
        LineageSnapshotEntry {
            lineage_id: 21,
            wisdom_fact_id: 7,
            source_fact_ids: vec![1, 2, 3],
            provenance: PromotionProvenance {
                source_count: 3,
                session_count: 2,
                date_range_start: ts(1_000),
                date_range_end: ts(2_000),
                confidence: 0.9,
                method_version: "v1".into(),
                representative_ids: vec![1, 2],
                lineage_id: 21,
            },
        }
    }

    // --- Fact explanation -------------------------------------------------

    #[test]
    fn fact_state_variants_roundtrip() {
        // Each variant carries distinct payload shapes — exercise them all so a
        // serde rename/reshape can't slip a variant through untested.
        for state in [
            FactState::Active,
            FactState::Pinned,
            FactState::Expired {
                reason: ExpiredReason::Unknown,
            },
            FactState::Expired {
                reason: ExpiredReason::Forgotten,
            },
            FactState::Expired {
                reason: ExpiredReason::ConflictResolved {
                    superseded_by: Some(99),
                },
            },
            FactState::Expired {
                reason: ExpiredReason::Deduplicated { canonical_id: None },
            },
            FactState::Invalidated {
                t_invalid: ts(3_000),
            },
            FactState::Due {
                t_valid: ts(1_500),
                surfaced_at: Some(ts(1_600)),
            },
            FactState::Due {
                t_valid: ts(1_500),
                surfaced_at: None,
            },
        ] {
            assert_roundtrip_eq(&state);
        }
    }

    #[test]
    fn expired_reason_variants_roundtrip() {
        for reason in [
            ExpiredReason::Forgotten,
            ExpiredReason::ConflictResolved {
                superseded_by: Some(5),
            },
            ExpiredReason::Deduplicated {
                canonical_id: Some(6),
            },
            ExpiredReason::Unknown,
        ] {
            assert_roundtrip_eq(&reason);
        }
    }

    #[test]
    fn fact_provenance_roundtrip() {
        // Both the Some(event) and None branches of source_event.
        assert_roundtrip_eq(&FactProvenance {
            source_event_id: Some(3),
            source_event: Some(sample_event()),
            importance: 0.5,
            importance_score: 0.4,
            is_pinned: false,
            access_count: 7,
        });
        assert_roundtrip_eq(&FactProvenance {
            source_event_id: None,
            source_event: None,
            importance: 0.1,
            importance_score: 0.2,
            is_pinned: true,
            access_count: 0,
        });
    }

    #[test]
    fn graph_context_roundtrip() {
        assert_roundtrip_eq(&GraphContext {
            degree: 3,
            neighbor_ids: vec![1, 2, 3],
            component_size: 10,
        });
    }

    #[test]
    fn fact_explanation_roundtrip() {
        assert_roundtrip_eq(&FactExplanation {
            fact_id: 7,
            state: FactState::Due {
                t_valid: ts(1_500),
                surfaced_at: None,
            },
            provenance: FactProvenance {
                source_event_id: Some(3),
                source_event: Some(sample_event()),
                importance: 0.5,
                importance_score: 0.4,
                is_pinned: false,
                access_count: 7,
            },
            graph_context: GraphContext {
                degree: 2,
                neighbor_ids: vec![8, 9],
                component_size: 4,
            },
            scope_path: "user:michael/project:demo".into(),
        });
    }

    // --- Temporal history -------------------------------------------------

    #[test]
    fn history_event_kind_variants_roundtrip() {
        for kind in [
            HistoryEventKind::Created,
            HistoryEventKind::BecameValid,
            HistoryEventKind::BecameInvalid,
            HistoryEventKind::Expired,
        ] {
            assert_roundtrip_eq(&kind);
        }
    }

    #[test]
    fn fact_history_roundtrip() {
        assert_roundtrip_eq(&FactHistory {
            fact_id: 7,
            timeline: vec![
                FactHistoryEntry {
                    timestamp: ts(1_000),
                    kind: HistoryEventKind::Created,
                },
                FactHistoryEntry {
                    timestamp: ts(1_500),
                    kind: HistoryEventKind::BecameValid,
                },
                FactHistoryEntry {
                    timestamp: ts(2_000),
                    kind: HistoryEventKind::Expired,
                },
            ],
        });
    }

    // --- Replay -----------------------------------------------------------

    #[test]
    fn replay_order_variants_roundtrip() {
        for order in [ReplayOrder::InsertionOrder, ReplayOrder::TimestampOrder] {
            assert_roundtrip_eq(&order);
        }
    }

    #[test]
    fn replay_filter_roundtrip() {
        // Fully-populated filter.
        assert_roundtrip_eq(&ReplayFilter {
            since: Some(ts(1_000)),
            until: Some(ts(2_000)),
            id_range: Some((10, 20)),
            session_id: Some("sess-7".into()),
            event_type: Some(EventType::ToolCall),
            limit: Some(100),
            upcast: true,
            order: ReplayOrder::TimestampOrder,
        });
        // Default (all-None) filter.
        assert_roundtrip_eq(&ReplayFilter::default());
    }

    // --- Statistics -------------------------------------------------------

    #[test]
    fn fact_stats_roundtrip() {
        assert_roundtrip_eq(&FactStats {
            total: 100,
            active: 80,
            expired: 15,
            pinned: 5,
            due: 3,
        });
    }

    #[test]
    fn edge_stats_roundtrip() {
        assert_roundtrip_eq(&EdgeStats {
            total: 50,
            active: 40,
            expired: 10,
        });
    }

    #[test]
    fn summary_stats_roundtrip() {
        let mut by_level = BTreeMap::new();
        by_level.insert(ConsolidationLevel::Local, 3);
        by_level.insert(ConsolidationLevel::Cluster, 2);
        by_level.insert(ConsolidationLevel::Global, 1);
        assert_roundtrip_eq(&SummaryStats { total: 6, by_level });
    }

    #[test]
    fn scope_stats_roundtrip() {
        assert_roundtrip_eq(&ScopeStats {
            total: 12,
            max_depth: 4,
        });
    }

    #[test]
    fn event_stats_roundtrip() {
        assert_roundtrip_eq(&EventStats { total: 999 });
    }

    #[test]
    fn storage_stats_roundtrip() {
        assert_roundtrip_eq(&StorageStats {
            page_count: 100,
            page_size: 4_096,
            main_db_bytes: 409_600,
            file_path: Some("/tmp/mem.db".into()),
        });
        assert_roundtrip_eq(&StorageStats {
            page_count: 0,
            page_size: 4_096,
            main_db_bytes: 0,
            file_path: None,
        });
    }

    #[test]
    fn engine_statistics_roundtrip() {
        let mut by_level = BTreeMap::new();
        by_level.insert(ConsolidationLevel::Cluster, 2);
        assert_roundtrip_eq(&EngineStatistics {
            facts: FactStats {
                total: 10,
                active: 8,
                expired: 1,
                pinned: 1,
                due: 0,
            },
            edges: EdgeStats {
                total: 5,
                active: 4,
                expired: 1,
            },
            summaries: SummaryStats { total: 2, by_level },
            scopes: ScopeStats {
                total: 3,
                max_depth: 2,
            },
            events: EventStats { total: 20 },
            storage: StorageStats {
                page_count: 7,
                page_size: 4_096,
                main_db_bytes: 28_672,
                file_path: None,
            },
        });
    }

    // --- Snapshot (import/export format) ----------------------------------

    #[test]
    fn engine_snapshot_roundtrip() {
        let mut config = BTreeMap::new();
        config.insert("schema_version".to_string(), "11".to_string());
        config.insert("last_consolidated_at".to_string(), ts(2_000).to_rfc3339());

        let snapshot = EngineSnapshot {
            schema_version: 11,
            storage_epoch: 1,
            embed_dim: 4,
            facts: vec![sample_fact()],
            edges: vec![sample_edge()],
            summaries: vec![sample_summary()],
            scopes: vec![sample_scope()],
            events: vec![sample_event()],
            lineage: vec![sample_lineage()],
            config,
        };
        // EngineSnapshot is not PartialEq; assert JSON stability instead.
        assert_roundtrip_json_stable(&snapshot);
    }

    #[test]
    fn engine_snapshot_lineage_defaults_when_absent() {
        // Pre-v8 snapshots omit `lineage`; #[serde(default)] must fill an empty
        // vec rather than fail deserialization (back-compat for old archives).
        let json = serde_json::json!({
            "schema_version": 7,
            "storage_epoch": 1,
            "embed_dim": 4,
            "facts": [],
            "edges": [],
            "summaries": [],
            "scopes": [],
            "events": [],
            "config": {}
        });
        let snapshot: EngineSnapshot =
            serde_json::from_value(json).expect("pre-v8 snapshot must deserialize");
        assert!(snapshot.lineage.is_empty());
    }
}
