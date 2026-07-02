//! Inspection statistics + dump-format DTOs (relocated from the monolith's
//! `inspect/types.rs`, Wave 2 #816 E.4b Phase B).
//!
//! Pure serde DTOs: `DumpFormat` (the export-file format selector) and the
//! `EngineStatistics` tree of aggregate stats. `EngineSnapshot` and its
//! sub-DTOs stay in the monolith — they are import/export wire types tied to
//! the engine's own snapshot machinery, not pure leaf data.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::ConsolidationLevel;

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
