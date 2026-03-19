use std::fs::File;
use std::io::BufWriter;
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

/// Dump engine state to a JSON file.
///
/// Serializes all facts, edges, summaries, scopes, events, and config
/// via `serde_json::to_writer`. Works for both file-backed and in-memory engines.
///
/// # Errors
///
/// Returns [`MemoryError::Database`] on SQL failure or
/// [`MemoryError::Internal`] on I/O or serialization failure.
pub fn dump_json(conn: &Connection, embed_dim: usize, path: &Path) -> Result<()> {
    let registry = UpcasterRegistry::new(); // raw events, no upcasting

    // Collect all data
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

    let snapshot = EngineSnapshot {
        schema_version,
        storage_epoch,
        embed_dim,
        facts,
        edges,
        summaries,
        scopes,
        events,
        config,
    };

    let file = File::create(path).map_err(|e| MemoryError::Internal(e.to_string()))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer(writer, &snapshot)?;

    Ok(())
}

/// Create an atomic `SQLite` backup via `VACUUM INTO`.
///
/// # Errors
///
/// Returns [`MemoryError::Internal`] if the database is in-memory or
/// the `VACUUM INTO` statement fails.
pub fn dump_sqlite(conn: &Connection, path: &Path) -> Result<()> {
    // Check if this is an in-memory database
    let db_path: String = conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .map_err(|e| MemoryError::Internal(format!("cannot read database path: {e}")))?;

    if db_path.is_empty() || db_path == ":memory:" {
        return Err(MemoryError::Internal(
            "cannot create SQLite backup from in-memory database; use DumpFormat::Json instead"
                .to_string(),
        ));
    }

    // Remove existing file to avoid VACUUM INTO failure
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| MemoryError::Internal(format!("cannot remove existing dump file: {e}")))?;
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
        assert!(snapshot.scopes.len() >= 1); // root scope
    }

    #[test]
    fn sqlite_dump_fails_for_in_memory() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dump.db");
        let result = engine.dump_state(&DumpFormat::Sqlite(db_path));
        assert!(result.is_err());
    }
}
