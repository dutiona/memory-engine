//! Snapshot data-transfer types — the serde projections serialized into the
//! sidecar `.snapshot` file.
//!
//! These are **pure DTOs**: no `rusqlite`, no `fs`. The file IO + DB
//! fingerprinting that produces and consumes them is backend machinery (today
//! `crate::engine::snapshot`; `me-backend-sqlite` after Wave 2 #816). Homing the
//! DTOs in the data layer (`me-types`) lets the graph/scope projections, the
//! backend, and the facade all share one definition without anyone depending
//! "up" on the engine. Decoupled from internal representations so the wire
//! format stays stable across engine refactors.

use serde::{Deserialize, Serialize};

use crate::types::{RelationType, ScopeNode};

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub format_version: u32,
    pub fingerprint: DbFingerprint,
    pub embed_dim: usize,
    pub engine_version: String,
}

/// Composite fingerprint from the three source-of-truth tables.
///
/// Catches inserts (`max_*_id` changes) and soft-deletes (`active_*_count`
/// changes). **Not** based on the `events` table — many mutators
/// (`add_fact`, `forget`, `consolidate`, `link_session_facts`) modify
/// `facts`/`edges`/`scopes` without appending an event.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbFingerprint {
    pub max_fact_id: i64,
    pub active_fact_count: i64,
    pub max_edge_id: i64,
    pub active_edge_count: i64,
    pub max_scope_id: i64,
    pub scope_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub graph: GraphSnapshot,
    pub scope_tree: ScopeTreeSnapshot,
    /// Present when built with `ann` feature AND HNSW was active.
    /// `#[serde(default)]` allows non-ann snapshots to be loaded by ann builds
    /// and vice versa (named `MessagePack` handles missing fields).
    #[serde(default)]
    pub hnsw: Option<HnswSnapshot>,
}

/// Edge list — decoupled from petgraph `DiGraph` internals.
/// No isolated nodes: matches `MemoryGraph::load_from_db` semantics (edges only).
#[derive(Debug, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub edges: Vec<GraphEdgeSnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GraphEdgeSnapshot {
    pub edge_id: i64,
    pub source: i64,
    pub target: i64,
    pub relation_type: RelationType,
    pub weight: f64,
}

/// Flat list of scope nodes — `ScopeNode` is already serde.
#[derive(Debug, Serialize, Deserialize)]
pub struct ScopeTreeSnapshot {
    pub nodes: Vec<ScopeNode>,
}

/// Compact HNSW rebuild data: active fact embeddings only, no tombstones.
/// On load, rebuilds a fresh compact HNSW index (same as `build_from_db`).
#[derive(Debug, Serialize, Deserialize)]
pub struct HnswSnapshot {
    pub entries: Vec<HnswEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HnswEntry {
    pub fact_id: i64,
    pub embedding: Vec<f32>,
}
