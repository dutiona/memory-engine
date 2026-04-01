//! Sidecar snapshot for fast cold-start.
//!
//! Serializes `MemoryGraph`, `ScopeTree`, and (optionally) HNSW state into a
//! `.snapshot` file next to the database. On startup, if the snapshot validates
//! against the current DB fingerprint, the engine loads from it instead of
//! doing a full SQLite scan. Any failure falls back to full rebuild (current behavior).
//!
//! ## File format
//!
//! ```text
//! [header_len: u32 LE][header (MessagePack, named)][payload (MessagePack, named)][blake3: 32 bytes]
//! ```
//!
//! The blake3 checksum covers the payload bytes only. Header is checked first
//! (cheap) so format_version/embed_dim mismatches reject without hashing.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, Result};
use crate::types::ScopeNode;

/// Current snapshot format version. Bump on breaking changes to the snapshot
/// layout or type definitions.
pub(crate) const FORMAT_VERSION: u32 = 1;

/// Size of the blake3 checksum appended to the file.
const BLAKE3_LEN: usize = 32;

// ---------------------------------------------------------------------------
// Snapshot types (decoupled from internal representations)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SnapshotHeader {
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
pub(crate) struct DbFingerprint {
    pub max_fact_id: i64,
    pub active_fact_count: i64,
    pub max_edge_id: i64,
    pub active_edge_count: i64,
    pub max_scope_id: i64,
    pub scope_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SnapshotPayload {
    pub graph: GraphSnapshot,
    pub scope_tree: ScopeTreeSnapshot,
    /// Present when built with `ann` feature AND HNSW was active.
    /// `#[serde(default)]` allows non-ann snapshots to be loaded by ann builds
    /// and vice versa (named MessagePack handles missing fields).
    #[serde(default)]
    pub hnsw: Option<HnswSnapshot>,
}

/// Edge list — decoupled from petgraph `DiGraph` internals.
/// No isolated nodes: matches `MemoryGraph::load_from_db` semantics (edges only).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GraphSnapshot {
    pub edges: Vec<GraphEdgeSnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GraphEdgeSnapshot {
    pub edge_id: i64,
    pub source: i64,
    pub target: i64,
    pub relation_type: String,
    pub weight: f64,
}

/// Flat list of scope nodes — `ScopeNode` is already serde.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ScopeTreeSnapshot {
    pub nodes: Vec<ScopeNode>,
}

/// Compact HNSW rebuild data: active fact embeddings only, no tombstones.
/// On load, rebuilds a fresh compact HNSW index (same as `build_from_db`).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct HnswSnapshot {
    pub entries: Vec<HnswEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct HnswEntry {
    pub fact_id: i64,
    pub embedding: Vec<f32>,
}

// ---------------------------------------------------------------------------
// Path derivation
// ---------------------------------------------------------------------------

/// Derive the snapshot sidecar path from the database path.
pub(crate) fn snapshot_path(db_path: &Path) -> PathBuf {
    let mut p = db_path.as_os_str().to_owned();
    p.push(".snapshot");
    PathBuf::from(p)
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

/// Read a composite fingerprint from the three source-of-truth tables.
///
/// Uses a single query with subselects for atomicity within the SQLite
/// read transaction.
pub(crate) fn read_fingerprint(conn: &Connection) -> Result<DbFingerprint> {
    conn.query_row(
        "SELECT \
            COALESCE((SELECT MAX(id) FROM facts), 0), \
            (SELECT COUNT(*) FROM facts WHERE t_expired IS NULL), \
            COALESCE((SELECT MAX(id) FROM edges), 0), \
            (SELECT COUNT(*) FROM edges WHERE t_expired IS NULL), \
            COALESCE((SELECT MAX(id) FROM scopes), 0), \
            (SELECT COUNT(*) FROM scopes)",
        [],
        |row| {
            Ok(DbFingerprint {
                max_fact_id: row.get(0)?,
                active_fact_count: row.get(1)?,
                max_edge_id: row.get(2)?,
                active_edge_count: row.get(3)?,
                max_scope_id: row.get(4)?,
                scope_count: row.get(5)?,
            })
        },
    )
    .map_err(MemoryError::from)
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/// Write a snapshot to disk atomically.
///
/// 1. Serialize header and payload with `rmp_serde::to_vec_named` (named
///    MessagePack for forward/backward compatibility).
/// 2. Compute blake3 hash over the payload bytes.
/// 3. Write to a temp file in the same directory (`NamedTempFile` uses
///    `O_CREAT | O_EXCL` — no symlink follow).
/// 4. `fsync` the file.
/// 5. Atomic rename via `persist()`.
/// 6. `fsync` the parent directory for rename durability.
pub(crate) fn write_to_file(
    header: &SnapshotHeader,
    payload: &SnapshotPayload,
    path: &Path,
) -> Result<()> {
    let header_bytes = rmp_serde::to_vec_named(header)
        .map_err(|e| MemoryError::Internal(format!("snapshot header serialize: {e}")))?;
    let payload_bytes = rmp_serde::to_vec_named(payload)
        .map_err(|e| MemoryError::Internal(format!("snapshot payload serialize: {e}")))?;

    let checksum = blake3::hash(&payload_bytes);

    let header_len = u32::try_from(header_bytes.len())
        .map_err(|_| MemoryError::Internal("snapshot header too large".into()))?;

    // Write to a temp file in the same directory for atomic rename.
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;

    tmp.write_all(&header_len.to_le_bytes())?;
    tmp.write_all(&header_bytes)?;
    tmp.write_all(&payload_bytes)?;
    tmp.write_all(checksum.as_bytes())?;
    tmp.as_file().sync_all()?;

    // Atomic rename. `persist` overwrites the target if it exists.
    tmp.persist(path).map_err(|e| MemoryError::Io(e.error))?;

    // fsync the parent directory so the rename is durable across power loss.
    if let Ok(dir_fd) = fs::File::open(dir) {
        let _ = dir_fd.sync_all();
    }

    Ok(())
}

/// Load and validate a snapshot from disk.
///
/// Returns `None` on any failure (missing file, corrupt data, version
/// mismatch, checksum failure, embed_dim mismatch). Never errors — all
/// failures are logged and treated as "snapshot unavailable".
pub(crate) fn load_from_file(
    path: &Path,
    embed_dim: usize,
) -> Option<(SnapshotHeader, SnapshotPayload)> {
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), error = %e, "snapshot read failed");
            }
            return None;
        }
    };

    // Minimum size: 4 (header_len) + 1 (min header) + 1 (min payload) + 32 (blake3)
    if data.len() < 4 + 1 + 1 + BLAKE3_LEN {
        tracing::warn!(path = %path.display(), "snapshot too small");
        return None;
    }

    // Parse header_len
    let header_len = u32::from_le_bytes(data[..4].try_into().expect("4 bytes")) as usize;
    let header_end = 4 + header_len;

    if header_end >= data.len().saturating_sub(BLAKE3_LEN) {
        tracing::warn!(path = %path.display(), "snapshot header_len out of bounds");
        return None;
    }

    let header_bytes = &data[4..header_end];
    let payload_end = data.len() - BLAKE3_LEN;
    let payload_bytes = &data[header_end..payload_end];
    let stored_checksum = &data[payload_end..];

    // 1. Deserialize header first (cheap reject on version/dim mismatch)
    let header: SnapshotHeader = match rmp_serde::from_slice(header_bytes) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "snapshot header deserialize failed");
            return None;
        }
    };

    if header.format_version != FORMAT_VERSION {
        tracing::info!(
            path = %path.display(),
            snapshot_version = header.format_version,
            expected = FORMAT_VERSION,
            "snapshot format version mismatch, discarding"
        );
        return None;
    }

    if header.embed_dim != embed_dim {
        tracing::warn!(
            path = %path.display(),
            snapshot_dim = header.embed_dim,
            expected = embed_dim,
            "snapshot embed_dim mismatch"
        );
        return None;
    }

    // 2. Verify blake3 checksum
    let computed = blake3::hash(payload_bytes);
    if computed.as_bytes() != stored_checksum {
        tracing::warn!(path = %path.display(), "snapshot checksum mismatch");
        return None;
    }

    // 3. Deserialize payload
    let payload: SnapshotPayload = match rmp_serde::from_slice(payload_bytes) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "snapshot payload deserialize failed");
            return None;
        }
    };

    Some((header, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_path_derivation() {
        let p = snapshot_path(Path::new("/data/memory.db"));
        assert_eq!(p, PathBuf::from("/data/memory.db.snapshot"));
    }

    #[test]
    fn snapshot_path_no_extension() {
        let p = snapshot_path(Path::new("/data/mydb"));
        assert_eq!(p, PathBuf::from("/data/mydb.snapshot"));
    }

    #[test]
    fn fingerprint_empty_db() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::schema::init_schema(&conn).unwrap();
        let fp = read_fingerprint(&conn).unwrap();
        assert_eq!(fp.max_fact_id, 0);
        assert_eq!(fp.active_fact_count, 0);
        assert_eq!(fp.max_edge_id, 0);
        assert_eq!(fp.active_edge_count, 0);
        // scopes table has the root scope after init
        assert!(fp.max_scope_id >= 1);
        assert!(fp.scope_count >= 1);
    }

    #[test]
    fn corrupt_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db.snapshot");
        fs::write(&path, b"garbage data here").unwrap();
        assert!(load_from_file(&path, 128).is_none());
    }

    #[test]
    fn missing_file_returns_none() {
        let path = Path::new("/nonexistent/path/test.db.snapshot");
        assert!(load_from_file(path, 128).is_none());
    }

    #[test]
    fn bad_checksum_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db.snapshot");

        let header = SnapshotHeader {
            format_version: FORMAT_VERSION,
            fingerprint: DbFingerprint {
                max_fact_id: 0,
                active_fact_count: 0,
                max_edge_id: 0,
                active_edge_count: 0,
                max_scope_id: 1,
                scope_count: 1,
            },
            embed_dim: 128,
            engine_version: "test".into(),
        };
        let payload = SnapshotPayload {
            graph: GraphSnapshot { edges: vec![] },
            scope_tree: ScopeTreeSnapshot { nodes: vec![] },
            hnsw: None,
        };

        let header_bytes = rmp_serde::to_vec_named(&header).unwrap();
        let payload_bytes = rmp_serde::to_vec_named(&payload).unwrap();
        let header_len = (header_bytes.len() as u32).to_le_bytes();

        // Write with wrong checksum
        let mut data = Vec::new();
        data.extend_from_slice(&header_len);
        data.extend_from_slice(&header_bytes);
        data.extend_from_slice(&payload_bytes);
        data.extend_from_slice(&[0u8; 32]); // zeroed checksum
        fs::write(&path, &data).unwrap();

        assert!(load_from_file(&path, 128).is_none());
    }

    #[test]
    fn wrong_format_version_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db.snapshot");

        let header = SnapshotHeader {
            format_version: 999,
            fingerprint: DbFingerprint {
                max_fact_id: 0,
                active_fact_count: 0,
                max_edge_id: 0,
                active_edge_count: 0,
                max_scope_id: 1,
                scope_count: 1,
            },
            embed_dim: 128,
            engine_version: "test".into(),
        };
        let payload = SnapshotPayload {
            graph: GraphSnapshot { edges: vec![] },
            scope_tree: ScopeTreeSnapshot { nodes: vec![] },
            hnsw: None,
        };

        // Use correct checksum but wrong version
        let header_bytes = rmp_serde::to_vec_named(&header).unwrap();
        let payload_bytes = rmp_serde::to_vec_named(&payload).unwrap();
        let checksum = blake3::hash(&payload_bytes);
        let header_len = (header_bytes.len() as u32).to_le_bytes();

        let mut data = Vec::new();
        data.extend_from_slice(&header_len);
        data.extend_from_slice(&header_bytes);
        data.extend_from_slice(&payload_bytes);
        data.extend_from_slice(checksum.as_bytes());
        fs::write(&path, &data).unwrap();

        assert!(load_from_file(&path, 128).is_none());
    }

    #[test]
    fn wrong_embed_dim_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db.snapshot");

        let header = SnapshotHeader {
            format_version: FORMAT_VERSION,
            fingerprint: DbFingerprint {
                max_fact_id: 0,
                active_fact_count: 0,
                max_edge_id: 0,
                active_edge_count: 0,
                max_scope_id: 1,
                scope_count: 1,
            },
            embed_dim: 256, // mismatch — we'll request 128
            engine_version: "test".into(),
        };
        let payload = SnapshotPayload {
            graph: GraphSnapshot { edges: vec![] },
            scope_tree: ScopeTreeSnapshot { nodes: vec![] },
            hnsw: None,
        };

        let header_bytes = rmp_serde::to_vec_named(&header).unwrap();
        let payload_bytes = rmp_serde::to_vec_named(&payload).unwrap();
        let checksum = blake3::hash(&payload_bytes);
        let header_len = (header_bytes.len() as u32).to_le_bytes();

        let mut data = Vec::new();
        data.extend_from_slice(&header_len);
        data.extend_from_slice(&header_bytes);
        data.extend_from_slice(&payload_bytes);
        data.extend_from_slice(checksum.as_bytes());
        fs::write(&path, &data).unwrap();

        assert!(load_from_file(&path, 128).is_none());
    }

    #[test]
    fn round_trip_write_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db.snapshot");

        let header = SnapshotHeader {
            format_version: FORMAT_VERSION,
            fingerprint: DbFingerprint {
                max_fact_id: 42,
                active_fact_count: 10,
                max_edge_id: 7,
                active_edge_count: 5,
                max_scope_id: 3,
                scope_count: 3,
            },
            embed_dim: 128,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let payload = SnapshotPayload {
            graph: GraphSnapshot {
                edges: vec![GraphEdgeSnapshot {
                    edge_id: 1,
                    source: 10,
                    target: 20,
                    relation_type: "supplements".into(),
                    weight: 0.8,
                }],
            },
            scope_tree: ScopeTreeSnapshot {
                nodes: vec![ScopeNode {
                    id: 1,
                    parent_id: None,
                    label: "root".into(),
                    depth: 0,
                }],
            },
            hnsw: None,
        };

        write_to_file(&header, &payload, &path).unwrap();
        assert!(path.exists());

        let (h, p) = load_from_file(&path, 128).expect("round-trip should succeed");
        assert_eq!(h.fingerprint, header.fingerprint);
        assert_eq!(h.embed_dim, 128);
        assert_eq!(p.graph.edges.len(), 1);
        assert_eq!(p.graph.edges[0].edge_id, 1);
        assert_eq!(p.scope_tree.nodes.len(), 1);
        assert!(p.hnsw.is_none());
    }

    #[test]
    fn named_msgpack_handles_missing_hnsw_field() {
        // Simulate a payload serialized WITHOUT the hnsw field (non-ann build).
        // The named MessagePack + #[serde(default)] should handle this.
        #[derive(Serialize)]
        struct PayloadWithoutHnsw {
            graph: GraphSnapshot,
            scope_tree: ScopeTreeSnapshot,
            // no hnsw field
        }

        let minimal = PayloadWithoutHnsw {
            graph: GraphSnapshot { edges: vec![] },
            scope_tree: ScopeTreeSnapshot { nodes: vec![] },
        };

        let bytes = rmp_serde::to_vec_named(&minimal).unwrap();
        let full: SnapshotPayload = rmp_serde::from_slice(&bytes).unwrap();
        assert!(full.hnsw.is_none());
    }
}
