use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use crate::error::{MemoryError, Result};
use crate::store::UpcasterRegistry;
use crate::store::edges::EdgeStore;
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::schema::{get_config, list_config};
use crate::store::scopes::ScopeStore;
use crate::store::summaries::SummaryStore;

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

    // Scalar header fields
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
    let file = File::create(path)?;
    let mut buf = BufWriter::new(file);
    stream_snapshot(conn, embed_dim, &mut buf)
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
    let file = File::create(path)?;
    let mut encoder =
        flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::default());
    stream_snapshot(conn, embed_dim, &mut encoder)?;
    encoder.finish()?;
    Ok(())
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
    let file = File::create(path)?;
    let mut encoder = zstd::Encoder::new(BufWriter::new(file), zstd::DEFAULT_COMPRESSION_LEVEL)?;
    stream_snapshot(conn, embed_dim, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}

/// Create an atomic `SQLite` backup via `VACUUM INTO`.
///
/// Works for both file-backed and in-memory databases (`SQLite` 3.27+).
///
/// # Errors
///
/// Returns [`MemoryError::Io`] on filesystem failures or
/// [`MemoryError::Internal`] if `VACUUM INTO` fails.
pub fn dump_sqlite(conn: &Connection, path: &Path) -> Result<()> {
    // Detect whether this is a file-backed database.
    let db_path: String = conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .map_err(|e| MemoryError::Internal(format!("cannot read database path: {e}")))?;

    let is_file_backed = !db_path.is_empty() && db_path != ":memory:";

    // Guard: refuse to dump onto the live database file (or a symlink resolving to it).
    if is_file_backed {
        let source = std::fs::canonicalize(&db_path)?;
        if let Ok(target) = std::fs::canonicalize(path) {
            if target == source {
                return Err(MemoryError::Conflict(
                    "dump target resolves to the live database file".to_string(),
                ));
            }
        }
    }

    // Remove existing file to avoid VACUUM INTO failure on re-run.
    if path.exists() {
        std::fs::remove_file(path)?;
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
    use crate::types::FactType;

    const DIM: usize = 4;

    struct FakeEmbed;
    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, _text: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
    }

    #[test]
    fn json_dump_roundtrip() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        engine
            .add_fact(
                "test fact",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                None,
                None,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("dump.json");
        engine
            .dump_state(&DumpFormat::Json(json_path.clone()))
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
    #[test]
    fn streaming_output_matches_snapshot_schema() {
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
    #[test]
    fn streaming_empty_database() {
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

    #[test]
    fn sqlite_dump_from_in_memory() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        engine
            .add_fact(
                "in-memory fact",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                None,
                None,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dump.db");
        engine
            .dump_state(&DumpFormat::Sqlite(db_path.clone()))
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
    #[test]
    fn gzip_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        engine
            .add_fact(
                "gzip test",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                None,
                None,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json.gz");
        engine
            .dump_state(&DumpFormat::JsonGzip(path.clone()))
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 2, "file too small");
        assert_eq!(bytes[0], 0x1f, "gzip magic byte 0");
        assert_eq!(bytes[1], 0x8b, "gzip magic byte 1");
    }

    #[cfg(feature = "compress-zstd")]
    #[test]
    fn zstd_dump_has_correct_magic_bytes() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        engine
            .add_fact(
                "zstd test",
                FactType::Semantic,
                None,
                &FakeEmbed,
                None,
                None,
                None,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.json.zst");
        engine
            .dump_state(&DumpFormat::JsonZstd(path.clone()))
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 4, "file too small");
        assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic bytes");
    }
}
