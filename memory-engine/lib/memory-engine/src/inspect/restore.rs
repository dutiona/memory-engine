//! Restore engine state from a snapshot (JSON or `SQLite` backup).
//!
//! Complements the export side in [`super::dump`]. Import always targets a fresh
//! (empty) database — no merge/additive semantics.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use rusqlite::Connection;

use crate::error::{ConflictError, MemoryError, MigrationError, Result};
use crate::store::events::event_type_to_str;
use crate::store::lineage::LineageStore;
use crate::store::schema::{CURRENT_SCHEMA_VERSION, STORAGE_EPOCH, set_config};
use crate::store::serialize_embedding;

use super::types::EngineSnapshot;

/// Config keys managed by `init_schema`/`migrate` — never imported from snapshots.
///
/// `embedding_meta` is the pre-#622 legacy identity key. It no longer exists as a live
/// config row (the identity moved to the `embedding_spaces` table), so it must not be
/// copied back into `config`; an old snapshot's value is handled explicitly when restoring
/// the registry (see the embedding-space restore step).
const MANAGED_CONFIG_KEYS: &[&str] = &["schema_version", "storage_epoch", "embedding_meta"];

// ---------------------------------------------------------------------------
// Compression detection
// ---------------------------------------------------------------------------

/// Detected compression format of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    None,
    Gzip,
    Zstd,
}

/// Detect compression from magic bytes at the start of a file.
///
/// Reads the first 4 bytes from the given file handle and seeks back to the
/// start so the caller can continue reading from the beginning.
fn detect_compression(file: &mut File) -> Result<Compression> {
    use std::io::Seek;
    let mut magic = [0u8; 4];
    let n = file.read(&mut magic)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    if n >= 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        Ok(Compression::Gzip)
    } else if n >= 4 && magic[..4] == [0x28, 0xb5, 0x2f, 0xfd] {
        Ok(Compression::Zstd)
    } else {
        Ok(Compression::None)
    }
}

// ---------------------------------------------------------------------------
// Snapshot reading
// ---------------------------------------------------------------------------

/// Maximum snapshot size (4 GiB). Caps both the on-disk (compressed) file and
/// the **decompressed** stream, so a small gzip/zstd file that inflates past the
/// cap is rejected as a decompression bomb (CWE-409, #141).
const MAX_SNAPSHOT_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Deserialize an [`EngineSnapshot`] from a (possibly compressed) JSON file.
///
/// Auto-detects compression from magic bytes. Returns a clear error if the
/// file uses a compression format whose feature is not enabled.
///
/// Both the on-disk (compressed) size and the **decompressed** stream are capped
/// at [`MAX_SNAPSHOT_SIZE`]: the file-size check alone is insufficient because a
/// tiny gzip/zstd file can inflate far past the cap (a decompression bomb,
/// CWE-409). The decompressed cap is enforced by wrapping the decoder in
/// [`std::io::Read::take`], and tripping it surfaces a *distinct*
/// [`MemoryError::SnapshotTooLarge`] rather than the opaque truncated-input
/// [`MemoryError::Serialization`] a bare `take` would otherwise produce (#141).
///
/// # Errors
///
/// - [`MemoryError::Io`] on file access failure.
/// - [`MemoryError::Serialization`] on malformed JSON.
/// - [`MemoryError::NotImplemented`] if compression detected but feature disabled.
/// - [`MemoryError::SnapshotTooLarge`] if the compressed file or the decompressed
///   stream exceeds 4 GiB (a possible decompression bomb).
/// - [`MemoryError::Internal`] if zstd decoder initialization fails.
pub fn read_snapshot(path: &Path) -> Result<EngineSnapshot> {
    read_snapshot_capped(path, MAX_SNAPSHOT_SIZE)
}

/// Cap-parameterized core of [`read_snapshot`].
///
/// Splitting the byte cap out as a parameter lets the cap-firing path be tested
/// with a tiny limit instead of a real 4 GiB payload (mirrors `read_pak_capped`
/// in `archive::pak`). [`read_snapshot`] is the only non-test caller and always
/// passes [`MAX_SNAPSHOT_SIZE`].
///
/// The `cap` bounds the **decompressed** stream. The compressed file is also
/// rejected up front when its on-disk size already exceeds `cap` (no point
/// decompressing a too-large file), which keeps the plain (uncompressed) path
/// equivalent — for `Compression::None` the on-disk size *is* the decompressed
/// size.
fn read_snapshot_capped(path: &Path, cap: u64) -> Result<EngineSnapshot> {
    // Open the file once and perform all checks on the handle to avoid TOCTOU.
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > cap {
        return Err(MemoryError::SnapshotTooLarge { cap });
    }

    let compression = detect_compression(&mut file)?;
    let reader = BufReader::new(file);

    match compression {
        Compression::None => read_capped(reader, cap),
        Compression::Gzip => read_gzip(reader, cap),
        Compression::Zstd => read_zstd(reader, cap),
    }
}

/// Deserialize an [`EngineSnapshot`] from a reader, capping the consumed bytes at
/// `cap` and surfacing a decompression-bomb overflow as a distinct
/// [`MemoryError::SnapshotTooLarge`].
///
/// Reads up to `cap + 1` bytes. The cap is *inclusive* — a stream whose size is
/// exactly `cap` bytes is valid and must read. With a bare `take(reader, cap)`
/// such a stream consumes all `cap` bytes, leaving `limit() == 0`, and the
/// post-parse check would falsely reject it (off-by-one). The one-byte slack lets
/// an exactly-`cap` payload through (it leaves `limit() == 1`) while still
/// bounding memory, so the `limit() == 0` check trips *only* when MORE than `cap`
/// bytes were consumed — a genuine overflow. Same shape as `read_pak_capped`.
fn read_capped(reader: impl Read, cap: u64) -> Result<EngineSnapshot> {
    let mut limited = std::io::Read::take(reader, cap.saturating_add(1));
    let parsed: serde_json::Result<EngineSnapshot> = serde_json::from_reader(&mut limited);

    // Check the cap *before* propagating any serde error. When the decompressed
    // stream exceeds the cap, `Take` returns EOF and `serde_json` fails with a
    // truncated-input error indistinguishable from genuine corruption — exactly
    // the deficiency this guards (#141, CWE-409). By inspecting the cap first we
    // surface the bomb as a distinct `SnapshotTooLarge` regardless of whether
    // serde parsed a complete prefix or choked on the truncation.
    if limited.limit() == 0 {
        return Err(MemoryError::SnapshotTooLarge { cap });
    }
    Ok(parsed?)
}

#[cfg(feature = "compress-gzip")]
fn read_gzip(reader: impl Read, cap: u64) -> Result<EngineSnapshot> {
    let decoder = flate2::read::GzDecoder::new(reader);
    read_capped(decoder, cap)
}

#[cfg(not(feature = "compress-gzip"))]
fn read_gzip(_reader: impl Read, _cap: u64) -> Result<EngineSnapshot> {
    Err(MemoryError::NotImplemented(
        "gzip-compressed snapshot detected but `compress-gzip` feature is not enabled".into(),
    ))
}

#[cfg(feature = "compress-zstd")]
fn read_zstd(reader: impl Read, cap: u64) -> Result<EngineSnapshot> {
    let decoder =
        zstd::Decoder::new(reader).map_err(|e| MemoryError::Internal(format!("zstd init: {e}")))?;
    read_capped(decoder, cap)
}

#[cfg(not(feature = "compress-zstd"))]
fn read_zstd(_reader: impl Read, _cap: u64) -> Result<EngineSnapshot> {
    Err(MemoryError::NotImplemented(
        "zstd-compressed snapshot detected but `compress-zstd` feature is not enabled".into(),
    ))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a snapshot against the current engine version.
///
/// Checks schema compatibility, storage epoch, embedding dimension, the
/// per-embedding length invariant, and the root scope invariant required by
/// [`crate::scope::ScopeTree`].
///
/// # Errors
///
/// - [`MemoryError::Migration`] if `schema_version` is from a newer schema.
/// - [`MemoryError::UnsupportedEpoch`] if `storage_epoch` doesn't match.
/// - [`MemoryError::Internal`] if `embed_dim` is zero or root scope is missing.
/// - [`MemoryError::EmbeddingDimension`] if any fact, summary, or `fact_vectors`
///   embedding length does not equal the snapshot's `embed_dim` (#413).
pub fn validate_snapshot(snapshot: &EngineSnapshot) -> Result<()> {
    if snapshot.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(MigrationError::Incompatible(format!(
            "snapshot schema_version {} is newer than supported {}",
            snapshot.schema_version, CURRENT_SCHEMA_VERSION
        ))
        .into());
    }
    if snapshot.storage_epoch > STORAGE_EPOCH {
        return Err(MemoryError::UnsupportedEpoch {
            db_epoch: snapshot.storage_epoch,
            supported_epoch: STORAGE_EPOCH,
        });
    }
    if snapshot.embed_dim == 0 {
        return Err(MemoryError::Internal("snapshot embed_dim is 0".into()));
    }

    // Per-embedding length invariant (#413): every persisted embedding is read
    // back via `deserialize_embedding`, which hard-errors with `EmbeddingDimension`
    // when a blob's length is not `dim * 4` bytes. A restore that trusts the
    // snapshot's lengths blindly would persist corrupt-dimension blobs that then
    // fail every subsequent read. Reject them here, before the write transaction,
    // with the same typed error the read path uses — a crafted/corrupt snapshot is
    // refused atomically rather than partially imported. The sidecar `.snapshot`
    // path already does the equivalent per-`HnswEntry` check (#411); the JSON
    // dump/restore path was missing it.
    //
    // The **active** vectors (`facts[].embedding` and `summaries[].embedding`)
    // belong to the active space, so they must match the snapshot's `embed_dim`.
    let expected = snapshot.embed_dim;
    for fact in &snapshot.facts {
        if fact.embedding.len() != expected {
            return Err(MemoryError::EmbeddingDimension {
                expected,
                actual: fact.embedding.len(),
            });
        }
    }
    for summary in &snapshot.summaries {
        if summary.embedding.len() != expected {
            return Err(MemoryError::EmbeddingDimension {
                expected,
                actual: summary.embedding.len(),
            });
        }
    }

    // The non-active `fact_vectors` rows belong to *other* embedding spaces
    // (a `populating` space mid-reconstruction, or a `deprecated` one kept for
    // rollback). A different-dimension reconstruction (#742) legitimately stores
    // vectors at a dimension other than the active `embed_dim`, so each row must be
    // checked against **its owning space's** dimension — taken from the matching
    // `embedding_spaces` fingerprint — not the active dim. A row referencing an
    // unknown space is rejected early (it would otherwise fail the `fact_vectors`
    // FK at insert with an opaque database error).
    for fv in &snapshot.fact_vectors {
        let space_dim = snapshot
            .embedding_spaces
            .iter()
            .find(|s| s.name == fv.space_id)
            .map(|s| s.fingerprint.dim)
            .ok_or_else(|| {
                MemoryError::Internal(format!(
                    "snapshot fact_vectors row references unknown embedding space '{}'",
                    fv.space_id
                ))
            })?;
        if fv.embedding.len() != space_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: space_dim,
                actual: fv.embedding.len(),
            });
        }
    }

    // Root scope invariant: ScopeTree hardcodes root id=1, parent_id=None.
    let has_root = snapshot
        .scopes
        .iter()
        .any(|s| s.id == 1 && s.parent_id.is_none() && s.label == "root" && s.depth == 0);
    if !has_root && !snapshot.scopes.is_empty() {
        return Err(MemoryError::Internal(
            "snapshot scopes missing required root node (id=1, parent_id=None, label='root', depth=0)"
                .into(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Restore into connection
// ---------------------------------------------------------------------------

/// Assert that all user tables in the connection are empty (fresh DB).
///
/// The root scope (id=1) inserted by `init_schema` is expected and excluded.
fn assert_empty_db(conn: &Connection) -> Result<()> {
    let event_count: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
    let fact_count: i64 = conn.query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))?;
    let edge_count: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let summary_count: i64 = conn.query_row("SELECT COUNT(*) FROM summaries", [], |r| r.get(0))?;
    // scopes: root scope (id=1) is expected from init_schema
    let scope_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM scopes WHERE id > 1", [], |r| r.get(0))?;
    let lineage_count: i64 = conn.query_row("SELECT COUNT(*) FROM lineage", [], |r| r.get(0))?;
    let fact_vector_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM fact_vectors", [], |r| r.get(0))?;

    let total = event_count
        + fact_count
        + edge_count
        + summary_count
        + scope_count
        + lineage_count
        + fact_vector_count;
    if total > 0 {
        return Err(MemoryError::Conflict(ConflictError::TargetNotEmpty));
    }
    Ok(())
}

/// Insert all snapshot events, preserving explicit IDs. Helper for
/// [`restore_snapshot_into`].
fn restore_events(conn: &Connection, snapshot: &EngineSnapshot) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO events (id, timestamp, event_type, payload, source, session_id, \
         scope_id, origin_node_id, sequence_id, created_at, event_revision) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for event in &snapshot.events {
        let ts = event.timestamp.to_rfc3339();
        let et = event_type_to_str(&event.event_type);
        let payload = event.payload.to_string();
        let created = event.created_at.map(|dt| dt.to_rfc3339());
        stmt.execute(rusqlite::params![
            event.id,
            ts,
            et,
            payload,
            event.source,
            event.session_id,
            event.scope_id,
            event.origin_node_id,
            event.sequence_id,
            created,
            event.event_revision,
        ])?;
    }
    Ok(())
}

/// Insert all snapshot facts, preserving explicit IDs. Helper for
/// [`restore_snapshot_into`].
fn restore_facts(conn: &Connection, snapshot: &EngineSnapshot) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO facts (id, content, content_hash, embedding, fact_type, t_created, \
         t_expired, t_valid, t_invalid, source_event_id, importance, access_count, \
         last_accessed, metadata, scope_id, is_pinned, importance_score, surfaced_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
    )?;
    for fact in &snapshot.facts {
        let embedding_blob = serialize_embedding(&fact.embedding);
        let ft = crate::store::facts::fact_type_to_str(&fact.fact_type);
        let t_created = fact.t_created.to_rfc3339();
        let t_expired = fact.t_expired.map(|dt| dt.to_rfc3339());
        let t_valid = fact.t_valid.map(|dt| dt.to_rfc3339());
        let t_invalid = fact.t_invalid.map(|dt| dt.to_rfc3339());
        let last_accessed = fact.last_accessed.to_rfc3339();
        let metadata = fact.metadata.to_string();
        stmt.execute(rusqlite::params![
            fact.id,
            fact.content,
            fact.content_hash,
            embedding_blob,
            ft,
            t_created,
            t_expired,
            t_valid,
            t_invalid,
            fact.source_event_id,
            fact.base_importance, // -> DB column `importance`
            fact.access_count,
            last_accessed,
            metadata,
            fact.scope_id,
            fact.is_pinned,
            fact.importance_score,
            fact.surfaced_at.map(|dt| dt.to_rfc3339()),
        ])?;
    }
    Ok(())
}

/// Insert all snapshot edges, preserving explicit IDs. Helper for
/// [`restore_snapshot_into`].
fn restore_edges(conn: &Connection, snapshot: &EngineSnapshot) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO edges (id, source_fact_id, target_fact_id, relation_type, weight, \
         t_created, t_expired, scope_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for edge in &snapshot.edges {
        let t_created = edge.t_created.to_rfc3339();
        let t_expired = edge.t_expired.map(|dt| dt.to_rfc3339());
        stmt.execute(rusqlite::params![
            edge.id,
            edge.source_fact_id,
            edge.target_fact_id,
            edge.relation_type,
            edge.weight,
            t_created,
            t_expired,
            edge.scope_id,
        ])?;
    }
    Ok(())
}

/// Restore the embedding-space registry (#622) from a snapshot.
///
/// A current snapshot carries the `embedding_spaces` rows explicitly. A pre-#622 snapshot
/// has none, so the identity is reconstructed from the legacy `embedding_meta` config value
/// if present (older exports kept it as a config row).
///
/// # Errors
///
/// Returns [`MemoryError::Migration`] on a corrupt legacy `embedding_meta` value or an
/// unrecognized space status, and [`MemoryError::Database`]/[`MemoryError::Internal`] on
/// insert failure (e.g. a second active space).
fn restore_embedding_spaces(conn: &Connection, snapshot: &EngineSnapshot) -> Result<()> {
    if snapshot.embedding_spaces.is_empty() {
        if let Some(raw) = snapshot.config.get("embedding_meta") {
            let fp: crate::types::EmbeddingFingerprint =
                serde_json::from_str(raw).map_err(|e| {
                    MigrationError::Incompatible(format!(
                        "corrupt legacy embedding_meta in snapshot: {e}"
                    ))
                })?;
            crate::store::embedding_meta::store(conn, &fp)?;
        }
        return Ok(());
    }
    for s in &snapshot.embedding_spaces {
        let status = crate::store::embedding_spaces::SpaceStatus::from_sql(&s.status)?;
        crate::store::embedding_spaces::insert_active(
            conn,
            &crate::store::embedding_spaces::EmbeddingSpace {
                name: s.name.clone(),
                fingerprint: s.fingerprint.clone(),
                status,
            },
        )?;
    }
    Ok(())
}

/// Restore the `fact_vectors` rows (#623): the non-active spaces' per-fact vectors
/// (a `populating` space mid-reconstruction, or a `deprecated` space retained for
/// rollback). Pre-#623 snapshots have none — a no-op. MUST run **after**
/// [`restore_facts`] and [`restore_embedding_spaces`] (the `fact_id` and `space_id`
/// foreign keys). The active vectors are not here — they ride `facts.embedding`.
fn restore_fact_vectors(conn: &Connection, snapshot: &EngineSnapshot) -> Result<()> {
    if snapshot.fact_vectors.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare("INSERT INTO fact_vectors (fact_id, space_id, embedding) VALUES (?1, ?2, ?3)")?;
    for fv in &snapshot.fact_vectors {
        let blob = serialize_embedding(&fv.embedding);
        stmt.execute(rusqlite::params![fv.fact_id, fv.space_id, blob])?;
    }
    Ok(())
}

/// Write all snapshot data into a connection within a single transaction.
///
/// Uses explicit IDs to preserve foreign key relationships. The connection
/// must already have schema initialized (via `init_schema` + `migrate`).
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on any SQL failure (transaction rolls back).
/// Returns [`MemoryError::Conflict`] if the database is not empty.
/// Propagates the validation errors from [`validate_snapshot`]:
/// [`MemoryError::Migration`] if the snapshot's `schema_version` is newer than
/// supported, [`MemoryError::UnsupportedEpoch`] on a future storage epoch, and
/// [`MemoryError::Internal`] if `embed_dim` is zero or the root scope is missing.
/// Returns [`MemoryError::Serialization`] if a summary's `source_fact_ids`
/// cannot be serialized to JSON.
pub fn restore_snapshot_into(conn: &Connection, snapshot: &EngineSnapshot) -> Result<()> {
    validate_snapshot(snapshot)?;
    assert_empty_db(conn)?;

    let tx = conn.unchecked_transaction()?;

    // 1. Replace scopes with snapshot's scopes (preserving original IDs).
    //    When the snapshot has scopes, delete the auto-inserted root and reinsert all.
    //    When the snapshot has no scopes (empty snapshot), keep the root from init_schema.
    if !snapshot.scopes.is_empty() {
        tx.execute("DELETE FROM scopes", [])?;

        // 2. Insert scopes (sorted by depth then id to satisfy parent FK).
        let mut scope_idx: Vec<usize> = (0..snapshot.scopes.len()).collect();
        scope_idx.sort_by_key(|&i| (snapshot.scopes[i].depth, snapshot.scopes[i].id));
        let mut stmt =
            tx.prepare("INSERT INTO scopes (id, parent_id, label, depth) VALUES (?1, ?2, ?3, ?4)")?;
        for scope in scope_idx.into_iter().map(|i| &snapshot.scopes[i]) {
            stmt.execute(rusqlite::params![
                scope.id,
                scope.parent_id,
                scope.label,
                scope.depth,
            ])?;
        }
    }

    // 3-5. Insert events, facts, and edges (FK order: events -> facts -> edges).
    restore_events(&tx, snapshot)?;
    restore_facts(&tx, snapshot)?;
    restore_edges(&tx, snapshot)?;

    // 6. Insert summaries.
    {
        let mut stmt = tx.prepare(
            "INSERT INTO summaries (id, content, embedding, level, source_fact_ids, \
             created_at, scope_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for summary in &snapshot.summaries {
            let embedding_blob = serialize_embedding(&summary.embedding);
            let level = crate::store::summaries::level_to_str(&summary.level);
            let source_ids = serde_json::to_string(&summary.source_fact_ids)?;
            let created = summary.created_at.to_rfc3339();
            stmt.execute(rusqlite::params![
                summary.id,
                summary.content,
                embedding_blob,
                level,
                source_ids,
                created,
                summary.scope_id,
            ])?;
        }
    }

    // 7. Insert lineage records (Phase 5a provenance).
    {
        let lineage_store = LineageStore::new(&tx);
        for entry in &snapshot.lineage {
            lineage_store.insert_raw(entry)?;
        }
    }

    // 8. Import config keys (except managed ones).
    for (key, value) in &snapshot.config {
        if !MANAGED_CONFIG_KEYS.contains(&key.as_str()) {
            set_config(&tx, key, value)?;
        }
    }

    // 8b. Restore the embedding-space registry (#622).
    restore_embedding_spaces(&tx, snapshot)?;

    // 8c. Restore the non-active spaces' vectors (#623) — after facts (8b: spaces;
    //     3-5: facts) so both foreign keys resolve.
    restore_fact_vectors(&tx, snapshot)?;

    // 9. Reset autoincrement sequences so new inserts don't collide.
    reset_autoincrement(
        &tx,
        "scopes",
        snapshot.scopes.iter().map(|s| s.id).max().unwrap_or(0),
    )?;
    reset_autoincrement(
        &tx,
        "events",
        snapshot.events.iter().map(|e| e.id).max().unwrap_or(0),
    )?;
    reset_autoincrement(
        &tx,
        "facts",
        snapshot.facts.iter().map(|f| f.id).max().unwrap_or(0),
    )?;
    reset_autoincrement(
        &tx,
        "edges",
        snapshot.edges.iter().map(|e| e.id).max().unwrap_or(0),
    )?;
    reset_autoincrement(
        &tx,
        "summaries",
        snapshot.summaries.iter().map(|s| s.id).max().unwrap_or(0),
    )?;
    reset_autoincrement(
        &tx,
        "lineage",
        snapshot
            .lineage
            .iter()
            .map(|l| l.lineage_id)
            .max()
            .unwrap_or(0),
    )?;

    // 10. Commit.
    tx.commit()?;
    Ok(())
}

/// Reset the `sqlite_sequence` entry for a table to the maximum imported ID.
///
/// `sqlite_sequence` is auto-created by `SQLite` for AUTOINCREMENT tables but
/// has no unique constraint on `name`, so we use DELETE + INSERT.
fn reset_autoincrement(conn: &Connection, table: &str, max_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM sqlite_sequence WHERE name = ?1",
        rusqlite::params![table],
    )?;
    conn.execute(
        "INSERT INTO sqlite_sequence (name, seq) VALUES (?1, ?2)",
        rusqlite::params![table, max_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::inspect::types::DumpFormat;
    use crate::store::schema::{get_config, init_schema, migrate, open_memory};
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType};
    use std::collections::{BTreeMap, HashMap};

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }

        fn fingerprint(&self) -> crate::types::EmbeddingFingerprint {
            crate::types::EmbeddingFingerprint::new("mock", "test", 4)
        }
    }

    // --- Compression detection tests ---

    #[test]
    fn detect_plain_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        std::fs::write(&path, r#"{"hello":"world"}"#).unwrap();
        let mut f = File::open(&path).unwrap();
        assert_eq!(detect_compression(&mut f).unwrap(), Compression::None);
    }

    #[cfg(feature = "compress-gzip")]
    #[test]
    fn detect_gzip_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.gz");
        // Write minimal gzip header
        std::fs::write(&path, [0x1f, 0x8b, 0x08, 0x00]).unwrap();
        let mut f = File::open(&path).unwrap();
        assert_eq!(detect_compression(&mut f).unwrap(), Compression::Gzip);
    }

    #[cfg(feature = "compress-zstd")]
    #[test]
    fn detect_zstd_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.zst");
        std::fs::write(&path, [0x28, 0xb5, 0x2f, 0xfd]).unwrap();
        let mut f = File::open(&path).unwrap();
        assert_eq!(detect_compression(&mut f).unwrap(), Compression::Zstd);
    }

    // --- Validation tests ---

    #[test]
    fn validate_rejects_future_schema() {
        let snapshot = EngineSnapshot {
            schema_version: 99,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 4,
            facts: vec![],
            edges: vec![],
            summaries: vec![],
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![],
            fact_vectors: vec![],
            config: BTreeMap::new(),
        };
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(err.to_string().contains("newer than supported"));
    }

    #[test]
    fn validate_rejects_future_epoch() {
        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: 99,
            embed_dim: 4,
            facts: vec![],
            edges: vec![],
            summaries: vec![],
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![],
            fact_vectors: vec![],
            config: BTreeMap::new(),
        };
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(matches!(err, MemoryError::UnsupportedEpoch { .. }));
    }

    #[test]
    fn validate_rejects_zero_embed_dim() {
        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 0,
            facts: vec![],
            edges: vec![],
            summaries: vec![],
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![],
            fact_vectors: vec![],
            config: BTreeMap::new(),
        };
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(err.to_string().contains("embed_dim is 0"));
    }

    #[test]
    fn validate_rejects_missing_root_scope() {
        use crate::types::ScopeNode;
        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 4,
            facts: vec![],
            edges: vec![],
            summaries: vec![],
            scopes: vec![ScopeNode {
                id: 2,
                parent_id: Some(1),
                label: "child".into(),
                depth: 1,
            }],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![],
            fact_vectors: vec![],
            config: BTreeMap::new(),
        };
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(err.to_string().contains("root node"));
    }

    #[test]
    fn validate_accepts_empty_snapshot() {
        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 4,
            facts: vec![],
            edges: vec![],
            summaries: vec![],
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![],
            fact_vectors: vec![],
            config: BTreeMap::new(),
        };
        validate_snapshot(&snapshot).unwrap();
    }

    // --- Round-trip restore tests ---

    #[tokio::test]
    async fn json_dump_restore_roundtrip() {
        // Create engine and add data.
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "alpha fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "beta fact".into(),
                    fact_type: FactType::Episodic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        // Dump to JSON.
        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
            .await
            .unwrap();

        // Read snapshot and restore into fresh in-memory DB.
        let snapshot = read_snapshot(&json_path).unwrap();
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();
        restore_snapshot_into(&conn, &snapshot).unwrap();

        // Verify facts.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Verify scopes restored (at least root).
        let root_label: String = conn
            .query_row("SELECT label FROM scopes WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(root_label, "root");

        // Verify the embedding identity (#622) round-trips through dump→restore via the
        // explicit `embedding_spaces` snapshot section + `restore_embedding_spaces` (the
        // identity now lives in the registry table, not a config row). The bare `embed_dim`
        // key no longer exists.
        assert!(get_config(&conn, "embed_dim").unwrap().is_none());
        let meta = crate::store::embedding_meta::load(&conn).unwrap();
        assert_eq!(
            meta.map(|fp| fp.dim),
            Some(4),
            "embedding identity dim survives restore"
        );
    }

    #[test]
    fn restore_translates_legacy_embedding_meta_config() {
        // A pre-#622 dump has no `embedding_spaces` section but carries the identity as a
        // legacy `embedding_meta` config row. Restore must reconstruct it into the registry
        // (the back-compat fallback in `restore_embedding_spaces`) — otherwise an old export
        // restores with no identity (silent retrieval corruption, #614's failure class).
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let fp = crate::types::EmbeddingFingerprint::with_matryoshka("legacy-model", "tei", 4, 8);
        let mut config = BTreeMap::new();
        config.insert(
            "embedding_meta".to_string(),
            serde_json::to_string(&fp).unwrap(),
        );
        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 4,
            facts: vec![],
            edges: vec![],
            summaries: vec![],
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![], // pre-#622 dump: identity is in `config`, not here
            fact_vectors: vec![],     // pre-#623 dump: no shadow vectors
            config,
        };

        restore_embedding_spaces(&conn, &snapshot).unwrap();

        assert_eq!(
            crate::store::embedding_meta::load(&conn).unwrap(),
            Some(fp),
            "legacy embedding_meta config value reconstructed into the registry"
        );
    }

    #[tokio::test]
    async fn restore_rejects_non_empty_db() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "existing".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        // Dump.
        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
            .await
            .unwrap();
        let snapshot = read_snapshot(&json_path).unwrap();

        // Create a non-empty target DB.
        let conn2 = open_memory().unwrap();
        init_schema(&conn2).unwrap();
        migrate(&conn2, None).unwrap();
        conn2
            .execute(
                "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed) \
                 VALUES ('x', 'h', X'00', 'semantic', datetime('now'), datetime('now'))",
                [],
            )
            .unwrap();

        let err = restore_snapshot_into(&conn2, &snapshot).unwrap_err();
        assert!(err.to_string().contains("not empty"));
    }

    #[tokio::test]
    async fn autoincrement_reset_after_restore() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact1".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact2".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                None,
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
            .await
            .unwrap();

        let snapshot = read_snapshot(&json_path).unwrap();
        let max_fact_id = snapshot.facts.iter().map(|f| f.id).max().unwrap();

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();
        restore_snapshot_into(&conn, &snapshot).unwrap();

        // Insert a new fact — its ID should be > max imported ID.
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type, t_created, last_accessed) \
             VALUES ('new', 'h2', X'00', 'semantic', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        let new_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        assert!(
            new_id > max_fact_id,
            "new id {new_id} should be > max imported {max_fact_id}"
        );
    }

    #[test]
    fn read_snapshot_rejects_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, "not valid json{{{").unwrap();
        let err = read_snapshot(&path).unwrap_err();
        assert!(matches!(err, MemoryError::Serialization(_)));
    }

    // --- Decompression-bomb cap tests (#141, CWE-409) ---

    /// A minimal but valid `EngineSnapshot` JSON whose decompressed size is
    /// padded by `pad` config bytes — large enough to overflow a tiny test cap
    /// while compressing to a handful of bytes (the decompression-bomb shape).
    fn padded_snapshot_json(pad: usize) -> String {
        let filler = "x".repeat(pad);
        format!(
            r#"{{"schema_version":{CURRENT_SCHEMA_VERSION},"storage_epoch":{STORAGE_EPOCH},
            "embed_dim":4,"facts":[],"edges":[],"summaries":[],"scopes":[],"events":[],
            "lineage":[],"embedding_spaces":[],"fact_vectors":[],"config":{{"pad":"{filler}"}}}}"#
        )
    }

    /// The on-disk (compressed) cap still rejects an uncompressed file that is
    /// itself larger than the cap — and now surfaces the distinct
    /// `SnapshotTooLarge`, not the old opaque `Internal`.
    #[test]
    fn read_snapshot_rejects_oversized_plain_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        std::fs::write(&path, padded_snapshot_json(4096)).unwrap();
        // Cap below the on-disk size: the up-front file-size guard fires.
        let err = read_snapshot_capped(&path, 64).unwrap_err();
        assert!(
            matches!(err, MemoryError::SnapshotTooLarge { cap: 64 }),
            "expected SnapshotTooLarge, got {err:?}"
        );
    }

    #[cfg(feature = "compress-gzip")]
    #[test]
    fn read_snapshot_caps_decompressed_gzip_bomb() {
        use flate2::Compression as GzLevel;
        use flate2::write::GzEncoder;
        use std::io::Write;

        // A payload that decompresses to >> the cap but compresses to a few bytes
        // (highly-repetitive filler → the classic decompression-bomb shape).
        let payload = padded_snapshot_json(100_000);
        let mut enc = GzEncoder::new(Vec::new(), GzLevel::best());
        enc.write_all(payload.as_bytes()).unwrap();
        let compressed = enc.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.json.gz");
        std::fs::write(&path, &compressed).unwrap();

        // The compressed file is tiny — it clears the on-disk file-size guard,
        // so the trip must come from the *decompressed* cap, not the file cap.
        let cap: u64 = 1024;
        let compressed_len = u64::try_from(compressed.len()).unwrap();
        assert!(
            compressed_len <= cap,
            "test precondition: compressed ({compressed_len}) must fit under the cap ({cap}) so \
             the decompressed cap is what trips"
        );
        let err = read_snapshot_capped(&path, cap).unwrap_err();
        assert!(
            matches!(err, MemoryError::SnapshotTooLarge { cap: c } if c == cap),
            "expected distinct SnapshotTooLarge (not an opaque serde EOF), got {err:?}"
        );
    }

    #[cfg(feature = "compress-zstd")]
    #[test]
    fn read_snapshot_caps_decompressed_zstd_bomb() {
        let payload = padded_snapshot_json(100_000);
        let compressed = zstd::encode_all(payload.as_bytes(), 19).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.json.zst");
        std::fs::write(&path, &compressed).unwrap();

        let cap: u64 = 1024;
        let compressed_len = u64::try_from(compressed.len()).unwrap();
        assert!(
            compressed_len <= cap,
            "test precondition: compressed ({compressed_len}) must fit under the cap ({cap})"
        );
        let err = read_snapshot_capped(&path, cap).unwrap_err();
        assert!(
            matches!(err, MemoryError::SnapshotTooLarge { cap: c } if c == cap),
            "expected distinct SnapshotTooLarge (not an opaque serde EOF), got {err:?}"
        );
    }

    /// Inclusive-cap boundary: a plain snapshot whose decompressed size is exactly
    /// `cap` bytes must still read (the `+1` slack in `read_capped`), proving the
    /// cap does not falsely reject a legitimately maximal payload.
    #[test]
    fn read_snapshot_accepts_exactly_cap_sized_plain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exact.json");
        // Build a valid snapshot, then pad its config filler so the file length is
        // exactly some `cap`. We discover the base length, then size the filler.
        let base = padded_snapshot_json(0);
        let base_len = base.len();
        let target_filler = 200;
        let json = padded_snapshot_json(target_filler);
        let json_len = json.len();
        let cap = u64::try_from(json_len).unwrap();
        assert!(json_len > base_len);
        std::fs::write(&path, &json).unwrap();
        // cap == file size: the file-size guard (`> cap`) does not fire, and the
        // decompressed cap (`cap + 1` take) reads the whole exactly-`cap` payload.
        let snapshot = read_snapshot_capped(&path, cap).unwrap();
        assert_eq!(snapshot.embed_dim, 4);
    }

    /// Inclusive-cap boundary companion to
    /// [`read_snapshot_accepts_exactly_cap_sized_plain`]: a plain snapshot one
    /// byte OVER the cap (`cap = L - 1` for a file of length `L`) must be rejected
    /// with the distinct [`MemoryError::SnapshotTooLarge`] carrying that exact
    /// `cap`. Mirrors `read_pak_one_byte_over_cap_rejected` in `archive::pak`.
    ///
    /// For the plain (uncompressed) path the on-disk size *is* the decompressed
    /// size, so this trips the up-front file-size guard (`metadata.len() > cap`).
    /// Pairing accept-at-`cap` with reject-at-`cap + 1` pins the acceptance window
    /// to a single byte wide on the plain path — the inclusive contract.
    #[test]
    fn read_snapshot_caps_decompressed_one_byte_over_plain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("over.json");
        let json = padded_snapshot_json(200);
        let len = u64::try_from(json.len()).unwrap();
        assert!(
            len >= 2,
            "fixture must be >=2 bytes so cap = len - 1 is positive"
        );
        std::fs::write(&path, &json).unwrap();

        // cap = L - 1: the file is exactly one byte over the cap.
        let cap = len - 1;
        let err = read_snapshot_capped(&path, cap).unwrap_err();
        assert!(
            matches!(err, MemoryError::SnapshotTooLarge { cap: c } if c == cap),
            "expected SnapshotTooLarge {{ cap: {cap} }} one byte over the cap, got {err:?}"
        );
    }

    /// Mutation-proof companion to
    /// [`read_snapshot_accepts_exactly_cap_sized_plain`] for the **decompressed**
    /// cap: a gzip snapshot whose *decompressed* length is `L`, read with
    /// `cap = L - 1`, is one byte over the cap and must be rejected with the
    /// distinct [`MemoryError::SnapshotTooLarge`]. The compressed file is tiny, so
    /// it clears the up-front file-size guard and the trip must come from
    /// `read_capped`'s `limit() == 0` check on the decompressed stream — exactly
    /// the [`std::io::Read::take`] slack under test.
    ///
    /// This is what pins the slack to *exactly* `+1`: under a regression that
    /// widened it to `+2`, the `take(cap + 2)` would read all `L = cap + 1`
    /// decompressed bytes without exhausting (leaving `limit() == 1`), serde would
    /// parse the complete valid JSON, and the read would wrongly succeed — failing
    /// this test. (The plain companion above cannot witness that mutation: its
    /// file-size guard short-circuits before `read_capped` runs.)
    #[cfg(feature = "compress-gzip")]
    #[test]
    fn read_snapshot_caps_decompressed_one_byte_over_gzip() {
        use flate2::Compression as GzLevel;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let payload = padded_snapshot_json(200);
        let decompressed_len = u64::try_from(payload.len()).unwrap();
        assert!(
            decompressed_len >= 2,
            "fixture must decompress to >=2 bytes so cap = len - 1 is positive"
        );

        let mut enc = GzEncoder::new(Vec::new(), GzLevel::best());
        enc.write_all(payload.as_bytes()).unwrap();
        let compressed = enc.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("over.json.gz");
        std::fs::write(&path, &compressed).unwrap();

        // cap = decompressed_len - 1: the decompressed stream is one byte over the
        // cap, while the compressed file is tiny and clears the file-size guard.
        let cap = decompressed_len - 1;
        let compressed_len = u64::try_from(compressed.len()).unwrap();
        assert!(
            compressed_len <= cap,
            "test precondition: compressed ({compressed_len}) must clear the file-size guard \
             (cap {cap}) so the *decompressed* cap is what trips"
        );
        let err = read_snapshot_capped(&path, cap).unwrap_err();
        assert!(
            matches!(err, MemoryError::SnapshotTooLarge { cap: c } if c == cap),
            "expected SnapshotTooLarge {{ cap: {cap} }} one byte over the decompressed cap, \
             got {err:?}"
        );
    }

    // --- Embedding-dimension validation tests (#413) ---

    fn snapshot_with(
        facts: Vec<crate::types::Fact>,
        summaries: Vec<crate::types::Summary>,
        embedding_spaces: Vec<crate::inspect::types::EmbeddingSpaceSnapshot>,
        fact_vectors: Vec<crate::inspect::types::FactVectorSnapshot>,
    ) -> EngineSnapshot {
        EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 4,
            facts,
            edges: vec![],
            summaries,
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces,
            fact_vectors,
            config: BTreeMap::new(),
        }
    }

    fn fact_with_embedding(id: i64, embedding: Vec<f32>) -> crate::types::Fact {
        use chrono::Utc;
        crate::types::Fact {
            id,
            content: format!("f{id}"),
            content_hash: format!("h{id}"),
            embedding,
            fact_type: crate::types::FactType::Semantic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.0,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.0,
            surfaced_at: None,
        }
    }

    fn summary_with_embedding(id: i64, embedding: Vec<f32>) -> crate::types::Summary {
        use chrono::Utc;
        crate::types::Summary {
            id,
            content: format!("s{id}"),
            embedding,
            level: crate::types::ConsolidationLevel::Local,
            source_fact_ids: vec![],
            created_at: Utc::now(),
            scope_id: 1,
        }
    }

    #[test]
    fn validate_rejects_fact_dim_mismatch() {
        // embed_dim is 4 but the fact carries a 3-vector.
        let snapshot = snapshot_with(
            vec![fact_with_embedding(1, vec![0.1, 0.2, 0.3])],
            vec![],
            vec![],
            vec![],
        );
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension {
                    expected: 4,
                    actual: 3
                }
            ),
            "expected EmbeddingDimension{{4,3}}, got {err:?}"
        );
    }

    #[test]
    fn validate_rejects_summary_dim_mismatch() {
        let snapshot = snapshot_with(
            vec![],
            vec![summary_with_embedding(1, vec![0.1, 0.2, 0.3, 0.4, 0.5])],
            vec![],
            vec![],
        );
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension {
                    expected: 4,
                    actual: 5
                }
            ),
            "expected EmbeddingDimension{{4,5}}, got {err:?}"
        );
    }

    #[test]
    fn validate_accepts_matching_dims() {
        let snapshot = snapshot_with(
            vec![fact_with_embedding(1, vec![0.1, 0.2, 0.3, 0.4])],
            vec![summary_with_embedding(1, vec![0.5, 0.6, 0.7, 0.8])],
            vec![],
            vec![],
        );
        validate_snapshot(&snapshot).unwrap();
    }

    #[test]
    fn validate_rejects_fact_vector_dim_mismatch_against_its_space() {
        use crate::inspect::types::{EmbeddingSpaceSnapshot, FactVectorSnapshot};
        // The "shadow" space declares dim 4, but its fact_vectors row is a 2-vector.
        let snapshot = snapshot_with(
            vec![fact_with_embedding(1, vec![0.1, 0.2, 0.3, 0.4])],
            vec![],
            vec![EmbeddingSpaceSnapshot {
                name: "shadow".into(),
                status: "populating".into(),
                fingerprint: crate::types::EmbeddingFingerprint::new("shadow-model", "test", 4),
            }],
            vec![FactVectorSnapshot {
                fact_id: 1,
                space_id: "shadow".into(),
                embedding: vec![0.7, 0.7],
            }],
        );
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::EmbeddingDimension {
                    expected: 4,
                    actual: 2
                }
            ),
            "expected EmbeddingDimension{{4,2}}, got {err:?}"
        );
    }

    #[test]
    fn validate_accepts_different_dim_fact_vector_space() {
        // #742: a populating space may legitimately have a *different* dimension
        // than the active `embed_dim`. The fact_vectors row must be validated
        // against ITS space's dim (8), not the active dim (4) — so this snapshot,
        // whose shadow vectors are 8-wide for an 8-dim shadow space, must pass.
        use crate::inspect::types::{EmbeddingSpaceSnapshot, FactVectorSnapshot};
        let snapshot = snapshot_with(
            vec![fact_with_embedding(1, vec![0.1, 0.2, 0.3, 0.4])],
            vec![],
            vec![EmbeddingSpaceSnapshot {
                name: "shadow8".into(),
                status: "populating".into(),
                fingerprint: crate::types::EmbeddingFingerprint::new("big-model", "test", 8),
            }],
            vec![FactVectorSnapshot {
                fact_id: 1,
                space_id: "shadow8".into(),
                embedding: vec![0.0; 8],
            }],
        );
        validate_snapshot(&snapshot).unwrap();
    }

    #[test]
    fn validate_rejects_fact_vector_unknown_space() {
        use crate::inspect::types::FactVectorSnapshot;
        let snapshot = snapshot_with(
            vec![fact_with_embedding(1, vec![0.1, 0.2, 0.3, 0.4])],
            vec![],
            vec![], // no embedding_spaces — the fact_vectors space is unresolvable
            vec![FactVectorSnapshot {
                fact_id: 1,
                space_id: "ghost".into(),
                embedding: vec![0.7; 4],
            }],
        );
        let err = validate_snapshot(&snapshot).unwrap_err();
        assert!(
            matches!(err, MemoryError::Internal(ref m) if m.contains("unknown embedding space")),
            "expected unknown-space Internal error, got {err:?}"
        );
    }

    #[test]
    fn restore_rejects_dim_mismatch_end_to_end() {
        // The whole restore path refuses a dim-mismatched snapshot up front,
        // before any row is written (atomic refusal, not a partial import).
        let snapshot = snapshot_with(
            vec![fact_with_embedding(1, vec![0.1, 0.2, 0.3])], // 3 != embed_dim 4
            vec![],
            vec![],
            vec![],
        );
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();
        let err = restore_snapshot_into(&conn, &snapshot).unwrap_err();
        assert!(
            matches!(err, MemoryError::EmbeddingDimension { .. }),
            "expected EmbeddingDimension, got {err:?}"
        );
        // Nothing was imported.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "no facts should be written on a rejected restore");
    }

    // --- Persisted-format serialization tests (super-qa #505) ---

    /// Equivalence guard: documents that the canonical serializers and the
    /// historical `Debug + to_lowercase` shape coincide on every current
    /// variant. This intentionally encodes the coincidence so the test fails
    /// the day a multi-word or renamed variant breaks it — the latent footgun
    /// the restore path used to rely on.
    #[test]
    fn canonical_serializers_match_debug_lowercase() {
        use crate::store::facts::fact_type_to_str;
        use crate::store::summaries::level_to_str;
        use crate::types::{ConsolidationLevel, FactType};

        for ft in [FactType::Episodic, FactType::Semantic, FactType::Procedural] {
            assert_eq!(
                fact_type_to_str(&ft),
                format!("{ft:?}").to_lowercase(),
                "fact_type_to_str diverged from Debug+to_lowercase for {ft:?}"
            );
        }

        for level in [
            ConsolidationLevel::Local,
            ConsolidationLevel::Cluster,
            ConsolidationLevel::Global,
        ] {
            assert_eq!(
                level_to_str(&level),
                format!("{level:?}").to_lowercase(),
                "level_to_str diverged from Debug+to_lowercase for {level:?}"
            );
        }
    }

    /// On-disk cross-path compatibility: a fact written via `FactStore::insert`
    /// and the same `FactType` written via `restore_facts` must produce the
    /// identical `fact_type` column string. Likewise for a summary's `level`
    /// column. Guards against the restore path drifting from the canonical
    /// write path (the read-side `str_to_*` parsers only accept the canonical
    /// strings).
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive round-trip test covering all FactType and ConsolidationLevel variants; extracting helpers would scatter the assertions"
    )]
    fn restore_writes_canonical_column_strings() {
        use crate::store::facts::FactStore;
        use crate::types::{ConsolidationLevel, FactType, NewFact};
        use chrono::Utc;

        // 1. Write one fact per FactType through the canonical insert path and
        //    record the raw on-disk fact_type column string.
        let canon = open_memory().unwrap();
        init_schema(&canon).unwrap();
        migrate(&canon, None).unwrap();
        let store = FactStore::new(&canon, DIM);
        let now = Utc::now();
        let mut canonical_ft: HashMap<FactType, String> = HashMap::new();
        for ft in [FactType::Episodic, FactType::Semantic, FactType::Procedural] {
            let id = store
                .insert(&NewFact {
                    content: format!("fact {ft:?}"),
                    content_hash: format!("ch{ft:?}"),
                    embedding: vec![0.1, 0.2, 0.3, 0.4],
                    fact_type: ft,
                    t_created: now,
                    t_expired: None,
                    t_valid: None,
                    t_invalid: None,
                    source_event_id: None,
                    base_importance: 0.0,
                    access_count: 0,
                    last_accessed: now,
                    metadata: serde_json::json!({}),
                    scope_id: 1,
                    is_pinned: false,
                })
                .unwrap();
            let col: String = canon
                .query_row("SELECT fact_type FROM facts WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
            canonical_ft.insert(ft, col);
        }

        // 2. Build a snapshot whose facts cover every FactType and whose
        //    summaries cover every ConsolidationLevel, then restore it into a
        //    fresh DB.
        let make_fact = |id: i64, ft: FactType| crate::types::Fact {
            id,
            content: format!("snap {ft:?}"),
            content_hash: format!("h{id}"),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            fact_type: ft,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            base_importance: 0.0,
            access_count: 0,
            last_accessed: now,
            metadata: serde_json::json!({}),
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.0,
            surfaced_at: None,
        };
        let make_summary = |id: i64, level: ConsolidationLevel| crate::types::Summary {
            id,
            content: format!("summary {level:?}"),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            level,
            source_fact_ids: vec![],
            created_at: now,
            scope_id: 1,
        };
        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: DIM,
            facts: vec![
                make_fact(1, FactType::Episodic),
                make_fact(2, FactType::Semantic),
                make_fact(3, FactType::Procedural),
            ],
            edges: vec![],
            summaries: vec![
                make_summary(1, ConsolidationLevel::Local),
                make_summary(2, ConsolidationLevel::Cluster),
                make_summary(3, ConsolidationLevel::Global),
            ],
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![],
            fact_vectors: vec![],
            config: BTreeMap::new(),
        };

        let restored = open_memory().unwrap();
        init_schema(&restored).unwrap();
        migrate(&restored, None).unwrap();
        restore_snapshot_into(&restored, &snapshot).unwrap();

        // 3a. Raw fact_type column strings must be the canonical lowercase
        //     names AND must match the canonical insert path byte-for-byte.
        for (ft, expected_literal) in [
            (FactType::Episodic, "episodic"),
            (FactType::Semantic, "semantic"),
            (FactType::Procedural, "procedural"),
        ] {
            let col: String = restored
                .query_row(
                    "SELECT fact_type FROM facts WHERE content = ?1",
                    [format!("snap {ft:?}")],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                col, expected_literal,
                "restore wrote wrong fact_type for {ft:?}"
            );
            assert_eq!(
                col, canonical_ft[&ft],
                "restore vs FactStore::insert disagree on fact_type column for {ft:?}"
            );
        }

        // 3b. Raw level column strings must be the canonical lowercase names.
        for (level, expected_literal) in [
            (ConsolidationLevel::Local, "local"),
            (ConsolidationLevel::Cluster, "cluster"),
            (ConsolidationLevel::Global, "global"),
        ] {
            let col: String = restored
                .query_row(
                    "SELECT level FROM summaries WHERE content = ?1",
                    [format!("summary {level:?}")],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                col, expected_literal,
                "restore wrote wrong level for {level:?}"
            );
        }
    }

    // --- Serde backward-compat test ---

    #[test]
    fn event_deserializes_without_optional_fields() {
        // Simulate an older snapshot JSON that lacks origin_node_id, sequence_id.
        let json = r#"{
            "id": 1,
            "timestamp": "2026-01-01T00:00:00Z",
            "event_type": "Interaction",
            "payload": {},
            "source": "test",
            "session_id": null,
            "scope_id": 1,
            "created_at": null,
            "event_revision": 1
        }"#;
        let event: crate::types::Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.origin_node_id, "local");
        assert_eq!(event.sequence_id, 0);
    }

    // --- fact_vectors dump/restore (#623 T5) ---

    #[tokio::test]
    async fn round_trip_preserves_fact_vectors() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        // Ingest facts (records the active "default" identity in facts.embedding).
        let mut ids = Vec::new();
        for c in ["a", "b"] {
            let id = engine
                .add_fact(
                    &AddFactRequest {
                        content: c.into(),
                        fact_type: FactType::Semantic,
                        source_event_id: None,
                        scope: None,
                        opts: None,
                    },
                    std::sync::Arc::new(FakeEmbed) as std::sync::Arc<dyn EmbeddingProvider>,
                    None,
                )
                .await
                .unwrap();
            ids.push(id);
        }
        // Stage a populating space with distinct shadow vectors (an in-progress
        // reconstruction): fact_vectors now holds non-active rows.
        let shadow_fp = crate::types::EmbeddingFingerprint::new("shadow-model", "test", DIM);
        engine
            .storage()
            .begin_populating_space("shadow", &shadow_fp)
            .await
            .unwrap();
        let rows: Vec<(i64, Vec<f32>)> = ids.iter().map(|&id| (id, vec![0.7_f32; DIM])).collect();
        engine
            .storage()
            .write_backfill_batch("shadow", rows)
            .await
            .unwrap();

        // Dump → the streamed snapshot carries the shadow vectors.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(path.clone()))
            .await
            .unwrap();
        let snapshot = read_snapshot(&path).unwrap();
        assert_eq!(snapshot.fact_vectors.len(), 2, "shadow vectors captured");
        assert!(
            snapshot
                .fact_vectors
                .iter()
                .all(|fv| fv.space_id == "shadow" && fv.embedding == vec![0.7_f32; DIM])
        );

        // Restore into a fresh DB: the FK order (spaces → facts → fact_vectors) holds.
        let conn2 = open_memory().unwrap();
        init_schema(&conn2).unwrap();
        migrate(&conn2, None).unwrap();
        restore_snapshot_into(&conn2, &snapshot).unwrap();

        let n: i64 = conn2
            .query_row(
                "SELECT COUNT(*) FROM fact_vectors WHERE space_id = 'shadow'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "shadow vectors survived restore");
        // The active vectors (facts.embedding) round-trip unchanged.
        let blob: Vec<u8> = conn2
            .query_row("SELECT embedding FROM facts WHERE id = ?1", [ids[0]], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            crate::store::deserialize_embedding(&blob, DIM).unwrap(),
            vec![0.1, 0.2, 0.3, 0.4],
            "active vector unchanged (still in facts.embedding)"
        );
    }

    #[test]
    fn pre_623_snapshot_defaults_empty_fact_vectors() {
        // A snapshot predating #623 has no `fact_vectors` field — serde(default)
        // makes it deserialize to empty (the active vectors are in facts[].embedding).
        let json = r#"{"schema_version":13,"storage_epoch":1,"embed_dim":4,"facts":[],
            "edges":[],"summaries":[],"scopes":[],"events":[],"lineage":[],
            "embedding_spaces":[],"config":{}}"#;
        let snapshot: EngineSnapshot = serde_json::from_str(json).unwrap();
        assert!(
            snapshot.fact_vectors.is_empty(),
            "absent fact_vectors defaults to empty"
        );
    }

    // --- MANAGED_CONFIG_KEYS exclusion during restore (#316) ---

    /// Restore must NOT import the keys in [`MANAGED_CONFIG_KEYS`] from a snapshot's
    /// `config` map — those rows are owned by `init_schema`/`migrate`. A hand-crafted
    /// snapshot carrying `schema_version: "9999"` (and the other managed keys) must
    /// leave the DB's own managed values intact, while a non-managed `custom_key` IS
    /// imported.
    ///
    /// Non-vacuous & mutation-proof: the expected values are *asymmetric* — the
    /// managed `schema_version` must stay `CURRENT_SCHEMA_VERSION` ("14") and reject
    /// the snapshot's "9999"; `storage_epoch` must stay "1" and reject "9999"; the
    /// custom key must equal exactly "imported-value". If a regression dropped the
    /// `!MANAGED_CONFIG_KEYS.contains(...)` filter, `schema_version` would become
    /// "9999" and `storage_epoch` "9999" — both caught here. `embedding_meta` is the
    /// legacy managed key: it must NOT land in `config` (it is consumed by the
    /// embedding-space restore fallback instead), so its config row stays absent.
    #[test]
    fn restore_does_not_import_managed_config_keys() {
        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        // Sanity: the DB's own managed values before restore.
        assert_eq!(
            get_config(&conn, "schema_version").unwrap().as_deref(),
            Some(CURRENT_SCHEMA_VERSION.to_string().as_str())
        );
        assert_eq!(
            get_config(&conn, "storage_epoch").unwrap().as_deref(),
            Some(STORAGE_EPOCH.to_string().as_str())
        );

        // A legacy embedding_meta value: valid fingerprint JSON so the embedding-space
        // restore fallback consumes it (embedding_spaces is empty) rather than erroring.
        let fp = crate::types::EmbeddingFingerprint::new("legacy", "test", 4);
        let mut config = BTreeMap::new();
        config.insert("schema_version".to_string(), "9999".to_string());
        config.insert("storage_epoch".to_string(), "9999".to_string());
        config.insert(
            "embedding_meta".to_string(),
            serde_json::to_string(&fp).unwrap(),
        );
        config.insert("custom_key".to_string(), "imported-value".to_string());

        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 4,
            facts: vec![],
            edges: vec![],
            summaries: vec![],
            scopes: vec![],
            events: vec![],
            lineage: vec![],
            embedding_spaces: vec![], // empty → legacy embedding_meta fallback runs
            fact_vectors: vec![],
            config,
        };

        restore_snapshot_into(&conn, &snapshot).unwrap();

        // Managed keys: the DB's own values survive; the snapshot's "9999" is rejected.
        assert_eq!(
            get_config(&conn, "schema_version").unwrap().as_deref(),
            Some(CURRENT_SCHEMA_VERSION.to_string().as_str()),
            "managed schema_version must NOT be overwritten by the snapshot"
        );
        assert_eq!(
            get_config(&conn, "storage_epoch").unwrap().as_deref(),
            Some(STORAGE_EPOCH.to_string().as_str()),
            "managed storage_epoch must NOT be overwritten by the snapshot"
        );
        // The legacy managed key is never copied into `config` (it routes to the
        // embedding-space registry instead).
        assert!(
            get_config(&conn, "embedding_meta").unwrap().is_none(),
            "embedding_meta is managed → must not be imported as a config row"
        );
        // ...but its value WAS consumed: the identity landed in the registry.
        assert_eq!(
            crate::store::embedding_meta::load(&conn).unwrap(),
            Some(fp),
            "legacy embedding_meta value reconstructed into the registry"
        );

        // The non-managed custom key IS imported, verbatim.
        assert_eq!(
            get_config(&conn, "custom_key").unwrap().as_deref(),
            Some("imported-value"),
            "non-managed config keys must be imported"
        );
    }

    // --- MAX_SNAPSHOT_SIZE file-cap guard for read_snapshot (#464) ---

    /// The 4 GiB cap of the *public* [`read_snapshot`] entry point fires on an
    /// oversized on-disk file — proved without materializing 4 GiB by `set_len`-ing
    /// a sparse file to `MAX_SNAPSHOT_SIZE + 1` (the proven technique from
    /// `engine/snapshot.rs`'s binary-loader cap). #877 added the cap+1 decompression
    /// logic and tests the *parameterized* `read_snapshot_capped`; the remaining gap
    /// is that the public `read_snapshot` actually wires [`MAX_SNAPSHOT_SIZE`] as its
    /// cap. This pins that wiring: a `MAX_SNAPSHOT_SIZE + 1` file is rejected with the
    /// distinct [`MemoryError::SnapshotTooLarge`] carrying exactly that cap.
    ///
    /// Mutation-proof: it asserts the error's `cap` equals `MAX_SNAPSHOT_SIZE` — a
    /// regression that passed a *different* cap (e.g. `u64::MAX`, disabling the guard)
    /// would either not fire at all or carry the wrong cap, failing the match. The
    /// sparse file occupies ~0 real disk bytes, so the test stays cheap.
    #[test]
    fn read_snapshot_rejects_file_over_max_snapshot_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.json");
        let f = File::create(&path).unwrap();
        // Sparse: logical length = cap + 1, physical bytes ~= 0.
        f.set_len(MAX_SNAPSHOT_SIZE + 1).unwrap();
        drop(f);

        let err = read_snapshot(&path).unwrap_err();
        assert!(
            matches!(err, MemoryError::SnapshotTooLarge { cap } if cap == MAX_SNAPSHOT_SIZE),
            "expected SnapshotTooLarge {{ cap: {MAX_SNAPSHOT_SIZE} }} from the public \
             read_snapshot, got {err:?}"
        );
    }

    // --- Lineage restore orchestration (#465) ---

    /// The restore *orchestration* (step 7 of [`restore_snapshot_into`]) must carry a
    /// non-empty `lineage` section through `LineageStore::insert_raw`, AND reset the
    /// lineage autoincrement so a post-restore insert gets an id strictly greater than
    /// any imported `lineage_id`. Every in-module restore fixture uses `lineage: vec![]`,
    /// so this step-7 wiring + the autoincrement reset were untested end-to-end.
    ///
    /// Non-vacuous & mutation-proof on *distinct* values:
    /// - The imported row preserves its explicit `lineage_id = 42` and
    ///   `wisdom_fact_id = 1` (asymmetric, so a column swap is caught).
    /// - `COUNT(lineage) == 1` (a dropped step-7 loop would give 0).
    /// - A fresh `insert_raw` afterwards lands at `lineage_id > 42` (a missing
    ///   autoincrement reset would collide/reuse a low id).
    #[test]
    fn restore_roundtrip_with_lineage() {
        use crate::types::{LineageSnapshotEntry, PromotionProvenance};
        use chrono::Utc;

        // One wisdom fact + two source facts so the lineage references resolve.
        let wisdom = fact_with_embedding(1, vec![0.1, 0.2, 0.3, 0.4]);
        let src_a = fact_with_embedding(10, vec![0.1, 0.2, 0.3, 0.4]);
        let src_b = fact_with_embedding(20, vec![0.1, 0.2, 0.3, 0.4]);

        let imported_lineage_id = 42_i64;
        let entry = LineageSnapshotEntry {
            lineage_id: imported_lineage_id,
            wisdom_fact_id: 1,
            source_fact_ids: vec![10, 20],
            provenance: PromotionProvenance {
                source_count: 2,
                session_count: 1,
                date_range_start: Utc::now(),
                date_range_end: Utc::now(),
                confidence: 0.77,
                method_version: "test-v1".into(),
                representative_ids: vec![10, 20],
            },
        };

        let snapshot = EngineSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            storage_epoch: STORAGE_EPOCH,
            embed_dim: 4,
            facts: vec![wisdom, src_a, src_b],
            edges: vec![],
            summaries: vec![],
            scopes: vec![],
            events: vec![],
            lineage: vec![entry],
            embedding_spaces: vec![],
            fact_vectors: vec![],
            config: BTreeMap::new(),
        };

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();
        restore_snapshot_into(&conn, &snapshot).unwrap();

        // Exactly one lineage row, with the imported PK + wisdom fact preserved.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM lineage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "step-7 must import the one lineage row");
        let (lid, wfid): (i64, i64) = conn
            .query_row("SELECT lineage_id, wisdom_fact_id FROM lineage", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(lid, imported_lineage_id, "explicit lineage_id preserved");
        assert_eq!(
            wfid, 1,
            "wisdom_fact_id preserved (not swapped with lineage_id)"
        );

        // The source chain round-tripped through insert_raw.
        let store = LineageStore::new(&conn);
        assert_eq!(store.get_source_fact_ids(1).unwrap(), vec![10, 20]);

        // ID-continuity after restore: a fresh post-restore insert (via the
        // validating `insert`, which omits `lineage_id` and lets the table assign the
        // next AUTOINCREMENT value) must land STRICTLY ABOVE the imported id, never
        // colliding with or reusing it. This guards the consumer-visible guarantee
        // that imported provenance ids stay stable while new rows extend past them.
        //
        // Note: with an explicit `lineage_id = 42` row present, SQLite already raises
        // its internal AUTOINCREMENT floor to 42 on that insert, so this assertion
        // does not in isolation prove the manual `reset_autoincrement("lineage", …)`
        // call fires — it proves the end-to-end continuity property, which is what a
        // caller relies on. The step-7 import wiring is what the COUNT/id assertions
        // above pin.
        let new_id = store
            .insert(
                &crate::types::NewLineageRecord {
                    wisdom_fact_id: 10, // a different, existing fact (unique wisdom_fact_id)
                    source_fact_ids: vec![20],
                },
                &PromotionProvenance {
                    source_count: 1,
                    session_count: 1,
                    date_range_start: Utc::now(),
                    date_range_end: Utc::now(),
                    confidence: 0.5,
                    method_version: "post".into(),
                    representative_ids: vec![20],
                },
            )
            .unwrap();
        assert!(
            new_id > imported_lineage_id,
            "post-restore lineage id {new_id} must exceed imported {imported_lineage_id}"
        );
    }

    // --- NotImplemented compression arms when feature disabled (#466) ---
    //
    // These tests exercise the `#[cfg(not(feature = "compress-*"))]` arms of
    // `read_gzip` / `read_zstd`, which return `MemoryError::NotImplemented`. They are
    // gated on the feature being DISABLED, so they compile and run ONLY under a build
    // without that compress feature (e.g. the default-feature `cargo test`).
    //
    // ⚠️ CI CAVEAT: this repo's CI runs tests solely via `cargo test --workspace
    // --all-features` (see .github/workflows/ci.yml). Under `--all-features` both
    // compress features are ON, so these `cfg(not(...))` tests are compiled OUT and
    // NEVER execute in CI. They guard the NotImplemented arms only under a local
    // default-feature (`cargo test`) run. This is the known "default-features test
    // masked by --all-features" gap (#466 explicitly flags it); closing it for real
    // would require a no-compress test job in CI, out of scope for this test-coverage PR.

    /// gzip magic on a snapshot file, with `compress-gzip` disabled, must surface a
    /// clear [`MemoryError::NotImplemented`] (not a serde/Io error). Mutation-proof:
    /// asserts the variant AND that the message names "gzip" — a regression that
    /// returned a generic error or mislabelled the format fails the match/contains.
    #[cfg(not(feature = "compress-gzip"))]
    #[test]
    fn read_snapshot_gzip_without_feature_is_not_implemented() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.json.gz");
        // gzip magic [0x1f, 0x8b, ...] — detect_compression routes to read_gzip.
        std::fs::write(&path, [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00]).unwrap();
        let err = read_snapshot(&path).unwrap_err();
        assert!(
            matches!(err, MemoryError::NotImplemented(ref m) if m.contains("gzip")),
            "expected NotImplemented naming gzip, got {err:?}"
        );
    }

    /// zstd magic on a snapshot file, with `compress-zstd` disabled, must surface a
    /// clear [`MemoryError::NotImplemented`] naming "zstd".
    #[cfg(not(feature = "compress-zstd"))]
    #[test]
    fn read_snapshot_zstd_without_feature_is_not_implemented() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.json.zst");
        // zstd magic [0x28, 0xb5, 0x2f, 0xfd] — routes to read_zstd.
        std::fs::write(&path, [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x00]).unwrap();
        let err = read_snapshot(&path).unwrap_err();
        assert!(
            matches!(err, MemoryError::NotImplemented(ref m) if m.contains("zstd")),
            "expected NotImplemented naming zstd, got {err:?}"
        );
    }

    // --- EngineSnapshot serde roundtrip proptest (#467) ---

    mod proptest_snapshot {
        use super::*;
        use crate::types::{
            ConsolidationLevel, Edge, Event, EventType, Fact, FactType, LineageSnapshotEntry,
            PromotionProvenance, ScopeNode, Summary,
        };
        use chrono::{DateTime, TimeZone, Utc};
        use proptest::prelude::*;

        const PDIM: usize = 4;

        /// A bounded, valid UTC timestamp (avoids chrono's extreme-year edge cases
        /// that don't survive RFC3339; the snapshot wire format is RFC3339-based).
        fn arb_ts() -> impl Strategy<Value = DateTime<Utc>> {
            // 2000-01-01 .. ~2100 in seconds.
            (946_684_800_i64..4_102_444_800_i64)
                .prop_map(|s| Utc.timestamp_opt(s, 0).single().expect("valid timestamp"))
        }

        fn arb_opt_ts() -> impl Strategy<Value = Option<DateTime<Utc>>> {
            proptest::option::of(arb_ts())
        }

        /// A finite embedding of exactly `PDIM` floats (NaN/inf don't survive JSON).
        fn arb_embedding() -> impl Strategy<Value = Vec<f32>> {
            proptest::collection::vec(-1.0e3_f32..1.0e3_f32, PDIM..=PDIM)
        }

        fn arb_fact_type() -> impl Strategy<Value = FactType> {
            prop_oneof![
                Just(FactType::Episodic),
                Just(FactType::Semantic),
                Just(FactType::Procedural),
            ]
        }

        fn arb_level() -> impl Strategy<Value = ConsolidationLevel> {
            prop_oneof![
                Just(ConsolidationLevel::Local),
                Just(ConsolidationLevel::Cluster),
                Just(ConsolidationLevel::Global),
            ]
        }

        fn arb_fact() -> impl Strategy<Value = Fact> {
            (
                1..10_000i64,
                ".*",
                arb_embedding(),
                arb_fact_type(),
                arb_ts(),
                arb_opt_ts(),
                arb_opt_ts(),
                arb_opt_ts(),
                proptest::option::of(1..10_000i64),
                any::<bool>(),
            )
                .prop_map(
                    |(
                        id,
                        content,
                        embedding,
                        ft,
                        t_created,
                        t_expired,
                        t_valid,
                        t_invalid,
                        sev,
                        pinned,
                    )| {
                        Fact {
                            id,
                            content,
                            content_hash: format!("h{id}"),
                            embedding,
                            fact_type: ft,
                            t_created,
                            t_expired,
                            t_valid,
                            t_invalid,
                            source_event_id: sev,
                            base_importance: 0.5,
                            access_count: i64::from(pinned),
                            last_accessed: t_created,
                            metadata: serde_json::json!({"k": id}),
                            scope_id: 1,
                            is_pinned: pinned,
                            importance_score: 0.25,
                            surfaced_at: None,
                        }
                    },
                )
        }

        fn arb_edge() -> impl Strategy<Value = Edge> {
            (
                1..10_000i64,
                1..10_000i64,
                1..10_000i64,
                arb_ts(),
                arb_opt_ts(),
            )
                .prop_map(|(id, src, tgt, t_created, t_expired)| Edge {
                    id,
                    source_fact_id: src,
                    target_fact_id: tgt,
                    relation_type: "rel".into(),
                    weight: 0.5,
                    t_created,
                    t_expired,
                    scope_id: 1,
                })
        }

        fn arb_summary() -> impl Strategy<Value = Summary> {
            (
                1..10_000i64,
                ".*",
                arb_embedding(),
                arb_level(),
                proptest::collection::vec(1..10_000i64, 0..4),
                arb_ts(),
            )
                .prop_map(|(id, content, embedding, level, src, created_at)| Summary {
                    id,
                    content,
                    embedding,
                    level,
                    source_fact_ids: src,
                    created_at,
                    scope_id: 1,
                })
        }

        fn arb_event() -> impl Strategy<Value = Event> {
            (1..10_000i64, arb_ts(), proptest::option::of(".*")).prop_map(
                |(id, timestamp, session_id)| Event {
                    id,
                    timestamp,
                    event_type: EventType::Interaction,
                    payload: serde_json::json!({"e": id}),
                    source: "test".into(),
                    session_id,
                    scope_id: 1,
                    origin_node_id: "local".into(),
                    sequence_id: id,
                    created_at: Some(timestamp),
                    event_revision: 1,
                },
            )
        }

        fn arb_lineage() -> impl Strategy<Value = LineageSnapshotEntry> {
            (
                1..10_000i64,
                1..10_000i64,
                proptest::collection::vec(1..10_000i64, 0..4),
                arb_ts(),
                arb_ts(),
            )
                .prop_map(|(lineage_id, wisdom_fact_id, src, start, end)| {
                    LineageSnapshotEntry {
                        lineage_id,
                        wisdom_fact_id,
                        source_fact_ids: src,
                        provenance: PromotionProvenance {
                            source_count: 1,
                            session_count: 1,
                            date_range_start: start,
                            date_range_end: end,
                            confidence: 0.5,
                            method_version: "v".into(),
                            representative_ids: vec![],
                        },
                    }
                })
        }

        fn arb_snapshot() -> impl Strategy<Value = EngineSnapshot> {
            (
                proptest::collection::vec(arb_fact(), 0..4),
                proptest::collection::vec(arb_edge(), 0..4),
                proptest::collection::vec(arb_summary(), 0..4),
                proptest::collection::vec(arb_event(), 0..4),
                proptest::collection::vec(arb_lineage(), 0..3),
            )
                .prop_map(|(facts, edges, summaries, events, lineage)| {
                    let scopes = vec![ScopeNode {
                        id: 1,
                        parent_id: None,
                        label: "root".into(),
                        depth: 0,
                    }];
                    let mut config = BTreeMap::new();
                    config.insert("custom".to_string(), "value".to_string());
                    EngineSnapshot {
                        schema_version: CURRENT_SCHEMA_VERSION,
                        storage_epoch: STORAGE_EPOCH,
                        embed_dim: PDIM,
                        facts,
                        edges,
                        summaries,
                        scopes,
                        events,
                        lineage,
                        embedding_spaces: vec![],
                        fact_vectors: vec![],
                        config,
                    }
                })
        }

        proptest! {
            /// `serialize → deserialize → serialize` is byte-stable for arbitrary
            /// `EngineSnapshot`s, and the scalar/length invariants survive. This is the
            /// dump↔restore wire contract: any serde drift (a renamed field, a
            /// non-round-tripping timestamp, a dropped collection element) breaks the
            /// JSON stability check or a length assertion.
            ///
            /// `EngineSnapshot` is not `PartialEq` (it nests `serde_json::Value` and
            /// float scores), so JSON-value stability stands in for structural equality
            /// — the same approach as the example-based `engine_snapshot_roundtrip`.
            #[test]
            fn engine_snapshot_serde_roundtrip(snapshot in arb_snapshot()) {
                let json1 = serde_json::to_value(&snapshot).expect("serialize #1");
                let recovered: EngineSnapshot =
                    serde_json::from_value(json1.clone()).expect("deserialize");
                let json2 = serde_json::to_value(&recovered).expect("serialize #2");
                prop_assert_eq!(&json1, &json2, "snapshot JSON not stable across roundtrip");

                // Scalar + collection-length invariants (catch a dropped/duplicated
                // element that a coincidental value-equality might miss).
                prop_assert_eq!(snapshot.schema_version, recovered.schema_version);
                prop_assert_eq!(snapshot.storage_epoch, recovered.storage_epoch);
                prop_assert_eq!(snapshot.embed_dim, recovered.embed_dim);
                prop_assert_eq!(snapshot.facts.len(), recovered.facts.len());
                prop_assert_eq!(snapshot.edges.len(), recovered.edges.len());
                prop_assert_eq!(snapshot.summaries.len(), recovered.summaries.len());
                prop_assert_eq!(snapshot.events.len(), recovered.events.len());
                prop_assert_eq!(snapshot.lineage.len(), recovered.lineage.len());
                prop_assert_eq!(snapshot.config, recovered.config);
            }
        }
    }
}
