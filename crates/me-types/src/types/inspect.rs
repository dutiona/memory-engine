//! Inspection statistics + dump-format DTOs (relocated from the monolith's
//! `inspect/types.rs`, Wave 2 #816 E.4b Phase B).
//!
//! Pure serde DTOs: `DumpFormat` (the export-file format selector) and the
//! `EngineStatistics` tree of aggregate stats.
//!
//! `EngineSnapshot` (plus its `EmbeddingSpaceSnapshot`/`FactVectorSnapshot`
//! sub-DTOs) joined them in the S2 me-backend-sqlite carve (sub-PR 2b): the
//! backend's `stream_snapshot`/`dump_json` construct it directly and cannot
//! reach back into the facade, whose `inspect/types.rs` re-exports all of
//! these so `crate::inspect::types::*` keeps resolving for every consumer
//! (CLI/MCP import-export included).

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{
    ConsolidationLevel, Edge, EmbeddingFingerprint, Event, Fact, LineageSnapshotEntry, ScopeNode,
    Summary,
};

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

// ---------------------------------------------------------------------------
// Snapshot (import/export wire format)
// ---------------------------------------------------------------------------

/// One embedding-space registry row in a snapshot (#622).
///
/// Carries the `embedding_spaces` table content so the embedding identity survives a
/// dump→restore. Before #622 the identity was a `config` row (`embedding_meta`) that
/// round-tripped through the generic config copy; it now lives in its own table, so it is
/// serialized explicitly here. A pre-#622 snapshot has no `embedding_spaces` field — restore
/// falls back to translating the legacy `embedding_meta` config value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingSpaceSnapshot {
    /// Space name (PK); `"default"` for the degenerate single space.
    pub name: String,
    /// Lifecycle status TEXT: `active` / `populating` / `deprecated`.
    pub status: String,
    /// The canonical identity tuple, flattened to the row's identity columns.
    #[serde(flatten)]
    pub fingerprint: EmbeddingFingerprint,
}

/// One `fact_vectors` row in a snapshot (#623 background reconstruction).
///
/// Carries a **non-active** space's per-fact vector: the `populating` space's
/// vectors mid-reconstruction, or a `deprecated` space's vectors retained for
/// rollback after a promote. The **active** space's vectors stay in
/// `facts[].embedding` (the [`EngineSnapshot::facts`] rows are unchanged), so this
/// section is purely additive — a pre-#623 snapshot has no `fact_vectors` field
/// and restore defaults it empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactVectorSnapshot {
    /// Owning fact id (FK → `facts.id`).
    pub fact_id: i64,
    /// Owning space name (FK → `embedding_spaces.name`).
    pub space_id: String,
    /// The stored vector.
    pub embedding: Vec<f32>,
}

/// One `lifecycle_receipts` row in a snapshot (#1025): a delivery's idempotency key, with the
/// event it stored or the metadata of the body it quarantined.
///
/// A restore keeps the keys, so a client's resend after a restore is still a `duplicate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleReceiptSnapshot {
    /// The machine the token named.
    pub machine: String,
    /// `ME-Event-Id`, as sent.
    pub event_id: String,
    /// The SHA-256 of the body: 64 lowercase hex digits.
    pub body_sha256: String,
    /// When the server received it.
    pub received_at: DateTime<Utc>,
    /// The stored event (FK → `events.id`); `None` for a quarantined body.
    pub event_row: Option<i64>,
    /// The ME call record the event's result named (FK → `events.id`).
    pub call_event: Option<i64>,
    /// The quarantine record; `None` for a stored event. Exactly one of this and `event_row`.
    pub quarantine: Option<serde_json::Value>,
}

/// One `lifecycle_segments` row in a snapshot (#1025): a session segment, whose ended row is
/// the checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleSegmentSnapshot {
    /// The row's id, kept by a restore.
    pub id: i64,
    /// The machine the session ran on.
    pub machine: String,
    /// The session's identifier.
    pub session_id: String,
    /// The segment's number within the session, from 1.
    pub number: i64,
    /// The order key of its first event, as text.
    pub start_key: String,
    /// The order key of its last event, as text.
    pub last_key: String,
    /// When the server received the session's latest event.
    pub received_at: DateTime<Utc>,
    /// Events per kind.
    pub counts: serde_json::Value,
    /// Main-agent prompts and stops: more than 0 makes an ended segment a checkpoint.
    pub main_turns: i64,
    /// The order key of its last main-agent final message.
    pub final_key: Option<String>,
    /// The memory made from it (FK → `facts.id`).
    pub final_fact: Option<i64>,
    /// How it ended; `None` while it is open.
    pub ended_by: Option<String>,
    /// When it ended.
    pub ended_at: Option<DateTime<Utc>>,
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
    /// Embedding-space registry rows (#622). Absent in pre-#622 snapshots — defaults to
    /// empty, and restore then reconstructs the identity from the legacy `embedding_meta`
    /// config value.
    #[serde(default)]
    pub embedding_spaces: Vec<EmbeddingSpaceSnapshot>,
    /// `fact_vectors` rows for the non-active embedding spaces (#623). Absent in
    /// pre-#623 snapshots — defaults to empty (the active vectors live in
    /// `facts[].embedding`, so an old snapshot loses nothing).
    #[serde(default)]
    pub fact_vectors: Vec<FactVectorSnapshot>,
    /// The forget retraction's tombstones (#1023). Absent in pre-v16 snapshots — defaults
    /// to empty. Without them a restored store would let forgotten content back in and could
    /// not unforget.
    #[serde(default)]
    pub tombstones: Vec<super::Tombstone>,
    /// The lifecycle route's receipts (#1025). Absent in pre-v18 snapshots — defaults to empty.
    /// Without them a restored store would take a resent event twice and lose its quarantine
    /// count.
    #[serde(default)]
    pub lifecycle_receipts: Vec<LifecycleReceiptSnapshot>,
    /// The lifecycle route's segments (#1025). Absent in pre-v18 snapshots — defaults to empty.
    #[serde(default)]
    pub lifecycle_segments: Vec<LifecycleSegmentSnapshot>,
    pub config: BTreeMap<String, String>,
    /// The store's id high-water marks, `sqlite_sequence` as `table → seq` (#1131): the
    /// highest id each table ever gave, erased and archived rows included. A restore starts
    /// each table's ids above it, so no id is reused. Absent, so empty, in a pre-#1131
    /// snapshot, which restores its marks from its surviving rows.
    #[serde(default)]
    pub sequences: BTreeMap<String, i64>,
}
