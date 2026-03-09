use std::cell::RefCell;
use std::path::PathBuf;

use chrono::Utc;
use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::graph::MemoryGraph;
use crate::scope::ScopeTree;
use crate::search::hybrid::{hybrid_search, SearchQuery, SearchResult};
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::schema::{
    get_config, init_schema, migrate, open_connection, open_memory, set_config,
};
use crate::store::scopes::ScopeStore;
use crate::store::summaries::SummaryStore;
use crate::traits::{
    ConflictArbiter, ConflictResolution, ConsolidationConfig, ConsolidationStats,
    EmbeddingProvider, ForgetPolicy, PruneStats, SummaryGenerator,
};
use crate::types::{AddFactOptions, FactType, NewEvent, NewFact};

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
    graph: MemoryGraph,
    scope_tree: RefCell<ScopeTree>,
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
        migrate(&conn)?;
        Self::validate_or_set_embed_dim(&conn, config.embed_dim)?;
        let graph = MemoryGraph::load_from_db(&conn)?;
        let scope_tree = ScopeTree::load(&conn)?;
        Ok(Self {
            conn,
            embed_dim: config.embed_dim,
            graph,
            scope_tree: RefCell::new(scope_tree),
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
        migrate(&conn)?;
        Self::validate_or_set_embed_dim(&conn, embed_dim)?;
        let graph = MemoryGraph::load_from_db(&conn)?;
        let scope_tree = ScopeTree::load(&conn)?;
        Ok(Self {
            conn,
            embed_dim,
            graph,
            scope_tree: RefCell::new(scope_tree),
        })
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
        scope: Option<&str>,
        opts: Option<&AddFactOptions>,
    ) -> Result<i64> {
        let embedding = embedder.embed(content)?;
        let now = Utc::now();
        let opts = opts.cloned().unwrap_or_default();

        let scope_id = match scope {
            Some(path) => {
                let scope_store = ScopeStore::new(&self.conn);
                let id = scope_store.ensure_path(path)?;
                let node = scope_store.get(id)?;
                self.scope_tree.borrow_mut().insert(node);
                id
            }
            None => 1, // root scope
        };

        let new_fact = NewFact {
            content: content.into(),
            content_hash: String::new(), // FactStore::insert computes this via blake3
            embedding,
            fact_type,
            t_created: now,
            t_expired: None,
            t_valid: opts.t_valid,
            t_invalid: opts.t_invalid,
            source_event_id,
            scope_id,
            importance: opts.importance.unwrap_or(0.5),
            access_count: 0,
            last_accessed: now,
            metadata: opts.metadata.unwrap_or_else(|| serde_json::json!({})),
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

    // --- Phase 2: fully implemented ---

    /// Run three-pass consolidation: local dedup, cluster fusion, global integration.
    ///
    /// # Errors
    ///
    /// Propagates errors from any consolidation pass or the `SummaryGenerator`.
    pub fn consolidate(
        &mut self,
        generator: &dyn SummaryGenerator,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        let stats =
            crate::consolidation::consolidate(&self.conn, generator, self.embed_dim, config)?;

        // Rebuild graph: dedup may have expired facts and their edges
        if stats.duplicates_removed > 0 {
            self.graph = MemoryGraph::load_from_db(&self.conn)?;
        }

        Ok(stats)
    }

    /// Prune stale facts using Ebbinghaus decay and graph-aware importance scoring.
    ///
    /// Facts with computed importance below `policy.min_importance` get soft-deleted.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the policy is invalid.
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn forget(&mut self, policy: &ForgetPolicy) -> Result<PruneStats> {
        crate::forgetting::prune(
            &self.conn,
            &mut self.graph,
            policy,
            self.embed_dim,
            Utc::now(),
        )
    }

    /// Resolve a conflict between an existing fact and a candidate new fact.
    ///
    /// Delegates the decision to the consumer-provided [`ConflictArbiter`].
    /// Mutations happen in a single transaction; graph is updated only after commit.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the old fact doesn't exist.
    /// Propagates errors from the arbiter or database operations.
    pub fn resolve_conflict(
        &mut self,
        arbiter: &dyn ConflictArbiter,
        old_id: i64,
        new_fact: &NewFact,
    ) -> Result<ConflictResolution> {
        crate::conflict::resolve_conflict(
            &self.conn,
            &mut self.graph,
            arbiter,
            old_id,
            new_fact,
            self.embed_dim,
            Utc::now(),
        )
    }

    /// Access the in-memory graph (read-only).
    #[must_use]
    pub const fn graph(&self) -> &MemoryGraph {
        &self.graph
    }

    /// Get a `SummaryStore` borrowing this engine's connection.
    #[must_use]
    pub const fn summary_store(&self) -> SummaryStore<'_> {
        SummaryStore::new(&self.conn, self.embed_dim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::hybrid::SearchMode;
    use crate::traits::{ConsolidationConfig, CrudDecision, ForgetPolicy};
    use crate::types::{EventType, Fact, FactType};

    const DIM: usize = 4;

    struct MockEmbedder {
        dim: usize,
    }

    impl EmbeddingProvider for MockEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.5; self.dim])
        }
    }

    struct MockGen;
    impl SummaryGenerator for MockGen {
        fn summarize(&self, facts: &[Fact]) -> Result<String> {
            Ok(facts
                .iter()
                .map(|f| f.content.as_str())
                .collect::<Vec<_>>()
                .join("; "))
        }
        fn embed(&self, _: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1; DIM])
        }
    }

    struct FixedArbiter {
        decision: CrudDecision,
    }
    impl ConflictArbiter for FixedArbiter {
        fn arbitrate(&self, _: &Fact, _: &Fact) -> Result<CrudDecision> {
            Ok(self.decision.clone())
        }
    }

    fn make_new_fact(content: &str, embedding: Vec<f32>) -> NewFact {
        let now = Utc::now();
        NewFact {
            content: content.into(),
            content_hash: blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string(),
            embedding,
            fact_type: FactType::Semantic,
            t_created: now,
            t_expired: None,
            t_valid: None,
            t_invalid: None,
            source_event_id: None,
            scope_id: 1,
            importance: 0.5,
            access_count: 0,
            last_accessed: now,
            metadata: serde_json::json!({}),
        }
    }

    // --- Phase 1 tests (unchanged) ---

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
            scope_id: 1,
        };
        let id = engine.ingest(&event).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn add_fact_returns_fact_id() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let id = engine
            .add_fact(
                "Rust is fast",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
            )
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
                None,
                None,
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

    // --- Phase 2 tests ---

    #[test]
    fn graph_starts_empty() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        assert_eq!(engine.graph().node_count(), 0);
        assert_eq!(engine.graph().edge_count(), 0);
    }

    #[test]
    fn consolidate_deduplicates_similar_facts() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let fs = engine.fact_store();
        // Two near-identical embeddings
        fs.insert(&make_new_fact("fact alpha", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        fs.insert(&make_new_fact(
            "fact alpha copy",
            vec![0.99, 0.01, 0.0, 0.0],
        ))
        .unwrap();

        let config = ConsolidationConfig {
            dedup_threshold: 0.90,
            min_cluster_size: 10, // high threshold so no clusters form
        };
        let stats = engine.consolidate(&MockGen, &config).unwrap();
        assert_eq!(stats.duplicates_removed, 1);

        let active = engine.fact_store().list_active().unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn consolidate_is_idempotent() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let fs = engine.fact_store();
        fs.insert(&make_new_fact("unique A", vec![1.0, 0.0, 0.0, 0.0]))
            .unwrap();
        fs.insert(&make_new_fact("unique B", vec![0.0, 1.0, 0.0, 0.0]))
            .unwrap();

        let config = ConsolidationConfig {
            dedup_threshold: 0.92,
            min_cluster_size: 10,
        };

        let _stats1 = engine.consolidate(&MockGen, &config).unwrap();
        let stats2 = engine.consolidate(&MockGen, &config).unwrap();

        // Second run should find 0 new duplicates
        assert_eq!(stats2.duplicates_removed, 0);
        // Both facts still active
        assert_eq!(engine.fact_store().list_active().unwrap().len(), 2);
    }

    #[test]
    fn forget_prunes_stale_facts() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();

        // Insert a fact with very low importance
        let now = Utc::now();
        let old_time = now - chrono::Duration::days(200);
        engine
            .fact_store()
            .insert(&NewFact {
                content: "ancient fact".into(),
                content_hash: "h_ancient".into(),
                embedding: vec![0.1; DIM],
                fact_type: FactType::Episodic,
                t_created: old_time,
                t_expired: None,
                t_valid: None,
                t_invalid: None,
                source_event_id: None,
                scope_id: 1,
                importance: 0.01,
                access_count: 0,
                last_accessed: old_time,
                metadata: serde_json::json!({}),
            })
            .unwrap();

        let policy = ForgetPolicy {
            min_importance: 0.3,
            ..ForgetPolicy::default()
        };
        let stats = engine.forget(&policy).unwrap();
        assert_eq!(stats.facts_expired, 1);
        assert_eq!(stats.facts_evaluated, 1);
    }

    #[test]
    fn forget_rejects_invalid_policy() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let policy = ForgetPolicy {
            half_life_days: 0.0, // invalid
            ..ForgetPolicy::default()
        };
        assert!(engine.forget(&policy).is_err());
    }

    #[test]
    fn resolve_conflict_update_creates_edge() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let old_id = engine
            .fact_store()
            .insert(&make_new_fact("outdated", vec![0.5; DIM]))
            .unwrap();

        let arbiter = FixedArbiter {
            decision: CrudDecision::Update,
        };
        let result = engine
            .resolve_conflict(&arbiter, old_id, &make_new_fact("updated", vec![0.5; DIM]))
            .unwrap();

        assert_eq!(result.decision, CrudDecision::Update);
        assert!(result.new_fact_id.is_some());

        // Old fact should be expired
        let old = engine.fact_store().get(old_id).unwrap();
        assert!(old.t_expired.is_some());

        // Graph should have the new edge
        let new_id = result.new_fact_id.unwrap();
        assert!(engine.graph().has_node(new_id));
        assert_eq!(engine.graph().neighbors(new_id), vec![old_id]);
    }

    #[test]
    fn resolve_conflict_noop_no_changes() {
        let mut engine = MemoryEngine::open_memory(DIM).unwrap();
        let old_id = engine
            .fact_store()
            .insert(&make_new_fact("existing", vec![0.5; DIM]))
            .unwrap();

        let arbiter = FixedArbiter {
            decision: CrudDecision::Noop,
        };
        let result = engine
            .resolve_conflict(
                &arbiter,
                old_id,
                &make_new_fact("candidate", vec![0.5; DIM]),
            )
            .unwrap();

        assert_eq!(result.decision, CrudDecision::Noop);
        assert!(result.new_fact_id.is_none());

        // Old fact unchanged
        let old = engine.fact_store().get(old_id).unwrap();
        assert!(old.t_expired.is_none());
    }

    #[test]
    fn graph_loads_on_reopen() {
        let dir = std::env::temp_dir().join("memory_engine_test_graph_reload");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");

        let config = EngineConfig {
            path: db_path,
            embed_dim: DIM,
        };

        // First session: add facts and create an edge via conflict resolution
        {
            let mut engine = MemoryEngine::open(&config).unwrap();
            let old_id = engine
                .fact_store()
                .insert(&make_new_fact("original", vec![0.5; DIM]))
                .unwrap();
            let arbiter = FixedArbiter {
                decision: CrudDecision::Update,
            };
            engine
                .resolve_conflict(
                    &arbiter,
                    old_id,
                    &make_new_fact("replacement", vec![0.5; DIM]),
                )
                .unwrap();
            assert_eq!(engine.graph().edge_count(), 1);
        }

        // Second session: graph should be restored from DB
        {
            let engine = MemoryEngine::open(&config).unwrap();
            assert_eq!(engine.graph().edge_count(), 1);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_store_accessor_works() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let store = engine.summary_store();
        let summaries = store
            .list_by_level(&crate::types::ConsolidationLevel::Global)
            .unwrap();
        assert!(summaries.is_empty());
    }

    // --- Phase 3 / T2: AddFactOptions ---

    #[test]
    fn add_fact_with_custom_importance() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let opts = AddFactOptions {
            importance: Some(0.9),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                "important fact",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&opts),
            )
            .unwrap();
        let fact = engine.fact_store().get(id).unwrap();
        assert!((fact.importance - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn add_fact_with_temporal_bounds() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let now = Utc::now();
        let opts = AddFactOptions {
            t_valid: Some(now - chrono::Duration::hours(1)),
            t_invalid: Some(now + chrono::Duration::hours(1)),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                "temporal fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                Some(&opts),
            )
            .unwrap();
        let fact = engine.fact_store().get(id).unwrap();
        assert!(fact.t_valid.is_some());
        assert!(fact.t_invalid.is_some());
    }

    #[test]
    fn add_fact_with_scope_path() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let id = engine
            .add_fact(
                "scoped fact",
                FactType::Semantic,
                None,
                &embedder,
                Some("user:test/project:demo"),
                None,
            )
            .unwrap();
        let fact = engine.fact_store().get(id).unwrap();
        assert_ne!(fact.scope_id, 1); // not root
    }

    #[test]
    fn add_fact_none_opts_uses_defaults() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let id = engine
            .add_fact(
                "default fact",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
            )
            .unwrap();
        let fact = engine.fact_store().get(id).unwrap();
        assert!((fact.importance - 0.5).abs() < f64::EPSILON);
        assert!(fact.t_valid.is_none());
    }
}
