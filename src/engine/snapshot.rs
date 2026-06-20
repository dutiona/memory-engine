//! Sidecar snapshot for fast cold-start.
//!
//! Serializes `MemoryGraph`, `ScopeTree`, and (optionally) HNSW state into a
//! `.snapshot` file next to the database. On startup, if the snapshot validates
//! against the current DB fingerprint, the engine loads from it instead of
//! doing a full `SQLite` scan. Any failure falls back to full rebuild (current behavior).
//!
//! ## File format
//!
//! ```text
//! [header_len: u32 LE][header (MessagePack, named)][payload (MessagePack, named)][blake3: 32 bytes]
//! ```
//!
//! The blake3 checksum covers the payload bytes only. Header is checked first
//! (cheap) so `format_version/embed_dim` mismatches reject without hashing.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, Result};
use crate::types::ScopeNode;

/// Current snapshot format version. Bump on breaking changes to the snapshot
/// layout or type definitions.
pub const FORMAT_VERSION: u32 = 1;

/// Size of the blake3 checksum appended to the file.
const BLAKE3_LEN: usize = 32;

/// Hard upper bound on the on-disk size of a `.snapshot` sidecar (512 MiB).
///
/// Bounds the wholesale `fs::read` of an oversized or tampered sidecar
/// (CWE-400, resource exhaustion via large on-disk file). `load_from_file`
/// slurps the entire file into a `Vec<u8>` before parsing; rejecting on
/// `fs::metadata` first caps that read at this many bytes regardless of what
/// the file claims to contain.
///
/// This does **not** defend against in-band `MessagePack` length prefixes:
/// `rmp_serde` reports the declared `SeqAccess::size_hint`, but serde's `Vec`
/// deserializer routes every preallocation through `size_hint::cautious`, which
/// caps `with_capacity` at 1 MiB worth of elements (`MAX_PREALLOC_BYTES`) and
/// then grows the vector incrementally as real elements are decoded. A small
/// crafted file therefore cannot force a multi-gigabyte allocation — it simply
/// errors out of input long before that — so no per-entry-length `DoS` exists for
/// this guard to mitigate. The complementary integrity defense is the per-entry
/// embedding-dimension check in `load_from_file` (runs *after* deserialization).
///
/// The blake3 checksum is *unkeyed* — a corruption check, not an authenticity
/// control — so an actor able to write the sidecar (local tamper /
/// shared-data-dir threat) can still produce a checksum-passing file; the size
/// cap and the dim check are what bound the blast radius of such a file. Tune to
/// the expected corpus; a real snapshot is dominated by HNSW embeddings.
const MAX_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;

/// Outcome of the pre-read size gate in [`load_from_file`].
///
/// Separating this from the read makes the short-circuit observable in tests
/// (a `cap` parameter lets a test prove the over-cap branch fires *before* any
/// `fs::read`, without materializing a half-gigabyte file).
#[derive(Debug, PartialEq, Eq)]
enum SizeGate {
    /// File is within the cap (or its size is unknown but acceptable); proceed
    /// to read it.
    Proceed,
    /// File exceeds the cap, or its metadata could not be read; abort before
    /// reading and fall back to a full rebuild.
    Reject,
}

/// Decide, from filesystem metadata alone, whether a sidecar is small enough to
/// read into memory. Rejects over-cap files *before* `fs::read` so a crafted or
/// tampered large file cannot drive a wholesale slurp into a `Vec<u8>`.
///
/// `cap` is a parameter (not the hardcoded const) purely so tests can exercise
/// the gate against a small real file with a small cap — deleting the size check
/// here is then an observable test failure, not just a slower path.
fn size_gate(path: &Path, cap: u64) -> SizeGate {
    match fs::metadata(path) {
        Ok(meta) if meta.len() > cap => {
            tracing::warn!(
                path = %path.display(),
                size = meta.len(),
                cap,
                "snapshot exceeds size cap, discarding"
            );
            SizeGate::Reject
        }
        Ok(_) => SizeGate::Proceed,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), error = %e, "snapshot metadata failed");
            }
            SizeGate::Reject
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot types (decoupled from internal representations)
// ---------------------------------------------------------------------------

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
    pub relation_type: String,
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

// ---------------------------------------------------------------------------
// Path derivation
// ---------------------------------------------------------------------------

/// Derive the snapshot sidecar path from the database path.
pub fn snapshot_path(db_path: &Path) -> PathBuf {
    let mut p = db_path.as_os_str().to_owned();
    p.push(".snapshot");
    PathBuf::from(p)
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

/// Read a composite fingerprint from the three source-of-truth tables.
///
/// Uses a single query with subselects for atomicity within the `SQLite`
/// read transaction.
pub fn read_fingerprint(conn: &Connection) -> Result<DbFingerprint> {
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
///    `MessagePack` for forward/backward compatibility).
/// 2. Compute blake3 hash over the payload bytes.
/// 3. Write to a temp file in the same directory (`NamedTempFile` uses
///    `O_CREAT | O_EXCL` — no symlink follow).
/// 4. `fsync` the file.
/// 5. Atomic rename via `persist()`.
/// 6. `fsync` the parent directory for rename durability.
pub fn write_to_file(
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
/// mismatch, checksum failure, `embed_dim` mismatch). Never errors — all
/// failures are logged and treated as "snapshot unavailable".
pub fn load_from_file(path: &Path, embed_dim: usize) -> Option<(SnapshotHeader, SnapshotPayload)> {
    // Size guard BEFORE reading the file into memory: an oversized or tampered
    // sidecar must not be slurped wholesale into a Vec<u8> (see
    // MAX_SNAPSHOT_BYTES). This bounds the on-disk read only; in-band length
    // prefixes are already bounded by serde's cautious preallocation cap. blake3
    // is unkeyed and so is not an authenticity gate. On metadata failure (other
    // than NotFound) we discard and fall back to a full rebuild.
    if size_gate(path, MAX_SNAPSHOT_BYTES) == SizeGate::Reject {
        return None;
    }

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

    // 4. Post-validate per-entry embedding dimensions. The header `embed_dim`
    //    check above only guards the declared dimension, not the actual vectors
    //    inside each `HnswEntry`. A corrupt/tampered payload could carry a
    //    wrong-length embedding that the header still claims is `embed_dim`;
    //    feeding that into the HNSW rebuild would be a latent dimension bug.
    if let Some(bad) = payload
        .hnsw
        .as_ref()
        .and_then(|hnsw| hnsw.entries.iter().find(|e| e.embedding.len() != embed_dim))
    {
        tracing::warn!(
            path = %path.display(),
            fact_id = bad.fact_id,
            entry_dim = bad.embedding.len(),
            expected = embed_dim,
            "snapshot HNSW entry embedding dimension mismatch, discarding"
        );
        return None;
    }

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
        let header_len = u32::try_from(header_bytes.len()).unwrap().to_le_bytes();

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
        let header_len = u32::try_from(header_bytes.len()).unwrap().to_le_bytes();

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
        let header_len = u32::try_from(header_bytes.len()).unwrap().to_le_bytes();

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
    fn size_gate_rejects_over_cap_before_read() {
        // Security (#411): the size gate must REJECT a file larger than the cap
        // *before* `fs::read`, so a crafted/corrupt file cannot force a
        // wholesale slurp into memory. This is the load-bearing short-circuit —
        // and it is what makes deleting the size check an observable failure
        // (the outcome `None` alone is not, since content checks also reject
        // garbage; see `oversized_sparse_file_still_returns_none`).
        //
        // We use a SMALL real file with a SMALL cap to exercise the exact
        // over-cap branch with zero large allocations: write a few KiB, set the
        // cap below it, and assert Reject. A complementary small cap above the
        // file size yields Proceed, pinning the boundary on a real path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.db.snapshot");
        fs::write(&path, vec![0u8; 4096]).unwrap();

        // File (4096 B) over a 1024 B cap → the gate rejects before any read.
        assert_eq!(size_gate(&path, 1024), SizeGate::Reject);
        // File (4096 B) under an 8192 B cap → the gate lets the read proceed.
        assert_eq!(size_gate(&path, 8192), SizeGate::Proceed);
        // Missing file → reject (NotFound is silent; still a reject).
        assert_eq!(
            size_gate(&dir.path().join("absent.snapshot"), 8192),
            SizeGate::Reject
        );
    }

    #[test]
    fn oversized_sparse_file_still_returns_none() {
        // Companion to `size_gate_rejects_over_cap_before_read`: the public
        // entry point returns None for a file whose metadata exceeds the real
        // cap. A sparse file (set_len) reports the logical size without
        // consuming disk blocks, so we exercise the actual MAX_SNAPSHOT_BYTES
        // without writing 512 MiB. NOTE: this pins only the OUTCOME (None) — the
        // short-circuit-before-read property is pinned by the `size_gate` unit
        // test above, because an all-zero file is also rejected on the content
        // path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.db.snapshot");
        let f = fs::File::create(&path).unwrap();
        f.set_len(MAX_SNAPSHOT_BYTES + 1).unwrap();
        drop(f);

        assert!(load_from_file(&path, 128).is_none());
    }

    #[test]
    fn size_cap_boundary_is_exclusive() {
        // The cap boundary is `>` not `>=`: a file exactly at the cap passes the
        // size gate so a legitimately large corpus at the limit still loads,
        // while one byte over is rejected. Pinned on a real path via `size_gate`
        // with a SMALL cap — a small file straddles a small cap with zero large
        // allocations, instead of materializing a half-gigabyte file (a
        // `set_len` + `load_from_file` would `fs::read` 512 MiB of zeros for zero
        // extra coverage, risking OOM under cargo's parallel test harness).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boundary.db.snapshot");
        let cap: usize = 4096;
        fs::write(&path, vec![0u8; cap]).unwrap();
        // Exactly at the cap → proceed (boundary is exclusive).
        assert_eq!(size_gate(&path, cap as u64), SizeGate::Proceed);

        // One byte over the cap → reject.
        fs::write(&path, vec![0u8; cap + 1]).unwrap();
        assert_eq!(size_gate(&path, cap as u64), SizeGate::Reject);
    }

    #[test]
    fn mismatched_entry_embedding_dim_returns_none() {
        // Security/integrity (#411): the header `embed_dim` check does not
        // validate per-entry embedding lengths. A payload whose HnswEntry
        // carries a wrong-dimension vector (corruption or tamper) must be
        // rejected rather than fed into the index rebuild.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_dim.db.snapshot");

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
            embed_dim: 4,
            engine_version: "test".into(),
        };
        let payload = SnapshotPayload {
            graph: GraphSnapshot { edges: vec![] },
            scope_tree: ScopeTreeSnapshot { nodes: vec![] },
            hnsw: Some(HnswSnapshot {
                entries: vec![HnswEntry {
                    fact_id: 1,
                    // 3 components, but embed_dim is 4 → mismatch.
                    embedding: vec![0.1, 0.2, 0.3],
                }],
            }),
        };

        write_to_file(&header, &payload, &path).unwrap();
        assert!(load_from_file(&path, 4).is_none());
    }

    #[test]
    fn matching_entry_embedding_dim_loads() {
        // Counterpart to the mismatch test: a payload whose entry embedding
        // matches `embed_dim` loads successfully (the validation does not
        // reject well-formed data).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("good_dim.db.snapshot");

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
            embed_dim: 4,
            engine_version: "test".into(),
        };
        let payload = SnapshotPayload {
            graph: GraphSnapshot { edges: vec![] },
            scope_tree: ScopeTreeSnapshot { nodes: vec![] },
            hnsw: Some(HnswSnapshot {
                entries: vec![HnswEntry {
                    fact_id: 1,
                    embedding: vec![0.1, 0.2, 0.3, 0.4],
                }],
            }),
        };

        write_to_file(&header, &payload, &path).unwrap();
        let (_, p) = load_from_file(&path, 4).expect("matching dim should load");
        assert_eq!(p.hnsw.unwrap().entries.len(), 1);
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

    // -----------------------------------------------------------------------
    // Property-based coverage (#451)
    //
    // The example tests above exercise the happy path and each individual
    // reject branch. These proptests stress the `write_to_file` →
    // `load_from_file` pipeline (two MessagePack serializations + a u32 LE
    // header length + a blake3 integrity check) over arbitrary field values —
    // including i64 boundaries (MIN/MAX) and random collection counts — and
    // assert the integrity-protected region rejects any single-byte mutation.
    // -----------------------------------------------------------------------
    mod proptest_roundtrip {
        // Bit-exact equality is the whole point of a serde roundtrip test: a
        // faithfully written then loaded `f32`/`f64` must be identical. We
        // exclude NaN in the strategies, so `==` is well-defined here.
        #![allow(clippy::float_cmp)]

        use super::*;
        use proptest::prelude::*;

        /// Embedding dimensions kept small to keep generated payloads cheap;
        /// the dimension is the only field both serialized into the header and
        /// cross-checked against each `HnswEntry`, so it is the interesting one.
        const DIM_RANGE: std::ops::RangeInclusive<usize> = 1..=8;

        prop_compose! {
            fn arb_fingerprint()(
                max_fact_id in any::<i64>(),
                active_fact_count in any::<i64>(),
                max_edge_id in any::<i64>(),
                active_edge_count in any::<i64>(),
                max_scope_id in any::<i64>(),
                scope_count in any::<i64>(),
            ) -> DbFingerprint {
                DbFingerprint {
                    max_fact_id,
                    active_fact_count,
                    max_edge_id,
                    active_edge_count,
                    max_scope_id,
                    scope_count,
                }
            }
        }

        prop_compose! {
            fn arb_edge()(
                edge_id in any::<i64>(),
                source in any::<i64>(),
                target in any::<i64>(),
                relation_type in ".*",
                weight in proptest::num::f64::NORMAL | proptest::num::f64::ZERO,
            ) -> GraphEdgeSnapshot {
                GraphEdgeSnapshot { edge_id, source, target, relation_type, weight }
            }
        }

        prop_compose! {
            fn arb_node()(
                id in any::<i64>(),
                parent_id in proptest::option::of(any::<i64>()),
                label in ".*",
                depth in any::<i64>(),
            ) -> ScopeNode {
                ScopeNode { id, parent_id, label, depth }
            }
        }

        prop_compose! {
            fn arb_entry(dim: usize)(
                fact_id in any::<i64>(),
                embedding in prop::collection::vec(
                    proptest::num::f32::NORMAL | proptest::num::f32::ZERO,
                    dim..=dim,
                ),
            ) -> HnswEntry {
                HnswEntry { fact_id, embedding }
            }
        }

        prop_compose! {
            /// Generate a self-consistent `(header, payload)` pair: the header's
            /// `embed_dim` and every HNSW entry's embedding length agree, so a
            /// faithful roundtrip is expected to succeed.
            fn arb_snapshot()(
                dim in DIM_RANGE,
            )(
                dim in Just(dim),
                fingerprint in arb_fingerprint(),
                engine_version in ".*",
                edges in prop::collection::vec(arb_edge(), 0..6),
                nodes in prop::collection::vec(arb_node(), 0..6),
                hnsw in proptest::option::of(
                    prop::collection::vec(arb_entry(dim), 0..6),
                ),
            ) -> (SnapshotHeader, SnapshotPayload, usize) {
                let header = SnapshotHeader {
                    format_version: FORMAT_VERSION,
                    fingerprint,
                    embed_dim: dim,
                    engine_version,
                };
                let payload = SnapshotPayload {
                    graph: GraphSnapshot { edges },
                    scope_tree: ScopeTreeSnapshot { nodes },
                    hnsw: hnsw.map(|entries| HnswSnapshot { entries }),
                };
                (header, payload, dim)
            }
        }

        proptest! {
            /// A faithfully written snapshot roundtrips to identical data for
            /// arbitrary field values (including i64::MIN/MAX and random counts).
            #[test]
            fn write_load_roundtrip((header, payload, dim) in arb_snapshot()) {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("rt.db.snapshot");

                write_to_file(&header, &payload, &path).unwrap();
                let (h, p) = load_from_file(&path, dim)
                    .expect("faithful roundtrip must load");

                prop_assert_eq!(h.format_version, header.format_version);
                prop_assert_eq!(h.embed_dim, header.embed_dim);
                prop_assert_eq!(h.fingerprint, header.fingerprint);
                prop_assert_eq!(&h.engine_version, &header.engine_version);

                prop_assert_eq!(p.graph.edges.len(), payload.graph.edges.len());
                for (a, b) in p.graph.edges.iter().zip(payload.graph.edges.iter()) {
                    prop_assert_eq!(a.edge_id, b.edge_id);
                    prop_assert_eq!(a.source, b.source);
                    prop_assert_eq!(a.target, b.target);
                    prop_assert_eq!(&a.relation_type, &b.relation_type);
                    prop_assert_eq!(a.weight, b.weight);
                }

                prop_assert_eq!(p.scope_tree.nodes, payload.scope_tree.nodes);

                match (&p.hnsw, &payload.hnsw) {
                    (Some(got), Some(want)) => {
                        prop_assert_eq!(got.entries.len(), want.entries.len());
                        for (a, b) in got.entries.iter().zip(want.entries.iter()) {
                            prop_assert_eq!(a.fact_id, b.fact_id);
                            prop_assert_eq!(&a.embedding, &b.embedding);
                        }
                    }
                    (None, None) => {}
                    _ => prop_assert!(false, "hnsw presence mismatch"),
                }
            }

            /// Any single-byte mutation inside the blake3-covered region
            /// (payload bytes + the trailing 32-byte checksum) must be rejected.
            /// The header region is intentionally NOT integrity-protected
            /// (the format hashes the payload only), so it is excluded here.
            #[test]
            fn bit_flip_in_protected_region_rejected(
                (header, payload, dim) in arb_snapshot(),
                flip_bit in 0u8..8,
                pos in any::<usize>(),
            ) {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("flip.db.snapshot");
                write_to_file(&header, &payload, &path).unwrap();

                let mut bytes = fs::read(&path).unwrap();
                let header_len =
                    u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
                let protected_start = 4 + header_len; // first payload byte
                let protected_len = bytes.len() - protected_start;
                prop_assume!(protected_len > 0);

                // Map `pos` into the protected region [protected_start, len).
                let offset = protected_start + (pos % protected_len);
                bytes[offset] ^= 1u8 << flip_bit;
                fs::write(&path, &bytes).unwrap();

                prop_assert!(
                    load_from_file(&path, dim).is_none(),
                    "single-byte mutation in the payload/checksum region must reject"
                );
            }
        }
    }
}
