use std::fs;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use crate::error::{ConflictError, MemoryError, Result};
use crate::inspect::types::EmbeddingSpaceSnapshot;
use crate::store::UpcasterRegistry;
use crate::store::edges::EdgeStore;
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::lineage::LineageStore;
use crate::store::schema::{get_config, list_config};
use crate::store::scopes::ScopeStore;
use crate::store::summaries::SummaryStore;

/// Build a sibling temporary path for atomic write-then-rename.
fn tmp_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Stream engine state as JSON to `writer`, one entity at a time.
///
/// Produces the same JSON format as [`super::types::EngineSnapshot`] but never
/// holds more than one entity in memory per collection.  Peak memory drops from
/// O(total entities) to O(1), making this suitable for databases with 100K+
/// facts.
fn stream_snapshot<W: Write>(conn: &Connection, embed_dim: usize, writer: &mut W) -> Result<()> {
    let registry = UpcasterRegistry::new(); // raw events, no upcasting

    let schema_version: u32 = get_config(conn, "schema_version")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let storage_epoch: u16 = get_config(conn, "storage_epoch")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // Scalar header fields.
    //
    // INVARIANT: `embed_dim` MUST stay among these leading scalar fields, before
    // the streamed collections below. `memory-engine-cli`'s `import` peeks it from
    // the head of the snapshot under a small byte cap (`peek_embed_dim_from_reader`)
    // to auto-detect the dimension; moving `embed_dim` after `facts`/`edges`/… would
    // push it past that cap and silently break auto-detection on large snapshots.
    write!(
        writer,
        r#"{{"schema_version":{schema_version},"storage_epoch":{storage_epoch},"embed_dim":{embed_dim}"#
    )?;

    // Stream each collection row-by-row

    write!(writer, r#","facts":"#)?;
    stream_for_each(writer, |cb| FactStore::new(conn, embed_dim).for_each(cb))?;

    write!(writer, r#","edges":"#)?;
    stream_for_each(writer, |cb| EdgeStore::new(conn).for_each(cb))?;

    write!(writer, r#","summaries":"#)?;
    stream_for_each(writer, |cb| SummaryStore::new(conn, embed_dim).for_each(cb))?;

    write!(writer, r#","scopes":"#)?;
    stream_for_each(writer, |cb| ScopeStore::new(conn).for_each(cb))?;

    write!(writer, r#","events":"#)?;
    stream_for_each(writer, |cb| EventStore::new(conn, &registry).for_each(cb))?;

    write!(writer, r#","lineage":"#)?;
    stream_for_each(writer, |cb| LineageStore::new(conn).for_each(cb))?;

    // Embedding-space registry (#622): the identity left the `config` table for its own
    // table, so dump it explicitly or a restore would come up with no identity. Always
    // small (one active row today).
    let spaces: Vec<EmbeddingSpaceSnapshot> = crate::store::embedding_spaces::list_spaces(conn)?
        .into_iter()
        .map(|s| EmbeddingSpaceSnapshot {
            name: s.name,
            status: s.status.as_sql().to_string(),
            fingerprint: s.fingerprint,
        })
        .collect();
    write!(writer, r#","embedding_spaces":"#)?;
    serde_json::to_writer(&mut *writer, &spaces)?;

    // Config is always small — serialize directly.
    let config = list_config(conn)?;
    write!(writer, r#","config":"#)?;
    serde_json::to_writer(&mut *writer, &config)?;

    write!(writer, "}}")?;
    writer.flush()?;
    Ok(())
}

/// Write a JSON array by streaming entities through a `for_each`-style callback.
///
/// `iterate` must call the provided closure once per entity.  Each entity is
/// serialized to the writer immediately, then dropped — only one `T` is live
/// at a time.
fn stream_for_each<W, T, F>(writer: &mut W, iterate: F) -> Result<()>
where
    W: Write,
    T: Serialize,
    F: FnOnce(Box<dyn FnMut(T) -> Result<()> + '_>) -> Result<()>,
{
    writer.write_all(b"[")?;
    let mut first = true;
    iterate(Box::new(|item: T| {
        if !first {
            writer.write_all(b",")?;
        }
        first = false;
        serde_json::to_writer(&mut *writer, &item)?;
        Ok(())
    }))?;
    writer.write_all(b"]")?;
    Ok(())
}

/// Dump engine state to a JSON file.
///
/// Streams entities one-by-one via [`stream_snapshot`], keeping peak memory
/// constant regardless of database size.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure,
/// [`MemoryError::Io`] on filesystem failure, or
/// [`MemoryError::Serialization`] on JSON serialization failure.
pub fn dump_json(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let tmp = tmp_path(path);
    let file = File::create(&tmp)?;
    let mut buf = BufWriter::new(file);
    match stream_snapshot(conn, embed_dim, &mut buf) {
        Ok(()) => {
            fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Dump engine state to a gzip-compressed JSON file.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure,
/// [`MemoryError::Io`] on filesystem failure, or
/// [`MemoryError::Serialization`] on JSON serialization failure.
#[cfg(feature = "compress-gzip")]
pub fn dump_json_gzip(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let tmp = tmp_path(path);
    let file = File::create(&tmp)?;
    let mut encoder =
        flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::default());
    match stream_snapshot(conn, embed_dim, &mut encoder) {
        Ok(()) => {
            encoder.finish()?;
            fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Dump engine state to a zstd-compressed JSON file.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure,
/// [`MemoryError::Io`] on filesystem or zstd I/O failure, or
/// [`MemoryError::Serialization`] on JSON serialization failure.
#[cfg(feature = "compress-zstd")]
pub fn dump_json_zstd(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let tmp = tmp_path(path);
    let file = File::create(&tmp)?;
    let mut encoder = zstd::Encoder::new(BufWriter::new(file), zstd::DEFAULT_COMPRESSION_LEVEL)?;
    match stream_snapshot(conn, embed_dim, &mut encoder) {
        Ok(()) => {
            encoder.finish()?;
            fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Create an atomic `SQLite` backup via `VACUUM INTO`.
///
/// Works for both file-backed and in-memory databases (`SQLite` 3.27+).
///
/// # Trusted-path contract
///
/// `path` is treated as a caller-controlled destination. The function probes
/// the path and then writes to it, leaving an inherent check-then-act (TOCTOU)
/// window; it does **not** defend against an adversary racing the filesystem to
/// swap `path` between the probe and the write. Callers must not direct dumps at
/// a location writable by an untrusted party. The guards below turn the common
/// *mistakes* (live DB, a directory) into clear errors — they are not a defense
/// against a concurrent attacker.
///
/// # Errors
///
/// Returns [`MemoryError::Conflict`] with
/// [`ConflictError::DumpTargetIsLiveDatabase`] if `path` resolves to the live
/// database file (refusing to overwrite the source) or
/// [`ConflictError::DumpTargetIsDirectory`] if `path` is an existing directory;
/// [`MemoryError::Io`] on filesystem failures; or [`MemoryError::Internal`] if
/// the database path cannot be read, `path` contains a null byte, or
/// `VACUUM INTO` fails.
pub fn dump_sqlite(conn: &Connection, path: &Path) -> Result<()> {
    // Detect whether this is a file-backed database.
    let db_path: String = conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .map_err(|e| MemoryError::Internal(format!("cannot read database path: {e}")))?;

    let is_file_backed = !db_path.is_empty() && db_path != ":memory:";

    // Guard: refuse to dump onto the live database file (or a symlink resolving to it).
    if is_file_backed {
        let source = std::fs::canonicalize(&db_path)?;
        if let Ok(target) = std::fs::canonicalize(path)
            && target == source
        {
            return Err(MemoryError::Conflict(
                ConflictError::DumpTargetIsLiveDatabase,
            ));
        }
    }

    // Re-run support: `VACUUM INTO` refuses to write to a path that already
    // exists, so a prior dump file must be unlinked first.
    //
    // Trusted-path contract: `path` is a caller-controlled dump destination. A
    // residual check-then-act window remains between this probe and the
    // `VACUUM INTO` below (a classic TOCTOU); the engine does *not* defend
    // against an adversary racing the filesystem in that window, so callers
    // MUST NOT direct dumps at a location writable by an untrusted party.
    //
    // What this guard *does* enforce: probe with `symlink_metadata` (which does
    // not follow symlinks, unlike the previous `path.exists()`) and only ever
    // unlink a non-directory. A directory target is rejected with a clear typed
    // error instead of the opaque "Is a directory" I/O failure.
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            // `is_dir()` follows symlinks, so this refuses both a real directory
            // and a symlink that resolves to one — neither is a valid VACUUM INTO
            // target and we must never unlink a directory. A symlink to a regular
            // file (or a broken symlink) is removed as the link itself.
            if path.is_dir() {
                return Err(MemoryError::Conflict(ConflictError::DumpTargetIsDirectory));
            }
            std::fs::remove_file(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // absent: nothing to remove
        Err(e) => return Err(MemoryError::Io(e)),
    }

    let escaped = path.to_string_lossy().replace('\'', "''");
    if escaped.contains('\0') {
        return Err(MemoryError::Internal(
            "dump path contains null byte".to_string(),
        ));
    }
    let sql = format!("VACUUM INTO '{escaped}'");
    conn.execute_batch(&sql)
        .map_err(|e| MemoryError::Internal(format!("VACUUM INTO failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::inspect::types::{DumpFormat, EngineSnapshot};
    use crate::traits::EmbeddingProvider;
    use crate::types::{AddFactRequest, FactType};

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

    #[tokio::test]
    async fn json_dump_roundtrip() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "test fact".into(),
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

        // Deserialize and verify
        let content = std::fs::read_to_string(&json_path).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_str(&content).unwrap();
        assert_eq!(snapshot.facts.len(), 1);
        assert_eq!(snapshot.facts[0].content, "test fact");
        assert_eq!(snapshot.embed_dim, DIM);
        assert!(!snapshot.scopes.is_empty()); // root scope
    }

    /// Verify that streaming produces valid JSON that round-trips through
    /// `EngineSnapshot` deserialization — the format contract with restore.
    #[tokio::test]
    async fn streaming_output_matches_snapshot_schema() {
        use crate::store::schema::{init_schema, migrate, open_memory};
        use crate::store::serialize_embedding;

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        // Insert test data via raw SQL to avoid MemoryEngine dependencies.
        // Timestamps must be RFC3339 — SQLite's datetime('now') produces a
        // format that the row mappers cannot parse.
        let emb = serialize_embedding(&[0.1, 0.2, 0.3, 0.4]);
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type,
                    t_created, last_accessed, metadata, scope_id, is_pinned, importance_score)
             VALUES ('alpha', 'h1', ?1, 'semantic', ?2, ?2, '{}', 1, 0, 0.0)",
            rusqlite::params![emb, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (content, content_hash, embedding, fact_type,
                    t_created, last_accessed, metadata, scope_id, is_pinned, importance_score)
             VALUES ('beta', 'h2', ?1, 'episodic', ?2, ?2, '{}', 1, 0, 0.0)",
            rusqlite::params![emb, now],
        )
        .unwrap();

        // Stream to an in-memory buffer.
        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();

        // Must deserialize cleanly into EngineSnapshot.
        let snapshot: EngineSnapshot =
            serde_json::from_slice(&buf).expect("streaming output must be valid EngineSnapshot");

        assert_eq!(snapshot.facts.len(), 2);
        assert_eq!(snapshot.edges.len(), 0);
        assert!(!snapshot.scopes.is_empty());
        assert_eq!(snapshot.embed_dim, DIM);
        assert!(snapshot.schema_version > 0);
    }

    /// Verify empty database produces a valid (empty) snapshot.
    #[tokio::test]
    async fn streaming_empty_database() {
        use crate::store::schema::{init_schema, migrate, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();

        let snapshot: EngineSnapshot =
            serde_json::from_slice(&buf).expect("empty streaming output must be valid");

        assert_eq!(snapshot.facts.len(), 0);
        assert_eq!(snapshot.edges.len(), 0);
        assert_eq!(snapshot.summaries.len(), 0);
        assert!(!snapshot.scopes.is_empty()); // root scope
    }

    /// Stress test: 10K facts streamed to buffer. Verifies correctness at scale.
    #[tokio::test]
    async fn streaming_10k_facts_roundtrips() {
        use crate::store::schema::{init_schema, migrate, open_memory};
        use crate::store::serialize_embedding;

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let emb = serialize_embedding(&[0.1, 0.2, 0.3, 0.4]);
        let now = chrono::Utc::now().to_rfc3339();

        // Batch-insert 10K facts via raw SQL for speed.
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..10_000 {
            conn.execute(
                "INSERT INTO facts (content, content_hash, embedding, fact_type,
                        t_created, last_accessed, metadata, scope_id, is_pinned, importance_score)
                 VALUES (?1, ?2, ?3, 'semantic', ?4, ?4, '{}', 1, 0, 0.0)",
                rusqlite::params![format!("fact-{i}"), format!("h-{i}"), emb, now],
            )
            .unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();

        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();

        let snapshot: EngineSnapshot =
            serde_json::from_slice(&buf).expect("10K streaming output must be valid");
        assert_eq!(snapshot.facts.len(), 10_000);
        assert_eq!(snapshot.embed_dim, DIM);
    }

    #[tokio::test]
    async fn sqlite_dump_from_in_memory() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "in-memory fact".into(),
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
        let db_path = dir.path().join("dump.db");
        engine
            .dump_state(&DumpFormat::Sqlite(db_path.clone()))
            .await
            .unwrap();

        // Verify the dump is a valid SQLite database with our data.
        let dump_conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count: i64 = dump_conn
            .query_row("SELECT count(*) FROM facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[cfg(feature = "compress-gzip")]
    #[tokio::test]
    async fn gzip_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "gzip test".into(),
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
        let path = dir.path().join("dump.json.gz");
        engine
            .dump_state(&DumpFormat::JsonGzip(path.clone()))
            .await
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 2, "file too small");
        assert_eq!(bytes[0], 0x1f, "gzip magic byte 0");
        assert_eq!(bytes[1], 0x8b, "gzip magic byte 1");
    }

    #[cfg(feature = "compress-zstd")]
    #[tokio::test]
    async fn zstd_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "zstd test".into(),
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
        let path = dir.path().join("dump.json.zst");
        engine
            .dump_state(&DumpFormat::JsonZstd(path.clone()))
            .await
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 4, "file too small");
        assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic bytes");
    }

    #[tokio::test]
    async fn snapshot_empty_engine_dump() {
        use crate::store::schema::{init_schema, migrate, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn, None).unwrap();

        let mut buf = Vec::new();
        stream_snapshot(&conn, DIM, &mut buf).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_slice(&buf).unwrap();

        insta::assert_yaml_snapshot!(snapshot, {
            ".config" => insta::sorted_redaction(),
        });
    }

    #[tokio::test]
    async fn snapshot_populated_engine_dump() {
        let engine = MemoryEngine::builder(DIM).build().unwrap();
        engine
            .add_fact(
                &AddFactRequest {
                    content: "snapshot fact".into(),
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

        let content = std::fs::read_to_string(&json_path).unwrap();
        let snapshot: EngineSnapshot = serde_json::from_str(&content).unwrap();

        insta::assert_yaml_snapshot!(snapshot, {
            ".facts[].t_created" => "[timestamp]",
            ".facts[].last_accessed" => "[timestamp]",
            ".facts[].embedding" => "[embedding]",
            ".facts[].content_hash" => "[hash]",
            ".events" => "[]",
            ".config" => "{}",
        });
    }

    /// L6 hardening: a mistaken or hostile dump target that is a directory must
    /// be refused with a clear typed error, not the opaque `remove_file`-on-a-
    /// directory I/O failure the bare `path.exists()` check produced before.
    #[tokio::test]
    async fn dump_sqlite_refuses_directory_target() {
        use crate::store::schema::{init_schema, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = dump_sqlite(&conn, dir.path());

        assert!(
            matches!(
                result,
                Err(MemoryError::Conflict(ConflictError::DumpTargetIsDirectory))
            ),
            "dumping onto a directory must be refused with DumpTargetIsDirectory, got {result:?}"
        );
    }

    /// A symlink resolving to a directory must also be refused — `is_dir()`
    /// follows the link, so the guard is not bypassed by indirection.
    #[cfg(unix)]
    #[tokio::test]
    async fn dump_sqlite_refuses_symlink_to_directory() {
        use crate::store::schema::{init_schema, open_memory};

        let conn = open_memory().unwrap();
        init_schema(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real_dir");
        std::fs::create_dir(&real_dir).unwrap();
        let link = dir.path().join("link_to_dir");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let result = dump_sqlite(&conn, &link);
        assert!(
            matches!(
                result,
                Err(MemoryError::Conflict(ConflictError::DumpTargetIsDirectory))
            ),
            "a symlink to a directory must be refused, got {result:?}"
        );
    }
}
