//! Restore engine state from a snapshot (JSON or SQLite backup).
//!
//! Complements the export side in [`super::dump`]. Import always targets a fresh
//! (empty) database — no merge/additive semantics.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::store::events::event_type_to_str;
use crate::store::schema::{CURRENT_SCHEMA_VERSION, STORAGE_EPOCH, set_config};
use crate::store::serialize_embedding;

use super::types::EngineSnapshot;

/// Config keys managed by `init_schema`/`migrate` — never imported from snapshots.
const MANAGED_CONFIG_KEYS: &[&str] = &["schema_version", "storage_epoch"];

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

/// Maximum snapshot file size (4 GiB). Prevents OOM from crafted snapshots.
const MAX_SNAPSHOT_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Deserialize an [`EngineSnapshot`] from a (possibly compressed) JSON file.
///
/// Auto-detects compression from magic bytes. Returns a clear error if the
/// file uses a compression format whose feature is not enabled.
///
/// # Errors
///
/// - [`MemoryError::Io`] on file access failure.
/// - [`MemoryError::Serialization`] on malformed JSON.
/// - [`MemoryError::NotImplemented`] if compression detected but feature disabled.
/// - [`MemoryError::Internal`] if the file exceeds 4 GiB.
pub fn read_snapshot(path: &Path) -> Result<EngineSnapshot> {
    // Open the file once and perform all checks on the handle to avoid TOCTOU.
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_SNAPSHOT_SIZE {
        return Err(MemoryError::Internal(format!(
            "snapshot file too large: {} bytes (max {MAX_SNAPSHOT_SIZE})",
            metadata.len()
        )));
    }

    let compression = detect_compression(&mut file)?;
    let reader = BufReader::new(file);

    match compression {
        Compression::None => Ok(serde_json::from_reader(reader)?),
        Compression::Gzip => read_gzip(reader),
        Compression::Zstd => read_zstd(reader),
    }
}

#[cfg(feature = "compress-gzip")]
fn read_gzip(reader: impl Read) -> Result<EngineSnapshot> {
    let decoder = flate2::read::GzDecoder::new(reader);
    Ok(serde_json::from_reader(decoder)?)
}

#[cfg(not(feature = "compress-gzip"))]
fn read_gzip(_reader: impl Read) -> Result<EngineSnapshot> {
    Err(MemoryError::NotImplemented(
        "gzip-compressed snapshot detected but `compress-gzip` feature is not enabled".into(),
    ))
}

#[cfg(feature = "compress-zstd")]
fn read_zstd(reader: impl Read) -> Result<EngineSnapshot> {
    let decoder =
        zstd::Decoder::new(reader).map_err(|e| MemoryError::Internal(format!("zstd init: {e}")))?;
    Ok(serde_json::from_reader(decoder)?)
}

#[cfg(not(feature = "compress-zstd"))]
fn read_zstd(_reader: impl Read) -> Result<EngineSnapshot> {
    Err(MemoryError::NotImplemented(
        "zstd-compressed snapshot detected but `compress-zstd` feature is not enabled".into(),
    ))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a snapshot against the current engine version.
///
/// Checks schema compatibility, storage epoch, embedding dimension, and the
/// root scope invariant required by [`crate::scope::ScopeTree`].
///
/// # Errors
///
/// - [`MemoryError::Migration`] if `schema_version` is from a newer schema.
/// - [`MemoryError::UnsupportedEpoch`] if `storage_epoch` doesn't match.
/// - [`MemoryError::Internal`] if `embed_dim` is zero or root scope is missing.
pub fn validate_snapshot(snapshot: &EngineSnapshot) -> Result<()> {
    if snapshot.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(MemoryError::Migration(format!(
            "snapshot schema_version {} is newer than supported {}",
            snapshot.schema_version, CURRENT_SCHEMA_VERSION
        )));
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

    let total = event_count + fact_count + edge_count + summary_count + scope_count;
    if total > 0 {
        return Err(MemoryError::Conflict(
            "target database is not empty; restore only works on a fresh engine".into(),
        ));
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
        let mut scopes = snapshot.scopes.clone();
        scopes.sort_by_key(|s| (s.depth, s.id));
        let mut stmt =
            tx.prepare("INSERT INTO scopes (id, parent_id, label, depth) VALUES (?1, ?2, ?3, ?4)")?;
        for scope in &scopes {
            stmt.execute(rusqlite::params![
                scope.id,
                scope.parent_id,
                scope.label,
                scope.depth,
            ])?;
        }
    }

    // 3. Insert events.
    {
        let mut stmt = tx.prepare(
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
    }

    // 4. Insert facts.
    {
        let mut stmt = tx.prepare(
            "INSERT INTO facts (id, content, content_hash, embedding, fact_type, t_created, \
             t_expired, t_valid, t_invalid, source_event_id, importance, access_count, \
             last_accessed, metadata, scope_id, is_pinned, importance_score, surfaced_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        )?;
        for fact in &snapshot.facts {
            let embedding_blob = serialize_embedding(&fact.embedding);
            let ft = format!("{:?}", fact.fact_type).to_lowercase();
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
                fact.importance,
                fact.access_count,
                last_accessed,
                metadata,
                fact.scope_id,
                fact.is_pinned,
                fact.importance_score,
                fact.surfaced_at.map(|dt| dt.to_rfc3339()),
            ])?;
        }
    }

    // 5. Insert edges.
    {
        let mut stmt = tx.prepare(
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
    }

    // 6. Insert summaries.
    {
        let mut stmt = tx.prepare(
            "INSERT INTO summaries (id, content, embedding, level, source_fact_ids, \
             created_at, scope_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for summary in &snapshot.summaries {
            let embedding_blob = serialize_embedding(&summary.embedding);
            let level = format!("{:?}", summary.level).to_lowercase();
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

    // 7. Import config keys (except managed ones).
    for (key, value) in &snapshot.config {
        if !MANAGED_CONFIG_KEYS.contains(&key.as_str()) {
            set_config(&tx, key, value)?;
        }
    }

    // 8. Reset autoincrement sequences so new inserts don't collide.
    reset_autoincrement(
        &tx,
        "scopes",
        &snapshot.scopes.iter().map(|s| s.id).collect::<Vec<_>>(),
    )?;
    reset_autoincrement(
        &tx,
        "events",
        &snapshot.events.iter().map(|e| e.id).collect::<Vec<_>>(),
    )?;
    reset_autoincrement(
        &tx,
        "facts",
        &snapshot.facts.iter().map(|f| f.id).collect::<Vec<_>>(),
    )?;
    reset_autoincrement(
        &tx,
        "edges",
        &snapshot.edges.iter().map(|e| e.id).collect::<Vec<_>>(),
    )?;
    reset_autoincrement(
        &tx,
        "summaries",
        &snapshot.summaries.iter().map(|s| s.id).collect::<Vec<_>>(),
    )?;

    // 9. Commit.
    tx.commit()?;
    Ok(())
}

/// Reset the `sqlite_sequence` entry for a table to the maximum imported ID.
///
/// `sqlite_sequence` is auto-created by SQLite for AUTOINCREMENT tables but
/// has no unique constraint on `name`, so we use DELETE + INSERT.
fn reset_autoincrement(conn: &Connection, table: &str, ids: &[i64]) -> Result<()> {
    let max_id = ids.iter().copied().max().unwrap_or(0);
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
    use std::collections::HashMap;

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
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
            config: HashMap::new(),
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
            config: HashMap::new(),
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
            config: HashMap::new(),
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
            config: HashMap::new(),
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
            config: HashMap::new(),
        };
        validate_snapshot(&snapshot).unwrap();
    }

    // --- Round-trip restore tests ---

    #[test]
    fn json_dump_restore_roundtrip() {
        // Create engine and add data.
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "alpha fact".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
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
                &FakeEmbed,
                None,
            )
            .unwrap();

        // Dump to JSON.
        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
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

        // Verify config imported (embed_dim should be present).
        let embed_dim_cfg = get_config(&conn, "embed_dim").unwrap();
        assert_eq!(embed_dim_cfg, Some("4".to_string()));
    }

    #[test]
    fn restore_rejects_non_empty_db() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "existing".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
            .unwrap();

        // Dump.
        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
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

    #[test]
    fn autoincrement_reset_after_restore() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "fact1".into(),
                    fact_type: FactType::Semantic,
                    source_event_id: None,
                    scope: None,
                    opts: None,
                },
                &FakeEmbed,
                None,
            )
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
                &FakeEmbed,
                None,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
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
}
