//! Cold-storage archive manifest row (relocated from the monolith's
//! `archive/types.rs`, Wave 2 #816 E.4b Phase B).
//!
//! Gated behind the no-op `archive` feature mirroring the facade's own
//! `archive` feature (see `me-types`'s `Cargo.toml`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Logical content-schema version of a `.pak` archive — **backend-independent**.
///
/// A `.pak` is a portable `zstd` + `serde_json` blob of `me-types` DTOs (facts and
/// edges). Its compatibility gate therefore answers *"can this build deserialize and
/// consume the pak's contents?"* — a question about the **DTO shape**, not about any
/// backend's physical migration history. It is stamped on write
/// (`ArchivePak::engine_schema_version`) and checked on read; both sides MUST use
/// **this** constant, so they cannot drift apart.
///
/// # Why this is not a backend's schema version
///
/// It was previously taken from `me-backend-sqlite`'s `CURRENT_SCHEMA_VERSION`. That is a
/// *`SQLite` migration counter*, and it is **not comparable across backends** — Postgres
/// keeps its own, independently-numbered `CURRENT_PG_SCHEMA_VERSION` (= 1, while `SQLite`
/// is at 14; see `me-backend-postgres`'s migration docs, which explicitly forbid "syncing"
/// the two numbers). Binding a portable archive format to one backend's counter is a
/// category error: it only appeared to work while `SQLite` was the sole backend. Sourcing
/// both the write stamp and the read check from a single L0 constant keeps them symmetric
/// **by construction**, and keeps `.pak` files portable across backends (the point of
/// epic #628).
///
/// # Why the value is 14 and not 1
///
/// The numbering is **inherited, deliberately**. Every `.pak` written to date stamps the
/// then-current `SQLite` schema version, and the read gate rejects a pak whose stamp is
/// *greater* than what this build supports. Restarting the sequence at 1 would make every
/// archive already on disk (stamped 14) read as "from the future" and be rejected — a
/// silent data-loss regression. So the sequence continues from 14 and advances only when
/// the `.pak` **content schema** changes.
///
/// (Wave 2 #816 / S4, sub-PR 3a.)
pub const ARCHIVE_SCHEMA_VERSION: u32 = 14;

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
