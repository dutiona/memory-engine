//! Cold-storage archive manifest row (relocated from the monolith's
//! `archive/types.rs`, Wave 2 #816 E.4b Phase B).
//!
//! Gated behind the no-op `archive` feature mirroring the facade's own
//! `archive` feature (see `me-types`'s `Cargo.toml`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A row from the `archive_manifest` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifestEntry {
    /// Auto-assigned primary key from `archive_manifest.id`.
    pub id: i64,
    /// Relative path to the `.pak` file from the archive directory; never contains
    /// `..` or an absolute anchor (enforced by the path-traversal guard on read).
    pub pak_path: String,
    /// System timestamp when the archival commit completed (wall-clock, UTC).
    pub created_at: DateTime<Utc>,
    /// Number of facts stored in the pak.
    pub fact_count: i64,
    /// Number of edges stored in the pak.
    pub edge_count: i64,
    /// Smallest fact id in the pak (used for manifest-level skip optimization).
    pub fact_id_min: i64,
    /// Largest fact id in the pak (used for manifest-level skip optimization).
    pub fact_id_max: i64,
    /// Earliest `t_created` timestamp among facts in the pak (system time).
    pub t_created_min: DateTime<Utc>,
    /// Latest `t_created` timestamp among facts in the pak (system time).
    pub t_created_max: DateTime<Utc>,
    /// Compressed file size in bytes.
    pub size_bytes: i64,
    /// Blake3 hex digest of the compressed pak file for integrity verification.
    pub blake3_hash: String,
}
