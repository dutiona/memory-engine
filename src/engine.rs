use std::path::PathBuf;

use chrono::Utc;
use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::search::hybrid::{hybrid_search, SearchQuery, SearchResult};
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::schema::{get_config, init_schema, open_connection, open_memory, set_config};
use crate::traits::{
    ConflictArbiter, ConflictResolution, ConsolidationConfig, ConsolidationStats,
    EmbeddingProvider, ForgetPolicy, PruneStats, SummaryGenerator,
};
use crate::types::{FactType, NewEvent, NewFact};

/// Configuration for opening a [`MemoryEngine`] backed by a file.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub path: PathBuf,
    pub embed_dim: usize,
}

/// Facade over all memory primitives: ingest, query, consolidate, forget, resolve.
///
/// `MemoryEngine` is `!Send` and `!Sync` because `rusqlite::Connection` is not
/// thread-safe. Consumers must wrap in a `Mutex` or use an actor pattern.
pub struct MemoryEngine {
    conn: Connection,
    embed_dim: usize,
}

impl std::fmt::Debug for MemoryEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEngine")
            .field("embed_dim", &self.embed_dim)
            .finish_non_exhaustive()
    }
}

impl MemoryEngine {
    /// Open or create a memory engine backed by a `SQLite` file.
    ///
    /// On first open, writes `embed_dim` to the config table.
    /// On subsequent opens, validates the stored `embed_dim` matches.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration` if the stored `embed_dim` doesn't match
    /// the requested value.
    pub fn open(config: &EngineConfig) -> Result<Self> {
        let path_str = config.path.to_string_lossy();
        let conn = open_connection(&path_str)?;
        init_schema(&conn)?;
        Self::validate_or_set_embed_dim(&conn, config.embed_dim)?;
        Ok(Self {
            conn,
            embed_dim: config.embed_dim,
        })
    }

    /// Open an in-memory engine for testing.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if the connection or schema setup fails.
    pub fn open_memory(embed_dim: usize) -> Result<Self> {
        let conn = open_memory()?;
        init_schema(&conn)?;
        Self::validate_or_set_embed_dim(&conn, embed_dim)?;
        Ok(Self { conn, embed_dim })
    }

    fn validate_or_set_embed_dim(conn: &Connection, embed_dim: usize) -> Result<()> {
        if let Some(stored) = get_config(conn, "embed_dim")? {
            let stored_dim: usize = stored.parse().map_err(|_| {
                MemoryError::Migration(format!("invalid stored embed_dim: {stored}"))
            })?;
            if stored_dim != embed_dim {
                return Err(MemoryError::Migration(format!(
                    "embed_dim mismatch: stored {stored_dim} vs requested {embed_dim}"
                )));
            }
        } else {
            set_config(conn, "embed_dim", &embed_dim.to_string())?;
        }
        Ok(())
    }

    // --- Phase 1: fully implemented ---

    /// Append an event to the event log. Returns the assigned event id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on insert failure.
    pub fn ingest(&self, event: &NewEvent) -> Result<i64> {
        self.event_store().insert(event)
    }

    /// Add a fact: compute embedding via `embedder`, delegate to `FactStore`
    /// (which computes blake3 content hash). Returns the assigned fact id.
    ///
    /// # Errors
    ///
    /// Returns errors from embedding computation, dimension validation, or DB insert.
    pub fn add_fact(
        &self,
        content: &str,
        fact_type: FactType,
        source_event_id: Option<i64>,
        embedder: &dyn EmbeddingProvider,
    ) -> Result<i64> {
        let embedding = embedder.embed(content)?;
        let now = Utc::now();

        let new_fact = NewFact {
            content: content.into(),
            content_hash: String::new(), // FactStore::insert computes this via blake3
            embedding,
            fact_type,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id,
            importance: 0.5,
            access_count: 0,
            last_accessed: now,
            metadata: serde_json::json!({}),
        };

        self.fact_store().insert(&new_fact)
    }

    /// Query facts using hybrid search (FTS5 + vector + RRF).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        hybrid_search(&self.conn, query, self.embed_dim)
    }

    /// Get a `FactStore` borrowing this engine's connection.
    #[must_use]
    pub const fn fact_store(&self) -> FactStore<'_> {
        FactStore::new(&self.conn, self.embed_dim)
    }

    /// Get an `EventStore` borrowing this engine's connection.
    #[must_use]
    pub const fn event_store(&self) -> EventStore<'_> {
        EventStore::new(&self.conn)
    }

    /// Read a config value by key.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        get_config(&self.conn, key)
    }

    /// Write a config value (upsert).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on write failure.
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        set_config(&self.conn, key, value)
    }

    // --- Phase 2 stubs ---

    /// Consolidate memories (Phase 2).
    ///
    /// # Errors
    ///
    /// Always returns `MemoryError::NotImplemented` in Phase 1.
    pub fn consolidate(
        &mut self,
        _generator: &dyn SummaryGenerator,
        _config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        Err(MemoryError::NotImplemented(
            "consolidation requires Phase 2 (Tasks 9-12)".into(),
        ))
    }

    /// Forget/prune stale facts (Phase 2).
    ///
    /// # Errors
    ///
    /// Always returns `MemoryError::NotImplemented` in Phase 1.
    pub fn forget(&mut self, _policy: &ForgetPolicy) -> Result<PruneStats> {
        Err(MemoryError::NotImplemented(
            "forgetting requires Phase 2 (Tasks 9, 11)".into(),
        ))
    }

    /// Resolve a conflict between facts (Phase 2).
    ///
    /// # Errors
    ///
    /// Always returns `MemoryError::NotImplemented` in Phase 1.
    pub fn resolve_conflict(
        &mut self,
        _arbiter: &dyn ConflictArbiter,
        _old_id: i64,
        _new_fact: NewFact,
    ) -> Result<ConflictResolution> {
        Err(MemoryError::NotImplemented(
            "conflict resolution requires Phase 2 (Tasks 9, 13)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::hybrid::SearchMode;
    use crate::traits::{ConsolidationConfig, ForgetPolicy};
    use crate::types::{EventType, FactType};

    const DIM: usize = 4;

    struct MockEmbedder {
        dim: usize,
    }

    impl EmbeddingProvider for MockEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; self.dim])
        }
    }

    struct DummyGen;
    impl SummaryGenerator for DummyGen {
        fn summarize(&self, _: &[crate::types::Fact]) -> Result<String> {
            Ok(String::new())
        }
        fn embed(&self, _: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }
    }

    struct DummyArbiter;
    impl crate::traits::ConflictArbiter for DummyArbiter {
        fn arbitrate(
            &self,
            _: &crate::types::Fact,
            _: &crate::types::Fact,
        ) -> Result<crate::traits::CrudDecision> {
            Ok(crate::traits::CrudDecision::Noop)
        }
    }

    #[test]
    fn open_memory_succeeds() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        assert_eq!(engine.embed_dim, DIM);
    }

    #[test]
    fn ingest_returns_event_id() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"msg": "hello"}),
            source: "test".into(),
            session_id: None,
        };
        let id = engine.ingest(&event).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn add_fact_returns_fact_id() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let id = engine
            .add_fact("Rust is fast", FactType::Semantic, None, &embedder)
            .unwrap();
        assert!(id > 0);
    }

    #[test]
    fn query_returns_results_after_adding_facts() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "Rust is a systems programming language",
                FactType::Semantic,
                None,
                &embedder,
            )
            .unwrap();

        let query = SearchQuery {
            text: Some("Rust".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            valid_at: None,
            fact_type: None,
        };
        let results = engine.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].fact.content.contains("Rust"));
    }

    #[test]
    fn consolidate_returns_not_implemented() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let config = ConsolidationConfig {
            dedup_threshold: 0.92,
            min_cluster_size: 3,
        };
        let err = engine.consolidate(&DummyGen, &config).unwrap_err();
        assert!(matches!(err, MemoryError::NotImplemented(_)));
    }

    #[test]
    fn forget_returns_not_implemented() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let policy = ForgetPolicy {
            min_importance: 0.1,
        };
        let err = engine.forget(&policy).unwrap_err();
        assert!(matches!(err, MemoryError::NotImplemented(_)));
    }

    #[test]
    fn resolve_conflict_returns_not_implemented() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let new_fact = NewFact {
            content: "test".into(),
            content_hash: "abc".into(),
            embedding: vec![0.1; DIM],
            fact_type: FactType::Episodic,
            t_created: Utc::now(),
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            importance: 0.5,
            access_count: 0,
            last_accessed: Utc::now(),
            metadata: serde_json::json!({}),
        };
        let err = engine
            .resolve_conflict(&DummyArbiter, 1, new_fact)
            .unwrap_err();
        assert!(matches!(err, MemoryError::NotImplemented(_)));
    }

    #[test]
    fn embed_dim_validation_rejects_mismatch() {
        let dir = std::env::temp_dir().join("memory_engine_test_dim_validation");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");

        let config_768 = EngineConfig {
            path: db_path.clone(),
            embed_dim: 768,
        };
        let config_384 = EngineConfig {
            path: db_path,
            embed_dim: 384,
        };

        // First open with dim=768
        {
            let _engine = MemoryEngine::open(&config_768).unwrap();
        }

        // Second open with dim=384 should fail
        let err = MemoryEngine::open(&config_384).unwrap_err();
        assert!(matches!(err, MemoryError::Migration(_)));
        assert!(err.to_string().contains("mismatch"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_set_config() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        assert!(engine.get_config("custom_key").unwrap().is_none());
        engine.set_config("custom_key", "custom_value").unwrap();
        assert_eq!(
            engine.get_config("custom_key").unwrap(),
            Some("custom_value".into())
        );
    }
}
