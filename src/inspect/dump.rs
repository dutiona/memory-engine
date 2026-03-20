use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::store::edges::EdgeStore;
use crate::store::events::{EventFilter, EventStore};
use crate::store::facts::FactStore;
use crate::store::schema::{get_config, list_config};
use crate::store::scopes::ScopeStore;
use crate::store::summaries::SummaryStore;
use crate::store::UpcasterRegistry;

use super::types::EngineSnapshot;

/// Build an [`EngineSnapshot`] from the current database state.
fn build_snapshot(conn: &Connection, embed_dim: usize) -> Result<EngineSnapshot> {
    let registry = UpcasterRegistry::new(); // raw events, no upcasting

    let facts = FactStore::new(conn, embed_dim).list_all()?;
    let edges = EdgeStore::new(conn).list_all()?;
    let summaries = SummaryStore::new(conn, embed_dim).list_all()?;
    let scopes = ScopeStore::new(conn).list_all()?;
    let events = EventStore::new(conn, &registry).list(&EventFilter::default())?;
    let config = list_config(conn)?;

    let schema_version: u32 = get_config(conn, "schema_version")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let storage_epoch: u16 = get_config(conn, "storage_epoch")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    Ok(EngineSnapshot {
        schema_version,
        storage_epoch,
        embed_dim,
        facts,
        edges,
        summaries,
        scopes,
        events,
        config,
    })
}

/// Serialize an [`EngineSnapshot`] to a writer.
fn write_snapshot(writer: impl Write, snapshot: &EngineSnapshot) -> Result<()> {
    serde_json::to_writer(writer, snapshot)?;
    Ok(())
}

/// Dump engine state to a JSON file.
///
/// Serializes all facts, edges, summaries, scopes, events, and config
/// via `serde_json::to_writer`. Works for both file-backed and in-memory engines.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure,
/// [`MemoryError::Io`] on filesystem failure, or
/// [`MemoryError::Serialization`] on JSON serialization failure.
pub fn dump_json(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let snapshot = build_snapshot(conn, embed_dim)?;
    let file = File::create(path)?;
    write_snapshot(BufWriter::new(file), &snapshot)
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
    let snapshot = build_snapshot(conn, embed_dim)?;
    let file = File::create(path)?;
    let encoder =
        flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::default());
    write_snapshot(encoder, &snapshot)
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
    let snapshot = build_snapshot(conn, embed_dim)?;
    let file = File::create(path)?;
    let mut encoder = zstd::Encoder::new(BufWriter::new(file), zstd::DEFAULT_COMPRESSION_LEVEL)?;
    serde_json::to_writer(&mut encoder, &snapshot)?;
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
    let sql = format!("VACUUM INTO '{escaped}'");
    conn.execute_batch(&sql)
        .map_err(|e| MemoryError::Internal(format!("VACUUM INTO failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemoryEngine;
    use crate::inspect::types::DumpFormat;
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
