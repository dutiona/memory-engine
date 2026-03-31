use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use parking_lot::{MutexGuard, RwLock};
use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::graph::MemoryGraph;
use crate::pool::ConnectionPool;
use crate::resume::context::{ResumeConfig, ResumeContext};
use crate::scope::ScopeTree;
use crate::search::hybrid::{
    hybrid_search, MatchType, QueryDiagnostics, QueryResponse, SearchMode, SearchQuery,
    SearchResult,
};
use crate::search::query::MemoryQuery;
use crate::search::strategy::{BruteForce, SearchConfig, VectorSearchStrategy};
use crate::store::events::EventStore;
use crate::store::facts::FactStore;
use crate::store::schema::{get_config, set_config};
use crate::store::scopes::ScopeStore;
use crate::store::summaries::SummaryStore;
use crate::store::upcaster::UpcasterRegistry;
use crate::traits::{
    ConflictArbiter, ConflictResolution, ConsolidationConfig, ConsolidationStats,
    EmbeddingProvider, ForgetPolicy, PersistenceClassifier, PruneStats, Reranker, SummaryGenerator,
};
use crate::types::{
    AddFactOptions, BatchFactEntry, ConsolidationLevel, Fact, FactType, NewEdge, NewEvent, NewFact,
};

/// Configuration for opening a [`MemoryEngine`] backed by a file.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub path: PathBuf,
    pub embed_dim: usize,
    /// Number of read connections in the pool (default: 4).
    pub read_pool_size: usize,
    /// Optional search configuration for ANN strategy dispatch.
    pub search_config: Option<SearchConfig>,
    /// Optional directory for WAL-safe pre-migration backups.
    /// When set, `VACUUM INTO` creates a backup before running migrations.
    pub backup_dir: Option<PathBuf>,
    /// Upcaster registry for event payload versioning.
    /// Defaults to empty (all event types at revision 1).
    pub upcaster_registry: UpcasterRegistry,
    /// Open in read-only mode: skip init/migration, reject writes.
    pub read_only: bool,
}

impl EngineConfig {
    /// Create a config with default read pool size.
    #[must_use]
    pub fn new(path: PathBuf, embed_dim: usize) -> Self {
        Self {
            path,
            embed_dim,
            read_pool_size: 4,
            search_config: None,
            backup_dir: None,
            upcaster_registry: UpcasterRegistry::new(),
            read_only: false,
        }
    }
}

/// Facade over all memory primitives: ingest, query, consolidate, forget, resolve.
///
/// `MemoryEngine` is `Send + Sync`. Thread safety is provided by:
/// - `ConnectionPool` — bounded read pool + exclusive write connection via `parking_lot::Mutex`
/// - `RwLock<MemoryGraph>` — concurrent readers, exclusive writer
/// - `RwLock<ScopeTree>` — concurrent readers, exclusive writer
///
/// All public methods take `&self`. Consumers can share via `Arc<MemoryEngine>`.
pub struct MemoryEngine {
    pub(crate) pool: ConnectionPool,
    embed_dim: usize,
    graph: RwLock<MemoryGraph>,
    scope_tree: RwLock<ScopeTree>,
    vector_strategy: Box<dyn VectorSearchStrategy>,
    reranker: Option<Box<dyn Reranker>>,
    #[cfg(feature = "ann")]
    hnsw_strategy: Option<crate::search::ann::HnswStrategy>,
    #[cfg_attr(not(feature = "ann"), allow(dead_code))]
    search_config: Option<SearchConfig>,
    upcaster_registry: UpcasterRegistry,
}

impl std::fmt::Debug for MemoryEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEngine")
            .field("embed_dim", &self.embed_dim)
            .field("vector_strategy", &self.vector_strategy.name())
            .field("active_strategy", &self.active_strategy_name())
            .field("reranker", &self.reranker_name())
            .finish_non_exhaustive()
    }
}

impl MemoryEngine {
    /// Open or create a memory engine backed by a SQLite file.
    ///
    /// On first open, writes `embed_dim` to the config table.
    /// On subsequent opens, validates the stored `embed_dim` matches.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration` if the stored `embed_dim` doesn't match.
    pub fn open(config: &EngineConfig) -> Result<Self> {
        let pool = if config.read_only {
            ConnectionPool::open_read_only(&config.path, config.embed_dim, config.read_pool_size)?
        } else {
            ConnectionPool::open(
                &config.path,
                config.embed_dim,
                config.read_pool_size,
                config.backup_dir.as_deref(),
            )?
        };
        Self::init_from_pool(
            pool,
            config.embed_dim,
            config.search_config.clone(),
            config.upcaster_registry.clone(),
            None,
        )
    }

    /// Open an in-memory engine for testing.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if the connection or schema setup fails.
    pub fn open_memory(embed_dim: usize) -> Result<Self> {
        let pool = ConnectionPool::open_memory(embed_dim)?;
        Self::init_from_pool(pool, embed_dim, None, UpcasterRegistry::new(), None)
    }

    /// Open an in-memory engine with optional search config for testing.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if the connection or schema setup fails.
    pub fn open_memory_with_config(
        embed_dim: usize,
        search_config: Option<SearchConfig>,
    ) -> Result<Self> {
        let pool = ConnectionPool::open_memory(embed_dim)?;
        Self::init_from_pool(
            pool,
            embed_dim,
            search_config,
            UpcasterRegistry::new(),
            None,
        )
    }

    /// Open a file-backed engine with an optional reranker.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration` if the stored `embed_dim` doesn't match.
    pub fn open_with_reranker(
        config: &EngineConfig,
        reranker: Option<Box<dyn Reranker>>,
    ) -> Result<Self> {
        let pool = if config.read_only {
            ConnectionPool::open_read_only(&config.path, config.embed_dim, config.read_pool_size)?
        } else {
            ConnectionPool::open(
                &config.path,
                config.embed_dim,
                config.read_pool_size,
                config.backup_dir.as_deref(),
            )?
        };
        Self::init_from_pool(
            pool,
            config.embed_dim,
            config.search_config.clone(),
            config.upcaster_registry.clone(),
            reranker,
        )
    }

    /// Open an in-memory engine with optional search config and reranker.
    ///
    /// Subsumes `open_memory_with_config()` — allows combining both.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if the connection or schema setup fails.
    pub fn open_memory_with(
        embed_dim: usize,
        search_config: Option<SearchConfig>,
        reranker: Option<Box<dyn Reranker>>,
    ) -> Result<Self> {
        let pool = ConnectionPool::open_memory(embed_dim)?;
        Self::init_from_pool(
            pool,
            embed_dim,
            search_config,
            UpcasterRegistry::new(),
            reranker,
        )
    }

    /// Shared constructor logic: validate embed_dim, load graph and scope tree.
    fn init_from_pool(
        pool: ConnectionPool,
        embed_dim: usize,
        search_config: Option<SearchConfig>,
        upcaster_registry: UpcasterRegistry,
        reranker: Option<Box<dyn Reranker>>,
    ) -> Result<Self> {
        // Scope the guard so it drops before we move `pool` into the struct.
        let (graph, scope_tree) = if pool.is_read_only() {
            // Read-only: use read connection, validate only (no set)
            let conn = pool.read();
            Self::validate_embed_dim(&conn, embed_dim)?;
            let graph = MemoryGraph::load_from_db(&conn)?;
            let scope_tree = ScopeTree::load(&conn)?;
            (graph, scope_tree)
        } else {
            let conn = pool.write();
            Self::validate_or_set_embed_dim(&conn, embed_dim)?;
            let graph = MemoryGraph::load_from_db(&conn)?;
            let scope_tree = ScopeTree::load(&conn)?;
            (graph, scope_tree)
        };

        // HNSW build is read-only — always use a read connection.
        #[cfg(feature = "ann")]
        let hnsw_strategy = if let Some(ref cfg) = search_config {
            if cfg.ann_threshold < usize::MAX {
                let conn = pool.read();
                Some(crate::search::ann::HnswStrategy::build_from_db(
                    &conn, embed_dim,
                )?)
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self {
            pool,
            embed_dim,
            graph: RwLock::new(graph),
            scope_tree: RwLock::new(scope_tree),
            vector_strategy: Box::new(BruteForce),
            reranker,
            #[cfg(feature = "ann")]
            hnsw_strategy,
            search_config,
            upcaster_registry,
        })
    }

    /// Returns the name of the active reranker, if any.
    #[must_use]
    pub fn reranker_name(&self) -> Option<&str> {
        self.reranker.as_ref().map(|r| r.name())
    }

    /// Name of the strategy that would be used for a query right now.
    #[must_use]
    pub fn active_strategy_name(&self) -> &str {
        if self.should_use_hnsw() {
            "hnsw"
        } else {
            "brute_force"
        }
    }

    #[cfg(feature = "ann")]
    fn should_use_hnsw(&self) -> bool {
        self.hnsw_strategy.as_ref().map_or(false, |hnsw| {
            hnsw.active_count()
                >= self
                    .search_config
                    .as_ref()
                    .map_or(usize::MAX, |c| c.ann_threshold)
        })
    }

    #[cfg(not(feature = "ann"))]
    const fn should_use_hnsw(&self) -> bool {
        false
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

    /// Validate stored embed_dim matches requested — read-only, never writes.
    fn validate_embed_dim(conn: &Connection, embed_dim: usize) -> Result<()> {
        if let Some(stored) = get_config(conn, "embed_dim")? {
            let stored_dim: usize = stored.parse().map_err(|_| {
                MemoryError::Migration(format!("invalid stored embed_dim: {stored}"))
            })?;
            if stored_dim != embed_dim {
                return Err(MemoryError::Migration(format!(
                    "embed_dim mismatch: stored {stored_dim} vs requested {embed_dim}"
                )));
            }
            Ok(())
        } else {
            Err(MemoryError::Migration(
                "embed_dim not set in database; open in read-write mode first".to_string(),
            ))
        }
    }

    /// Whether this engine was opened in read-only mode.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.pool.is_read_only()
    }

    // --- Private connection dispatch helpers ---

    /// Execute a read operation on a connection from the read pool.
    fn with_read<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R>,
    {
        let conn = self.pool.read();
        f(&conn)
    }

    /// Lock the write connection and return the guard directly.
    /// Callers use this when they need to hold the write lock across
    /// multiple operations (e.g., DB mutation + cache update).
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    fn write_conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.pool.try_write()
    }

    /// Stamp `surfaced_at` for the given fact IDs and return the DB-authoritative
    /// `(id, timestamp)` pairs. Shared by `list_due()` and `resume_context()`.
    fn stamp_surfaced_facts(
        &self,
        fact_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Result<std::collections::HashMap<i64, DateTime<Utc>>> {
        let conn = self.write_conn()?;
        let stamped = FactStore::new(&conn, self.embed_dim).stamp_surfaced(fact_ids, now)?;
        Ok(stamped.into_iter().collect())
    }

    /// Ensure a scope path exists using an already-held connection.
    ///
    /// Shared helper for [`ensure_scope_path()`] and [`add_fact()`] to avoid
    /// duplicating scope resolution logic.
    fn ensure_scope_with_conn(&self, conn: &Connection, path: &str) -> Result<i64> {
        let scope_store = ScopeStore::new(conn);
        let id = scope_store.ensure_path(path)?;
        let node = scope_store.get(id)?;
        self.scope_tree.write().insert(node);
        Ok(id)
    }

    // --- Public API: Ingest ---

    /// Append an event to the event log. Returns the assigned event id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on insert failure.
    pub fn ingest(&self, event: &NewEvent) -> Result<i64> {
        let conn = self.write_conn()?;
        EventStore::new(&conn, &self.upcaster_registry).insert(event)
    }

    /// Add a fact: compute embedding via `embedder`, compute blake3 content hash,
    /// and insert into the fact store. Returns the assigned fact id.
    ///
    /// Embedding is computed **before** acquiring the write lock, so slow
    /// embedding calls (network API) don't block readers.
    ///
    /// # Errors
    ///
    /// Returns errors from embedding computation, dimension validation, or DB insert.
    #[allow(clippy::too_many_arguments)]
    pub fn add_fact(
        &self,
        content: &str,
        fact_type: FactType,
        source_event_id: Option<i64>,
        embedder: &dyn EmbeddingProvider,
        scope: Option<&str>,
        opts: Option<&AddFactOptions>,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<i64> {
        // Embed OUTSIDE the write lock (potentially slow)
        let embedding = embedder.embed(content)?;
        let now = Utc::now();
        let opts = opts.cloned().unwrap_or_default();
        let base_importance = opts.importance.unwrap_or(0.5);
        let effective_created = opts.t_created.unwrap_or(now);
        let effective_last_accessed = opts.last_accessed.unwrap_or(now);

        // Classify OUTSIDE the write lock (potentially slow — LLM, I/O, etc.)
        // Uses scope_id=0 placeholder; classifiers should rely on content/type/importance/metadata.
        let is_pinned = match opts.pinned {
            Some(p) => p,
            None => classifier.is_some_and(|c| {
                let temp = Fact {
                    id: 0,
                    content: content.into(),
                    content_hash: String::new(),
                    embedding: embedding.clone(),
                    fact_type: fact_type.clone(),
                    t_created: effective_created,
                    t_expired: None,
                    t_valid: opts.t_valid,
                    t_invalid: opts.t_invalid,
                    source_event_id,
                    importance: base_importance,
                    access_count: 0,
                    last_accessed: effective_last_accessed,
                    metadata: opts
                        .metadata
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({})),
                    scope_id: 0,
                    is_pinned: false,
                    importance_score: base_importance,
                    surfaced_at: None,
                };
                c.should_pin(&temp)
            }),
        };

        // Resolve scope + insert fact in a single write lock, then release
        #[cfg(feature = "ann")]
        let emb_copy = embedding.clone();

        let fact_id = {
            let conn = self.write_conn()?;
            let scope_id = match scope {
                Some(path) => self.ensure_scope_with_conn(&conn, path)?,
                None => 1, // root scope
            };

            let new_fact = NewFact {
                content: content.into(),
                content_hash: String::new(), // FactStore::insert computes this via blake3
                embedding,
                fact_type,
                t_created: effective_created,
                t_expired: None,
                t_valid: opts.t_valid,
                t_invalid: opts.t_invalid,
                source_event_id,
                scope_id,
                importance: opts.importance.unwrap_or(0.5),
                access_count: 0,
                last_accessed: effective_last_accessed,
                metadata: opts.metadata.unwrap_or_else(|| serde_json::json!({})),
                is_pinned,
            };

            FactStore::new(&conn, self.embed_dim).insert(&new_fact)?
        }; // DB lock released = committed

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            hnsw.notify_insert(fact_id, &emb_copy);
        }

        Ok(fact_id)
    }

    /// Add multiple facts atomically: batch-embed all texts in a single call,
    /// classify outside the lock, then insert all facts in one transaction.
    ///
    /// Returns all assigned fact IDs on success. On any insert failure the
    /// entire batch is rolled back (all-or-nothing).
    ///
    /// # Performance
    ///
    /// - 1 embedding call (batch) instead of N
    /// - 1 SQLite transaction (savepoint) instead of N
    /// - Scope resolution inside savepoint for atomicity; `scope_tree`
    ///   cache deferred to after `RELEASE` (two-phase commit).
    ///
    /// # Errors
    ///
    /// Returns errors from batch embedding, dimension validation, or DB insert.
    pub fn add_facts_batch(
        &self,
        entries: &[BatchFactEntry],
        embedder: &dyn EmbeddingProvider,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<Vec<i64>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        // --- Phase 1: Batch embed OUTSIDE the write lock ---
        let texts: Vec<&str> = entries.iter().map(|e| e.content.as_str()).collect();
        let embeddings = embedder.embed_batch(&texts)?;

        if embeddings.len() != entries.len() {
            return Err(MemoryError::Internal(format!(
                "embed_batch returned {} embeddings for {} entries",
                embeddings.len(),
                entries.len()
            )));
        }

        // --- Phase 2: Classify + prepare OUTSIDE the write lock ---
        let now = Utc::now();

        let prepared: Vec<_> = entries
            .iter()
            .zip(embeddings.into_iter())
            .map(|(entry, embedding)| {
                let opts = entry.opts.clone().unwrap_or_default();
                let base_importance = opts.importance.unwrap_or(0.5);
                let effective_created = opts.t_created.unwrap_or(now);
                let effective_last_accessed = opts.last_accessed.unwrap_or(now);

                let is_pinned = match opts.pinned {
                    Some(p) => p,
                    None => classifier.is_some_and(|c| {
                        let temp = Fact {
                            id: 0,
                            content: entry.content.clone(),
                            content_hash: String::new(),
                            embedding: embedding.clone(),
                            fact_type: entry.fact_type.clone(),
                            t_created: effective_created,
                            t_expired: None,
                            t_valid: opts.t_valid,
                            t_invalid: opts.t_invalid,
                            source_event_id: entry.source_event_id,
                            importance: base_importance,
                            access_count: 0,
                            last_accessed: effective_last_accessed,
                            metadata: opts
                                .metadata
                                .clone()
                                .unwrap_or_else(|| serde_json::json!({})),
                            scope_id: 0,
                            is_pinned: false,
                            importance_score: base_importance,
                            surfaced_at: None,
                        };
                        c.should_pin(&temp)
                    }),
                };

                (
                    entry,
                    embedding,
                    opts,
                    is_pinned,
                    effective_created,
                    effective_last_accessed,
                )
            })
            .collect();

        // --- Phase 3: DB operations INSIDE the write lock ---
        #[cfg(feature = "ann")]
        let mut hnsw_pairs: Vec<(i64, Vec<f32>)> = Vec::with_capacity(entries.len());

        let fact_ids = {
            let conn = self.write_conn()?;
            conn.execute_batch("SAVEPOINT batch_insert")?;

            let result = (|| -> Result<(Vec<i64>, Vec<i64>)> {
                let scope_store = ScopeStore::new(&conn);
                let store = FactStore::new(&conn, self.embed_dim);

                // Resolve scopes INSIDE the savepoint so they roll back on
                // error. Deduplicate paths to avoid redundant DB lookups.
                let mut scope_cache: std::collections::HashMap<String, i64> =
                    std::collections::HashMap::new();
                let mut scope_ids = Vec::with_capacity(prepared.len());

                for (entry, ..) in &prepared {
                    let scope_id = match &entry.scope {
                        Some(path) => {
                            if let Some(&cached) = scope_cache.get(path) {
                                cached
                            } else {
                                let id = scope_store.ensure_path(path)?;
                                scope_cache.insert(path.clone(), id);
                                id
                            }
                        }
                        None => 1, // root scope
                    };
                    scope_ids.push(scope_id);
                }

                let mut ids = Vec::with_capacity(prepared.len());

                for (
                    i,
                    (entry, embedding, opts, is_pinned, effective_created, effective_last_accessed),
                ) in prepared.iter().enumerate()
                {
                    let new_fact = NewFact {
                        content: entry.content.clone(),
                        content_hash: String::new(), // FactStore::insert computes via blake3
                        embedding: embedding.clone(),
                        fact_type: entry.fact_type.clone(),
                        t_created: *effective_created,
                        t_expired: None,
                        t_valid: opts.t_valid,
                        t_invalid: opts.t_invalid,
                        source_event_id: entry.source_event_id,
                        scope_id: scope_ids[i],
                        importance: opts.importance.unwrap_or(0.5),
                        access_count: 0,
                        last_accessed: *effective_last_accessed,
                        metadata: opts
                            .metadata
                            .clone()
                            .unwrap_or_else(|| serde_json::json!({})),
                        is_pinned: *is_pinned,
                    };

                    let fact_id = store.insert(&new_fact)?;

                    #[cfg(feature = "ann")]
                    hnsw_pairs.push((fact_id, embedding.clone()));

                    ids.push(fact_id);
                }

                // Collect unique scope_ids for deferred cache update
                let unique_scope_ids: Vec<i64> = scope_cache.into_values().collect();

                Ok((ids, unique_scope_ids))
            })();

            match result {
                Ok((ids, scope_ids_to_cache)) => {
                    conn.execute_batch("RELEASE batch_insert")?;

                    // Deferred scope_tree cache update — only after successful
                    // commit. Prevents cache desync on rollback.
                    let scope_store = ScopeStore::new(&conn);
                    let mut tree = self.scope_tree.write();
                    for sid in scope_ids_to_cache {
                        if let Ok(node) = scope_store.get(sid) {
                            tree.insert(node);
                        }
                    }
                    drop(tree);

                    ids
                }
                Err(e) => {
                    // ROLLBACK TO restores savepoint but keeps it open —
                    // RELEASE closes it, leaving the connection clean.
                    let _ = conn.execute_batch("ROLLBACK TO batch_insert");
                    let _ = conn.execute_batch("RELEASE batch_insert");
                    return Err(e);
                }
            }
        }; // write lock released

        // --- Phase 4: HNSW notification AFTER lock (success only) ---
        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for (fact_id, emb) in &hnsw_pairs {
                hnsw.notify_insert(*fact_id, emb);
            }
        }

        Ok(fact_ids)
    }

    // --- Public API: Query ---

    /// Query facts using hybrid search (FTS5 + vector + RRF).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn query(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        // Resolve scope IDs from cache (short-lived read lock).
        // When a scope query is provided but the path doesn't exist,
        // return empty results instead of silently falling through to unscoped search.
        let scope_ids: Option<Vec<i64>> = match &query.scope {
            Some(sq) => {
                let resolved = self.scope_tree.read().resolve_query(sq);
                match resolved {
                    Some(ids) => Some(ids),
                    None => return Ok(vec![]), // scope doesn't exist → no results
                }
            }
            None => None,
        };

        #[cfg(feature = "ann")]
        let strategy: &dyn VectorSearchStrategy = if self.should_use_hnsw() {
            self.hnsw_strategy.as_ref().unwrap()
        } else {
            &*self.vector_strategy
        };
        #[cfg(not(feature = "ann"))]
        let strategy: &dyn VectorSearchStrategy = &*self.vector_strategy;

        let (mut results, _diagnostics) = self.with_read(|conn| {
            hybrid_search(conn, query, self.embed_dim, scope_ids.as_deref(), strategy)
        })?;

        // Apply reranker if present and query has text (cross-encoder needs query text).
        // Runs OUTSIDE the read lock — reranking may involve slow inference/API calls.
        if let (Some(reranker), Some(text)) = (&self.reranker, &query.text) {
            let ranked = reranker.rerank(text, &results)?;
            Self::validate_reranker_output(results.len(), &ranked)?;
            // Reconstruct results from indices — move, don't clone (#144).
            // Safe: validate_reranker_output guarantees unique indices.
            let mut candidates: Vec<_> = results.into_iter().map(Some).collect();
            results = ranked
                .into_iter()
                .map(|(idx, score)| {
                    let mut r = candidates[idx].take().expect("index validated as unique");
                    r.score = score;
                    r
                })
                .collect();
        }

        // Always truncate to limit — rerank_depth may have over-fetched from hybrid_search.
        results.truncate(query.limit);

        Ok(results)
    }

    // --- Public API: MemoryQuery ---

    /// Execute a composed query using the [`MemoryQuery`] builder.
    ///
    /// Routes to hybrid search (FTS + vector) when text/embedding is present,
    /// or to store-level queries (importance, pinned, period) otherwise.
    /// All code paths enforce the temporal safety invariant: future-dated facts
    /// (`t_valid > now`) are excluded unless overridden by `valid_at` or `period`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if:
    /// - `period_start` is set without `period_end` (or vice versa)
    /// - `valid_at` and `period` are both set (mutually exclusive)
    /// - `search_mode` conflicts with available text/embedding inputs
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn execute_query(&self, query: &MemoryQuery) -> Result<QueryResponse> {
        // --- Validation ---
        self.validate_memory_query(query)?;

        // --- Resolve scope ---
        let scope_ids: Option<Vec<i64>> = match &query.scope {
            Some(sq) => {
                let resolved = self.scope_tree.read().resolve_query(sq);
                match resolved {
                    Some(ids) => Some(ids),
                    None => {
                        return Ok(QueryResponse {
                            results: vec![],
                            diagnostics: QueryDiagnostics::default(),
                        });
                    }
                }
            }
            None => None,
        };

        // --- Compute effective temporal cutoff ---
        // Default: Utc::now() to hide future-dated facts (scheduling model invariant).
        // Overridden by explicit valid_at. Disabled when period is set (period handles its own semantics).
        let effective_cutoff = if query.valid_at.is_some() {
            query.valid_at
        } else if query.has_period() {
            None // period handles temporal semantics itself
        } else {
            Some(Utc::now()) // default: hide future-dated facts
        };

        let limit = query.effective_limit();

        if query.has_search() {
            self.execute_search_path(query, scope_ids.as_deref(), effective_cutoff, limit)
        } else {
            self.execute_store_path(query, scope_ids.as_deref(), effective_cutoff, limit)
        }
    }

    /// Validate a `MemoryQuery` for conflicting or incomplete options.
    fn validate_memory_query(&self, query: &MemoryQuery) -> Result<()> {
        // Period: both start and end must be set, or neither
        if query.period_start.is_some() != query.period_end.is_some() {
            return Err(MemoryError::Conflict(
                "period_start and period_end must both be set or both unset".into(),
            ));
        }

        // valid_at and period are mutually exclusive
        if query.valid_at.is_some() && query.has_period() {
            return Err(MemoryError::Conflict(
                "valid_at and period are mutually exclusive".into(),
            ));
        }

        // Search mode compatibility (D7)
        if let Some(ref mode) = query.search_mode {
            match mode {
                SearchMode::Fts if query.text.is_none() => {
                    return Err(MemoryError::Conflict(
                        "SearchMode::Fts requires text to be set".into(),
                    ));
                }
                SearchMode::Vector if query.embedding.is_none() => {
                    return Err(MemoryError::Conflict(
                        "SearchMode::Vector requires embedding to be set".into(),
                    ));
                }
                SearchMode::Hybrid if query.text.is_none() || query.embedding.is_none() => {
                    return Err(MemoryError::Conflict(
                        "SearchMode::Hybrid requires both text and embedding".into(),
                    ));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Infer search mode from available inputs (D7).
    fn infer_search_mode(query: &MemoryQuery) -> SearchMode {
        if let Some(ref mode) = query.search_mode {
            return mode.clone();
        }
        match (query.text.is_some(), query.embedding.is_some()) {
            (true, true) => SearchMode::Hybrid,
            (true, false) => SearchMode::Fts,
            (false, true) => SearchMode::Vector,
            (false, false) => unreachable!("has_search() should be false"),
        }
    }

    /// Search path: delegate to hybrid_search, then post-filter.
    ///
    /// When post-filters are active (period, importance, pinned), we pass the
    /// raw `limit` (not inflated) because `hybrid_search` already does its own
    /// internal 3x overfetch before its temporal filter.
    fn execute_search_path(
        &self,
        query: &MemoryQuery,
        scope_ids: Option<&[i64]>,
        effective_cutoff: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<QueryResponse> {
        let mode = Self::infer_search_mode(query);

        // When period is set, effective_cutoff is None — but hybrid_search defaults
        // valid_at=None to Utc::now(), which would hide facts outside the current
        // instant. We need hybrid_search to return ALL temporally-viable candidates
        // so our period post-filter can apply exact interval overlap semantics.
        // Fix: pass MAX_UTC as the cutoff to effectively disable hybrid_search's
        // built-in temporal filter. The period post-filter handles the real semantics.
        let search_cutoff = if effective_cutoff.is_none() && query.has_period() {
            Some(DateTime::<Utc>::MAX_UTC)
        } else {
            effective_cutoff
        };

        // hybrid_search already does its own 3x overfetch internally (line 87),
        // so we pass the raw limit here — no double inflation.
        let search_query = SearchQuery {
            text: query.text.clone(),
            embedding: query.embedding.clone(),
            mode,
            limit,
            rerank_depth: None,
            valid_at: search_cutoff,
            fact_type: query.fact_type.clone(),
            scope: query.scope.clone(),
        };

        #[cfg(feature = "ann")]
        let strategy: &dyn VectorSearchStrategy = if self.should_use_hnsw() {
            self.hnsw_strategy.as_ref().unwrap()
        } else {
            &*self.vector_strategy
        };
        #[cfg(not(feature = "ann"))]
        let strategy: &dyn VectorSearchStrategy = &*self.vector_strategy;

        let (mut results, mut diagnostics) = self.with_read(|conn| {
            hybrid_search(conn, &search_query, self.embed_dim, scope_ids, strategy)
        })?;

        // Post-filter by period overlap
        if let (Some(start), Some(end)) = (query.period_start, query.period_end) {
            results.retain(|r| fact_overlaps_period(&r.fact, start, end));
        }

        // Post-filter by importance score
        if let Some(min_score) = query.min_importance_score {
            results.retain(|r| r.fact.importance_score >= min_score);
        }

        // Post-filter by pinned
        if query.pinned_only {
            results.retain(|r| r.fact.is_pinned);
        }

        results.truncate(limit);
        diagnostics.results_returned = results.len();

        // Expired-facts probe (opt-in, FTS-only).
        if query.include_expired_probe {
            if let Some(text) = &query.text {
                let fact_type_ref = query.fact_type.as_ref();
                let expired_count = self.with_read(|conn| {
                    crate::search::fts::fts_count_expired(conn, text, fact_type_ref, scope_ids)
                })?;
                diagnostics.expired_matches = Some(expired_count);
            }
            // Vector-only queries: expired_matches stays None (documented limitation).
        }

        Ok(QueryResponse {
            results,
            diagnostics,
        })
    }

    /// Store path: no text/vector search, use FactStore directly.
    ///
    /// Strategy: fetch a broad candidate set from the most selective SQL query,
    /// then apply ALL remaining filters as post-filters to guarantee AND semantics.
    fn execute_store_path(
        &self,
        query: &MemoryQuery,
        scope_ids: Option<&[i64]>,
        effective_cutoff: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<QueryResponse> {
        let scope_slice = scope_ids.unwrap_or(&[]);

        // Fetch a broad candidate set. Use the most selective SQL query available,
        // but do NOT rely on it for AND semantics — post-filters handle that.
        // Overfetch to compensate for post-filter attrition.
        let fetch_limit = limit.saturating_mul(3).max(limit);

        let mut facts: Vec<Fact> =
            if let (Some(start), Some(end)) = (query.period_start, query.period_end) {
                // Period is the most selective: overlap + scope + optional fact_type in SQL.
                self.with_read(|conn| {
                    FactStore::new(conn, self.embed_dim).list_active_in_period(
                        start,
                        end,
                        scope_slice,
                        query.fact_type.as_ref(),
                    )
                })?
            } else {
                // Default: fetch by importance_score DESC (most useful ordering).
                // Use min_importance_score if set, otherwise 0.0 (all active facts).
                let min_score = query.min_importance_score.unwrap_or(0.0);
                self.with_read(|conn| {
                    FactStore::new(conn, self.embed_dim).list_by_importance_score(
                        scope_slice,
                        min_score,
                        fetch_limit,
                        &std::collections::HashSet::new(),
                    )
                })?
            };

        // --- Post-filters: apply ALL filters for AND semantics ---

        // Capture candidate count before post-filters for diagnostics.
        let candidates_before_filter = facts.len();

        // Temporal safety: exclude future-dated facts
        if let Some(cutoff) = effective_cutoff {
            facts.retain(|f| passes_temporal_cutoff(f, cutoff));
        }

        // Fact type (period branch already handles this in SQL; skip to avoid double-filter)
        if !query.has_period() {
            if let Some(ref ft) = query.fact_type {
                facts.retain(|f| &f.fact_type == ft);
            }
        }

        // Importance score (the default branch already handles this in SQL via min_score;
        // but the period branch does NOT, so we must apply it here for AND semantics)
        if query.has_period() {
            if let Some(min_score) = query.min_importance_score {
                facts.retain(|f| f.importance_score >= min_score);
            }
        }

        // Pinned filter — always applied as post-filter regardless of primary query
        if query.pinned_only {
            facts.retain(|f| f.is_pinned);
        }

        // Sort by importance_score DESC and truncate
        facts.sort_by(|a, b| {
            b.importance_score
                .partial_cmp(&a.importance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        facts.truncate(limit);

        let results: Vec<SearchResult> = facts.into_iter().map(fact_to_search_result).collect();
        let diagnostics = QueryDiagnostics {
            candidates_before_filter,
            results_returned: results.len(),
            // Store path has no FTS/vector search — no expired probe possible.
            ..QueryDiagnostics::default()
        };
        Ok(QueryResponse {
            results,
            diagnostics,
        })
    }

    // --- Public API: Config ---

    /// Read a config value by key.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        self.with_read(|conn| get_config(conn, key))
    }

    /// Write a config value (upsert).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on write failure.
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.write_conn()?;
        set_config(&conn, key, value)
    }

    // --- Public API: Bootstrap ---

    /// Bootstrap a single JSONL session log into historical memory.
    ///
    /// Parses the session log, extracts noteworthy episodes via keyword
    /// pre-filter, classifies session outcome, and ingests extracted facts.
    /// The entire session import is wrapped in a SQLite savepoint for
    /// crash safety (all-or-nothing per session).
    ///
    /// For LLM-powered extraction, provide a custom [`SessionExtractor`].
    /// The default [`KeywordExtractor`] requires no LLM.
    ///
    /// # Errors
    ///
    /// Returns errors from embedding, DB insertion, or scope resolution.
    pub fn bootstrap_session(
        &self,
        reader: impl std::io::BufRead,
        embedder: &dyn EmbeddingProvider,
        extractor: &dyn crate::bootstrap::SessionExtractor,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let conn = self.write_conn()?;
        let scope_id = match &config.scope {
            Some(path) => {
                let scope_store = ScopeStore::new(&conn);
                let id = scope_store.ensure_path(path)?;
                let node = scope_store.get(id)?;
                self.scope_tree.write().insert(node);
                id
            }
            None => 1,
        };
        crate::bootstrap::bootstrap_session_inner(
            &conn,
            self.embed_dim,
            &self.upcaster_registry,
            reader,
            embedder,
            extractor,
            config,
            classifier,
            scope_id,
        )
    }

    /// Bootstrap all JSONL session logs in a directory.
    ///
    /// Discovers top-level `*.jsonl` files (not subagent subdirectories).
    /// Each session is processed independently within its own savepoint.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Io` for directory traversal failures.
    pub fn bootstrap_directory(
        &self,
        dir: &std::path::Path,
        embedder: &dyn EmbeddingProvider,
        extractor: &dyn crate::bootstrap::SessionExtractor,
        config: &crate::bootstrap::BootstrapConfig,
        classifier: Option<&dyn PersistenceClassifier>,
    ) -> Result<crate::bootstrap::BootstrapReport> {
        let conn = self.write_conn()?;
        let scope_id = match &config.scope {
            Some(path) => {
                let scope_store = ScopeStore::new(&conn);
                let id = scope_store.ensure_path(path)?;
                let node = scope_store.get(id)?;
                self.scope_tree.write().insert(node);
                id
            }
            None => 1,
        };
        crate::bootstrap::bootstrap_directory_inner(
            &conn,
            self.embed_dim,
            &self.upcaster_registry,
            dir,
            embedder,
            extractor,
            config,
            classifier,
            scope_id,
        )
    }

    // --- Public API: Consolidation ---

    /// Run three-pass consolidation: local dedup, cluster fusion, global integration.
    ///
    /// # Errors
    ///
    /// Propagates errors from any consolidation pass or the `SummaryGenerator`.
    pub fn consolidate(
        &self,
        generator: &dyn SummaryGenerator,
        config: &ConsolidationConfig,
    ) -> Result<ConsolidationStats> {
        let (stats, expired_ids) = {
            let conn = self.write_conn()?;
            let (stats, expired_ids) =
                crate::consolidation::consolidate(&conn, generator, self.embed_dim, config)?;

            // Rebuild graph inside write lock — dedup may have expired facts and their edges
            if stats.duplicates_removed > 0 {
                *self.graph.write() = MemoryGraph::load_from_db(&conn)?;
            }

            (stats, expired_ids)
        }; // DB lock released

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for &id in &expired_ids {
                hnsw.notify_expire(id);
            }
        }

        // Suppress unused variable warning when ann feature is disabled
        #[cfg(not(feature = "ann"))]
        let _ = expired_ids;

        Ok(stats)
    }

    // --- Public API: Forgetting ---

    /// Prune stale facts using Ebbinghaus decay and graph-aware importance scoring.
    ///
    /// Facts with computed importance below `policy.min_importance` get soft-deleted.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the policy is invalid.
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn forget(&self, policy: &ForgetPolicy) -> Result<PruneStats> {
        let (stats, pruned_ids) = {
            let conn = self.write_conn()?;
            let mut graph = self.graph.write();
            crate::forgetting::prune(&conn, &mut graph, policy, self.embed_dim, Utc::now())?
        };

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            for &id in &pruned_ids {
                hnsw.notify_expire(id);
            }
        }

        let _ = pruned_ids; // consumed above when ann is enabled; suppress unused warning otherwise
        Ok(stats)
    }

    // --- Public API: Conflict Resolution ---

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
        &self,
        arbiter: &dyn ConflictArbiter,
        old_id: i64,
        new_fact: &NewFact,
    ) -> Result<ConflictResolution> {
        #[cfg(feature = "ann")]
        let embedding = new_fact.embedding.clone();
        let resolution = {
            let conn = self.write_conn()?;
            let mut graph = self.graph.write();
            crate::conflict::resolve_conflict(
                &conn,
                &mut graph,
                arbiter,
                old_id,
                new_fact,
                self.embed_dim,
                Utc::now(),
            )?
        }; // DB lock + graph lock released

        #[cfg(feature = "ann")]
        if let Some(ref hnsw) = self.hnsw_strategy {
            use crate::traits::CrudDecision;
            if matches!(
                &resolution.decision,
                CrudDecision::Update | CrudDecision::Delete
            ) {
                hnsw.notify_expire(old_id);
            }
            if matches!(
                &resolution.decision,
                CrudDecision::Update | CrudDecision::Add
            ) {
                if let Some(new_id) = resolution.new_fact_id {
                    hnsw.notify_insert(new_id, &embedding);
                }
            }
        }

        Ok(resolution)
    }

    // --- Public API: Co-session edge creation ---

    /// Relation type for edges linking facts that co-occur in the same session.
    const CO_SESSION_RELATION: &str = "co_session";
    /// Default weight for co-session edges — weaker than explicit semantic
    /// relationships by design intent. Note: the current forgetting system uses
    /// raw `graph.degree()` (unweighted), so the weight does not yet reduce
    /// connectivity impact. It will matter once weighted traversal ships (Phase 5).
    const CO_SESSION_WEIGHT: f64 = 0.5;
    /// Scope ID for co-session edges — root scope, since co-session is cross-scope.
    const CO_SESSION_SCOPE_ID: i64 = 1;

    /// Create `co_session` edges between all active facts sharing a session.
    ///
    /// Edges are bidirectional (A→B and B→A), with weight
    /// [`CO_SESSION_WEIGHT`](Self::CO_SESSION_WEIGHT) and
    /// `scope_id =` [`CO_SESSION_SCOPE_ID`](Self::CO_SESSION_SCOPE_ID)
    /// (root — cross-scope by nature). Idempotent: calling twice for the same
    /// session does not create duplicate edges.
    ///
    /// When `scope` is `Some`, only facts within that scope subtree are
    /// considered. When `None`, all scopes are included (global lookup,
    /// backward-compatible).
    ///
    /// Returns the number of new edges created.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    /// Returns `MemoryError::NotFound` if `scope` is `Some` but the path
    /// does not exist.
    pub fn link_session_facts(&self, session_id: &str, scope: Option<&str>) -> Result<usize> {
        use crate::graph::EdgeData;
        use crate::store::edges::EdgeStore;

        // Resolve scope path → subtree IDs (short-lived read lock)
        let scope_ids: Vec<i64> = match scope {
            Some(path) => {
                let tree = self.scope_tree.read();
                let id = tree
                    .resolve_path(path)
                    .ok_or_else(|| MemoryError::NotFound(format!("scope path: {path}")))?;
                tree.subtree(id)
            }
            None => Vec::new(),
        };

        let conn = self.write_conn()?;
        let facts =
            FactStore::new(&conn, self.embed_dim).list_active_by_session(session_id, &scope_ids)?;

        if facts.len() < 2 {
            drop(conn);
            return Ok(0);
        }

        let now = Utc::now();
        let mut new_edges: Vec<(i64, i64, i64)> = Vec::new(); // (edge_id, src, tgt)

        {
            let tx = conn.unchecked_transaction()?;
            let edge_store = EdgeStore::new(&tx);

            // Batch-fetch existing co_session edges for dedup (1 query instead of N²)
            let fact_ids: Vec<i64> = facts.iter().map(|f| f.id).collect();
            let existing =
                edge_store.list_active_pairs_by_facts(&fact_ids, Self::CO_SESSION_RELATION)?;

            for i in 0..facts.len() {
                for j in (i + 1)..facts.len() {
                    let a_id = facts[i].id;
                    let b_id = facts[j].id;

                    for (src, tgt) in [(a_id, b_id), (b_id, a_id)] {
                        if !existing.contains(&(src, tgt)) {
                            let edge_id = edge_store.insert(&NewEdge {
                                source_fact_id: src,
                                target_fact_id: tgt,
                                relation_type: Self::CO_SESSION_RELATION.to_string(),
                                weight: Self::CO_SESSION_WEIGHT,
                                scope_id: Self::CO_SESSION_SCOPE_ID,
                                t_created: now,
                                t_expired: None,
                            })?;
                            new_edges.push((edge_id, src, tgt));
                        }
                    }
                }
            }

            tx.commit()?;
        }
        drop(conn);

        // Sync in-memory graph after successful commit
        if !new_edges.is_empty() {
            let mut graph = self.graph.write();
            for &(edge_id, src, tgt) in &new_edges {
                graph.add_edge(
                    src,
                    tgt,
                    EdgeData {
                        edge_id,
                        relation_type: Self::CO_SESSION_RELATION.to_string(),
                        weight: Self::CO_SESSION_WEIGHT,
                    },
                );
            }
        }

        Ok(new_edges.len())
    }

    // --- Public API: Graph queries (no lock exposure) ---

    /// Get the degree (in + out edges) for a fact in the graph.
    #[must_use]
    pub fn graph_degree(&self, fact_id: i64) -> usize {
        self.graph.read().degree(fact_id)
    }

    /// Get the connected component containing a fact.
    #[must_use]
    pub fn graph_component(&self, fact_id: i64) -> Vec<i64> {
        self.graph.read().connected_component(fact_id)
    }

    /// Get outgoing neighbors of a fact in the graph.
    #[must_use]
    pub fn graph_neighbors(&self, fact_id: i64) -> Vec<i64> {
        self.graph.read().neighbors(fact_id)
    }

    /// Graph statistics: (node_count, edge_count).
    #[must_use]
    pub fn graph_stats(&self) -> (usize, usize) {
        let g = self.graph.read();
        (g.node_count(), g.edge_count())
    }

    /// Check if a node exists in the graph.
    #[must_use]
    pub fn graph_has_node(&self, fact_id: i64) -> bool {
        self.graph.read().has_node(fact_id)
    }

    /// Embedding dimension configured for this engine.
    #[must_use]
    pub const fn embed_dim(&self) -> usize {
        self.embed_dim
    }

    /// Whether this engine is file-backed (vs in-memory).
    #[must_use]
    pub fn is_file_backed(&self) -> bool {
        self.pool.is_file_backed()
    }

    // --- Public API: Scope management ---

    /// Ensure the given scope path exists, creating missing segments as needed.
    ///
    /// Returns the scope id of the leaf node. This is side-effecting: missing
    /// intermediate path segments are created in the database and the in-memory
    /// scope tree is updated.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on insert failure.
    pub fn ensure_scope_path(&self, path: &str) -> Result<i64> {
        let conn = self.write_conn()?;
        self.ensure_scope_with_conn(&conn, path)
    }

    // --- Public API: Direct data access ---

    /// Get a fact by id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact doesn't exist.
    pub fn get_fact(&self, id: i64) -> Result<Fact> {
        self.with_read(|conn| FactStore::new(conn, self.embed_dim).get(id))
    }

    /// List active (non-expired) facts, optionally limited.
    ///
    /// When `limit` is `Some(n)`, a SQL `LIMIT` clause avoids materializing
    /// the entire corpus. `None` returns all active facts.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        self.with_read(|conn| FactStore::new(conn, self.embed_dim).list_active(limit))
    }

    /// List summaries by consolidation level.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub fn list_summaries(&self, level: &ConsolidationLevel) -> Result<Vec<crate::types::Summary>> {
        self.with_read(|conn| SummaryStore::new(conn, self.embed_dim).list_by_level(level))
    }

    // --- Public API: Scheduling ---

    /// Returns active facts where `t_valid <= now` and `t_valid IS NOT NULL`.
    ///
    /// On first return, stamps `surfaced_at` for facts that have not yet been
    /// surfaced. Subsequent calls return the original timestamp. The returned
    /// facts always carry the DB-authoritative `surfaced_at` value.
    pub fn list_due(&self, now: DateTime<Utc>, scope: Option<&str>) -> Result<Vec<Fact>> {
        let scope_ids = self.resolve_scope_ids(scope)?;
        let mut facts =
            self.with_read(|conn| FactStore::new(conn, self.embed_dim).list_due(now, &scope_ids))?;

        // Stamp surfaced_at for newly-surfaced facts
        let unsurfaced_ids: Vec<i64> = facts
            .iter()
            .filter(|f| f.surfaced_at.is_none())
            .map(|f| f.id)
            .collect();

        if !unsurfaced_ids.is_empty() {
            let stamped = self.stamp_surfaced_facts(&unsurfaced_ids, now)?;
            apply_surfaced_stamps(facts.iter_mut(), &stamped);
        }

        Ok(facts)
    }

    /// Scheduling hint: when should the consumer next call `list_due()`?
    /// Returns the earliest `t_valid` among active future-dated facts.
    pub fn next_due_time(&self, scope: Option<&str>) -> Result<Option<DateTime<Utc>>> {
        let scope_ids = self.resolve_scope_ids(scope)?;
        self.with_read(|conn| {
            FactStore::new(conn, self.embed_dim).next_due_time(Utc::now(), &scope_ids)
        })
    }

    // --- Public API: Pinning ---

    /// Pin a fact (make it unforgettable).
    pub fn pin_fact(&self, id: i64) -> Result<()> {
        let conn = self.write_conn()?;
        FactStore::new(&conn, self.embed_dim).set_pinned(id, true)
    }

    /// Unpin a fact (allow forgetting).
    pub fn unpin_fact(&self, id: i64) -> Result<()> {
        let conn = self.write_conn()?;
        FactStore::new(&conn, self.embed_dim).set_pinned(id, false)
    }

    // --- Public API: Inspection ---

    /// Compute aggregate statistics about the engine state.
    ///
    /// Returns counts of facts (active, expired, pinned, due), edges,
    /// summaries, scopes, events, and storage metrics.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn statistics(&self) -> Result<crate::inspect::EngineStatistics> {
        self.with_read(|conn| {
            crate::inspect::statistics::compute_statistics(conn, self.pool.path())
        })
    }

    /// Replay a segment of the event log for debugging.
    ///
    /// Supports filtering by time range, ID range, session, and event type.
    /// Default ordering is by insertion order (ID ascending) for deterministic replay.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn replay_events(
        &self,
        filter: &crate::inspect::ReplayFilter,
    ) -> Result<Vec<crate::types::Event>> {
        let event_filter = crate::inspect::replay::to_event_filter(filter);
        self.with_read(|conn| {
            let store = EventStore::new(conn, &self.upcaster_registry);
            if filter.upcast {
                store.list_upcasted(&event_filter)
            } else {
                store.list(&event_filter)
            }
        })
    }

    /// Explain why a fact is in its current state.
    ///
    /// Returns provenance, temporal state, graph context, and scope path.
    /// For expired facts, the graph context reflects the current (active-only) graph state.
    ///
    /// **Note:** `ExpiredReason` is best-effort. Most expired facts return `Unknown`
    /// until event-based audit trail is added in a future version.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact doesn't exist.
    /// Returns `MemoryError::Database` on SQL failure.
    ///
    /// # Lock strategy
    ///
    /// The graph `RwLock` is acquired and released *before* the database read
    /// to reduce lock contention — connected-component traversal is the expensive
    /// part and does not need to be held across the DB call. The `scope_tree`
    /// lock is kept across `with_read` because scope-path resolution depends on
    /// `fact.scope_id` fetched from the database.
    ///
    /// This means the graph context may reflect a slightly older snapshot than the
    /// database state under concurrent writes. This trade-off is intentional for
    /// an informational API — see [`inspect::explain::explain_fact_with_graph_context`].
    pub fn explain_fact(&self, id: i64) -> Result<crate::inspect::FactExplanation> {
        // Snapshot graph context under the graph lock, then release it.
        let graph_context = {
            let graph = self.graph.read();
            crate::inspect::explain::build_graph_context(&graph, id)
        };
        // scope_tree lock is still held across with_read because scope_path
        // resolution requires fact.scope_id, which comes from the DB.
        let scope_tree = self.scope_tree.read();
        self.with_read(|conn| {
            crate::inspect::explain::explain_fact_with_graph_context(
                conn,
                &scope_tree,
                self.embed_dim,
                id,
                &self.upcaster_registry,
                graph_context,
            )
        })
    }

    /// Reconstruct the temporal history of a fact from its bi-temporal timestamps.
    ///
    /// Returns a sorted timeline of lifecycle events computed from the fact's
    /// `t_created`, `t_valid`, `t_invalid`, and `t_expired` timestamps.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact doesn't exist.
    /// Returns `MemoryError::Database` on SQL failure.
    pub fn fact_history(&self, id: i64) -> Result<crate::inspect::FactHistory> {
        self.with_read(|conn| crate::inspect::explain::fact_history(conn, self.embed_dim, id))
    }

    /// Export full engine state to a file.
    ///
    /// - `DumpFormat::Json(path)`: Serializes all facts, edges, summaries, scopes,
    ///   events, and config to JSON. Works for both file-backed and in-memory engines.
    ///   Uses raw events (not upcasted) for snapshot fidelity.
    /// - `DumpFormat::JsonGzip(path)`: Same as `Json`, but gzip-compressed.
    ///   Requires the `compress-gzip` feature.
    /// - `DumpFormat::JsonZstd(path)`: Same as `Json`, but zstd-compressed.
    ///   Requires the `compress-zstd` feature.
    /// - `DumpFormat::Sqlite(path)`: Creates an atomic backup via `VACUUM INTO`.
    ///   Works for both file-backed and in-memory engines (`SQLite` 3.27+).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`] on filesystem failure.
    /// Returns [`MemoryError::Conflict`] if the target path resolves to the live database.
    /// Returns [`MemoryError::Database`] on SQL failure.
    /// Returns [`MemoryError::NotImplemented`] if a compression format is used
    /// without the corresponding feature enabled.
    #[allow(unreachable_patterns)] // wildcard needed for #[non_exhaustive] forward compat
    pub fn dump_state(&self, format: &crate::inspect::DumpFormat) -> Result<()> {
        match format {
            crate::inspect::DumpFormat::Json(path) => {
                self.with_read(|conn| crate::inspect::dump::dump_json(conn, self.embed_dim, path))
            }
            #[cfg(feature = "compress-gzip")]
            crate::inspect::DumpFormat::JsonGzip(path) => self
                .with_read(|conn| crate::inspect::dump::dump_json_gzip(conn, self.embed_dim, path)),
            #[cfg(not(feature = "compress-gzip"))]
            crate::inspect::DumpFormat::JsonGzip(_) => Err(MemoryError::NotImplemented(
                "gzip compression requires the `compress-gzip` feature".into(),
            )),
            #[cfg(feature = "compress-zstd")]
            crate::inspect::DumpFormat::JsonZstd(path) => self
                .with_read(|conn| crate::inspect::dump::dump_json_zstd(conn, self.embed_dim, path)),
            #[cfg(not(feature = "compress-zstd"))]
            crate::inspect::DumpFormat::JsonZstd(_) => Err(MemoryError::NotImplemented(
                "zstd compression requires the `compress-zstd` feature".into(),
            )),
            crate::inspect::DumpFormat::Sqlite(path) => {
                let conn = self.write_conn()?;
                crate::inspect::dump::dump_sqlite(&conn, path)
            }
            _ => Err(MemoryError::NotImplemented(
                "unsupported dump format".into(),
            )),
        }
    }

    // --- Restore (static constructors) ---

    /// Restore from a JSON snapshot into a new file-backed engine.
    ///
    /// `config.path` must not already exist. The `config.embed_dim` is validated
    /// against the snapshot's `embed_dim`. All other config fields (pool size,
    /// search config, upcaster registry, backup dir) are passed through.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the target path already exists.
    /// Returns `MemoryError::EmbeddingDimension` if `config.embed_dim` mismatches.
    /// Returns `MemoryError::Io` / `MemoryError::Serialization` on read failure.
    pub fn restore_json(snapshot_path: &Path, config: &EngineConfig) -> Result<Self> {
        if config.path.exists() {
            return Err(MemoryError::Conflict(
                "target database path already exists".into(),
            ));
        }

        let snapshot = crate::inspect::restore::read_snapshot(snapshot_path)?;

        if config.embed_dim != snapshot.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: config.embed_dim,
                actual: snapshot.embed_dim,
            });
        }

        let pool = ConnectionPool::open(
            &config.path,
            config.embed_dim,
            config.read_pool_size,
            config.backup_dir.as_deref(),
        );

        // On any failure after DB creation, clean up the orphan file.
        let pool = match pool {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&config.path);
                return Err(e);
            }
        };

        let restore_result = {
            let conn = pool.write();
            crate::inspect::restore::restore_snapshot_into(&conn, &snapshot)
        };

        if let Err(e) = restore_result {
            drop(pool);
            let _ = std::fs::remove_file(&config.path);
            return Err(e);
        }

        Self::init_from_pool(
            pool,
            config.embed_dim,
            config.search_config.clone(),
            config.upcaster_registry.clone(),
            None,
        )
    }

    /// Restore from a JSON snapshot into a new in-memory engine.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Io` / `MemoryError::Serialization` on read failure.
    pub fn restore_json_memory(snapshot_path: &Path) -> Result<Self> {
        let snapshot = crate::inspect::restore::read_snapshot(snapshot_path)?;
        let pool = ConnectionPool::open_memory(snapshot.embed_dim)?;

        {
            let conn = pool.write();
            crate::inspect::restore::restore_snapshot_into(&conn, &snapshot)?;
        }

        Self::init_from_pool(
            pool,
            snapshot.embed_dim,
            None,
            UpcasterRegistry::new(),
            None,
        )
    }

    /// Restore from a [`dump_sqlite()`](crate::inspect::dump::dump_sqlite) backup
    /// into a new file-backed engine.
    ///
    /// **Only accepts clean backups** produced by `dump_state(DumpFormat::Sqlite(..))`.
    /// Copying a live WAL-mode database is unsafe (the WAL sidecar may be missing).
    ///
    /// `config.path` must not already exist. The backup's `embed_dim` is validated
    /// against `config.embed_dim`.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Conflict` if the target path already exists.
    /// Returns `MemoryError::NotFound` if the backup file doesn't exist.
    /// Returns `MemoryError::EmbeddingDimension` on dimension mismatch.
    pub fn restore_sqlite(backup_path: &Path, config: &EngineConfig) -> Result<Self> {
        if config.path.exists() {
            return Err(MemoryError::Conflict(
                "target database path already exists".into(),
            ));
        }
        if !backup_path.exists() {
            return Err(MemoryError::NotFound(format!(
                "backup file: {}",
                backup_path.display()
            )));
        }

        std::fs::copy(backup_path, &config.path)?;

        // Any failure after copy must clean up the orphan file.
        let cleanup = |e| {
            let _ = std::fs::remove_file(&config.path);
            e
        };

        // Probe the copied DB for embed_dim validation.
        let probe = crate::store::schema::open_connection(&config.path.to_string_lossy())
            .map_err(cleanup)?;
        let db_dim: usize = get_config(&probe, "embed_dim")
            .map_err(cleanup)?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| {
                let _ = std::fs::remove_file(&config.path);
                MemoryError::Internal("backup has no embed_dim in config".into())
            })?;
        drop(probe);

        if config.embed_dim != db_dim {
            let _ = std::fs::remove_file(&config.path);
            return Err(MemoryError::EmbeddingDimension {
                expected: config.embed_dim,
                actual: db_dim,
            });
        }

        Self::open(config).inspect_err(|_| {
            let _ = std::fs::remove_file(&config.path);
        })
    }

    // --- Private helpers ---

    /// Validate reranker output indices and scores.
    ///
    /// Checks four invariants:
    /// 1. Output length does not exceed input length
    /// 2. Every index is within `0..num_candidates`
    /// 3. No duplicate indices
    /// 4. All scores are finite (not NaN or Inf)
    fn validate_reranker_output(num_candidates: usize, output: &[(usize, f64)]) -> Result<()> {
        use std::collections::HashSet;

        if output.len() > num_candidates {
            return Err(MemoryError::Reranker(format!(
                "reranker violated subset contract: output length ({}) exceeds input length ({num_candidates})",
                output.len(),
            )));
        }

        let mut seen = HashSet::with_capacity(output.len());

        for &(idx, score) in output {
            if idx >= num_candidates {
                return Err(MemoryError::Reranker(format!(
                    "reranker returned out-of-bounds index {idx} (candidates length: {num_candidates})",
                )));
            }
            if !seen.insert(idx) {
                return Err(MemoryError::Reranker(format!(
                    "reranker violated subset contract: duplicate index {idx} in output",
                )));
            }
            if !score.is_finite() {
                return Err(MemoryError::Reranker(format!(
                    "reranker returned non-finite score {score} for index {idx}",
                )));
            }
        }

        Ok(())
    }

    /// Resolve scope IDs from an optional scope path.
    /// Returns [root_id] when scope is None, or ancestor IDs when scope exists.
    fn resolve_scope_ids(&self, scope: Option<&str>) -> Result<Vec<i64>> {
        let tree = self.scope_tree.read();
        match scope {
            Some(path) => {
                let id = tree
                    .resolve_path(path)
                    .ok_or_else(|| MemoryError::NotFound(format!("scope path: {path}")))?;
                Ok(tree.ancestors(id))
            }
            None => Ok(vec![tree.root_id()]),
        }
    }

    // --- Public API: Resume ---

    /// Retrieve tiered context for resuming a session.
    ///
    /// Returns five tiers of facts (mutually exclusive):
    /// 1. **Pinned** — all pinned facts (cross-scope)
    /// 2. **High-importance** — top-N by materialized importance_score
    /// 3. **Due** — facts with t_valid <= now
    /// 4. **Recent** — most recent, from scope ancestors
    /// 5. **KB stubs** — placeholder for Phase 5
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the requested scope path doesn't exist.
    pub fn resume_context(&self, config: &ResumeConfig) -> Result<ResumeContext> {
        // Step 1: Resolve scope IDs from cache (short-lived read lock)
        let scope_ids = {
            let tree = self.scope_tree.read();
            let root = tree.root_id();
            match config.scope_path.as_ref() {
                Some(path) => {
                    let id = tree
                        .resolve_path(path)
                        .ok_or_else(|| MemoryError::NotFound(format!("scope path: {path}")))?;
                    tree.ancestors(id)
                }
                None => vec![root],
            }
        }; // scope_tree read lock dropped here

        // Step 2: Query DB on read connection
        let mut ctx = self.with_read(|conn| {
            crate::resume::resume_context(conn, &scope_ids, self.embed_dim, config)
        })?;

        // Step 3: Stamp surfaced_at on ALL due facts across ALL tiers (#93).
        // A fact is "due" if t_valid <= now AND not bi-temporally invalidated,
        // regardless of which tier claimed it. Matches FactStore::list_due() predicate.
        // Must use write_conn — read connections have query_only = ON.
        let is_unsurfaced_due = |f: &Fact| -> bool {
            f.surfaced_at.is_none()
                && f.t_valid.is_some_and(|tv| tv <= config.now)
                && f.t_invalid.is_none_or(|ti| ti > config.now)
        };
        let unsurfaced_ids: Vec<i64> = ctx
            .pinned
            .iter()
            .chain(ctx.high_importance.iter())
            .chain(ctx.due.iter())
            .chain(ctx.recent.iter())
            .filter(|f| is_unsurfaced_due(f))
            .map(|f| f.id)
            .collect();

        if !unsurfaced_ids.is_empty() {
            let stamped = self.stamp_surfaced_facts(&unsurfaced_ids, config.now)?;
            apply_surfaced_stamps(
                ctx.pinned
                    .iter_mut()
                    .chain(ctx.high_importance.iter_mut())
                    .chain(ctx.due.iter_mut())
                    .chain(ctx.recent.iter_mut()),
                &stamped,
            );
        }

        Ok(ctx)
    }
}

/// Apply DB-authoritative `surfaced_at` timestamps to in-memory facts.
fn apply_surfaced_stamps<'a>(
    facts: impl Iterator<Item = &'a mut Fact>,
    stamped_map: &std::collections::HashMap<i64, DateTime<Utc>>,
) {
    for fact in facts {
        if let Some(&ts) = stamped_map.get(&fact.id) {
            fact.surfaced_at = Some(ts);
        }
    }
}

/// Check if a fact's validity window passes the temporal cutoff.
/// A fact passes if it's valid at the cutoff instant:
/// `(t_valid IS NULL OR t_valid <= cutoff) AND (t_invalid IS NULL OR t_invalid > cutoff)`
fn passes_temporal_cutoff(fact: &Fact, cutoff: DateTime<Utc>) -> bool {
    if let Some(t_valid) = fact.t_valid {
        if t_valid > cutoff {
            return false;
        }
    }
    if let Some(t_invalid) = fact.t_invalid {
        if t_invalid <= cutoff {
            return false;
        }
    }
    true
}

/// Check if a fact's `[t_valid, t_invalid)` interval overlaps `[start, end)`.
fn fact_overlaps_period(fact: &Fact, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    // (t_valid IS NULL OR t_valid < end) AND (t_invalid IS NULL OR t_invalid > start)
    fact.t_valid.is_none_or(|tv| tv < end) && fact.t_invalid.is_none_or(|ti| ti > start)
}

/// Wrap a `Fact` into a `SearchResult` with `MatchType::ImportanceRank`.
fn fact_to_search_result(fact: Fact) -> SearchResult {
    SearchResult {
        score: fact.importance_score,
        match_type: MatchType::ImportanceRank,
        fact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::hybrid::SearchMode;
    use crate::traits::{ConsolidationConfig, CrudDecision, ForgetPolicy, PersistenceClassifier};
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
            is_pinned: false,
        }
    }

    /// Test helper: insert a raw fact via the write connection (bypasses engine's `add_fact`).
    fn insert_raw_fact(engine: &MemoryEngine, fact: &NewFact) -> i64 {
        let conn = engine.pool.write();
        FactStore::new(&conn, engine.embed_dim)
            .insert(fact)
            .unwrap()
    }

    // --- Phase 1 tests ---

    #[test]
    fn open_memory_succeeds() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        assert_eq!(engine.embed_dim(), DIM);
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
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
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
                None,
            )
            .unwrap();

        let query = SearchQuery {
            text: Some("Rust".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        };
        let results = engine.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].fact.content.contains("Rust"));
    }

    #[test]
    fn embed_dim_validation_rejects_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let config_768 = EngineConfig::new(db_path.clone(), 768);
        let config_384 = EngineConfig::new(db_path, 384);

        // First open with dim=768
        {
            let _engine = MemoryEngine::open(&config_768).unwrap();
        }

        // Second open with dim=384 should fail
        let err = MemoryEngine::open(&config_384).unwrap_err();
        assert!(matches!(err, MemoryError::Migration(_)));
        assert!(err.to_string().contains("mismatch"));
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
        assert_eq!(engine.graph_stats(), (0, 0));
    }

    #[test]
    fn consolidate_deduplicates_similar_facts() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        // Two near-identical embeddings
        insert_raw_fact(
            &engine,
            &make_new_fact("fact alpha", vec![1.0, 0.0, 0.0, 0.0]),
        );
        insert_raw_fact(
            &engine,
            &make_new_fact("fact alpha copy", vec![0.99, 0.01, 0.0, 0.0]),
        );

        let config = ConsolidationConfig {
            dedup_threshold: 0.90,
            min_cluster_size: 10, // high threshold so no clusters form
        };
        let stats = engine.consolidate(&MockGen, &config).unwrap();
        assert_eq!(stats.duplicates_removed, 1);

        let active = engine.list_active_facts(None).unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn consolidate_is_idempotent() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        insert_raw_fact(
            &engine,
            &make_new_fact("unique A", vec![1.0, 0.0, 0.0, 0.0]),
        );
        insert_raw_fact(
            &engine,
            &make_new_fact("unique B", vec![0.0, 1.0, 0.0, 0.0]),
        );

        let config = ConsolidationConfig {
            dedup_threshold: 0.92,
            min_cluster_size: 10,
        };

        let _stats1 = engine.consolidate(&MockGen, &config).unwrap();
        let stats2 = engine.consolidate(&MockGen, &config).unwrap();

        // Second run should find 0 new duplicates
        assert_eq!(stats2.duplicates_removed, 0);
        // Both facts still active
        assert_eq!(engine.list_active_facts(None).unwrap().len(), 2);
    }

    #[test]
    fn forget_prunes_stale_facts() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();

        // Insert a fact with very low importance
        let now = Utc::now();
        let old_time = now - chrono::Duration::days(200);
        insert_raw_fact(
            &engine,
            &NewFact {
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
                is_pinned: false,
            },
        );

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
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let policy = ForgetPolicy {
            half_life_days: 0.0, // invalid
            ..ForgetPolicy::default()
        };
        assert!(engine.forget(&policy).is_err());
    }

    #[test]
    fn resolve_conflict_update_creates_edge() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let old_id = insert_raw_fact(&engine, &make_new_fact("outdated", vec![0.5; DIM]));

        let arbiter = FixedArbiter {
            decision: CrudDecision::Update,
        };
        let result = engine
            .resolve_conflict(&arbiter, old_id, &make_new_fact("updated", vec![0.5; DIM]))
            .unwrap();

        assert_eq!(result.decision, CrudDecision::Update);
        assert!(result.new_fact_id.is_some());

        // Old fact should be expired
        let old = engine.get_fact(old_id).unwrap();
        assert!(old.t_expired.is_some());

        // Graph should have the new edge
        let new_id = result.new_fact_id.unwrap();
        assert!(engine.graph_has_node(new_id));
        assert_eq!(engine.graph_neighbors(new_id), vec![old_id]);
    }

    #[test]
    fn resolve_conflict_noop_no_changes() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let old_id = insert_raw_fact(&engine, &make_new_fact("existing", vec![0.5; DIM]));

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
        let old = engine.get_fact(old_id).unwrap();
        assert!(old.t_expired.is_none());
    }

    #[test]
    fn graph_loads_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let config = EngineConfig::new(db_path, DIM);

        // First session: add facts and create an edge via conflict resolution
        {
            let engine = MemoryEngine::open(&config).unwrap();
            let old_id = insert_raw_fact(&engine, &make_new_fact("original", vec![0.5; DIM]));
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
            assert_eq!(engine.graph_stats().1, 1);
        }

        // Second session: graph should be restored from DB
        {
            let engine = MemoryEngine::open(&config).unwrap();
            assert_eq!(engine.graph_stats().1, 1);
        }
    }

    #[test]
    fn list_summaries_empty() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let summaries = engine.list_summaries(&ConsolidationLevel::Global).unwrap();
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
                None,
            )
            .unwrap();
        let fact = engine.get_fact(id).unwrap();
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
                None,
            )
            .unwrap();
        let fact = engine.get_fact(id).unwrap();
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
                None,
            )
            .unwrap();
        let fact = engine.get_fact(id).unwrap();
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
                None,
            )
            .unwrap();
        let fact = engine.get_fact(id).unwrap();
        assert!((fact.importance - 0.5).abs() < f64::EPSILON);
        assert!(fact.t_valid.is_none());
    }

    // --- Phase 3 / T7: Send + Sync ---

    #[test]
    fn engine_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemoryEngine>();
    }

    #[test]
    fn engine_concurrent_reads() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("concurrent.db");
        let config = EngineConfig::new(db_path, DIM);

        let engine = std::sync::Arc::new(MemoryEngine::open(&config).unwrap());
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "Rust is fast",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "Python is flexible",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let mut handles = vec![];
        for _ in 0..4 {
            let e = engine.clone();
            handles.push(std::thread::spawn(move || {
                let results = e
                    .query(&SearchQuery {
                        text: Some("Rust".into()),
                        embedding: None,
                        mode: SearchMode::Fts,
                        limit: 10,
                        rerank_depth: None,
                        valid_at: None,
                        fact_type: None,
                        scope: None,
                    })
                    .unwrap();
                assert_eq!(results.len(), 1);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn engine_write_then_read_across_threads() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("write_read.db");
        let config = EngineConfig::new(db_path, DIM);

        let engine = std::sync::Arc::new(MemoryEngine::open(&config).unwrap());

        // Thread 1: write
        let e1 = engine.clone();
        let writer = std::thread::spawn(move || {
            let embedder = MockEmbedder { dim: DIM };
            e1.add_fact(
                "Concurrent write test",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        });
        writer.join().unwrap();

        // Thread 2: read (after write completes)
        let reader = std::thread::spawn(move || {
            let results = engine
                .query(&SearchQuery {
                    text: Some("Concurrent".into()),
                    embedding: None,
                    mode: SearchMode::Fts,
                    limit: 10,
                    rerank_depth: None,
                    valid_at: None,
                    fact_type: None,
                    scope: None,
                })
                .unwrap();
            assert_eq!(results.len(), 1);
        });
        reader.join().unwrap();
    }

    // --- Phase 3 / T9: resume_context ---

    #[test]
    fn resume_empty_engine() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let ctx = engine.resume_context(&ResumeConfig::default()).unwrap();
        assert!(ctx.pinned.is_empty());
        assert!(ctx.high_importance.is_empty());
        assert!(ctx.due.is_empty());
        assert!(ctx.recent.is_empty());
    }

    #[test]
    fn resume_with_facts() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        // Add a pinned fact (appears in tier 1)
        let opts_pinned = AddFactOptions {
            importance: Some(0.95),
            pinned: Some(true),
            ..Default::default()
        };
        engine
            .add_fact(
                "user prefers Rust",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&opts_pinned),
                None,
            )
            .unwrap();

        // Add a low-importance root fact (recent tier)
        let opts_low = AddFactOptions {
            importance: Some(0.1),
            ..Default::default()
        };
        engine
            .add_fact(
                "had coffee today",
                FactType::Episodic,
                None,
                &embedder,
                None,
                Some(&opts_low),
                None,
            )
            .unwrap();

        let config = ResumeConfig::default();
        let ctx = engine.resume_context(&config).unwrap();
        // The pinned fact should appear in the pinned tier
        assert_eq!(ctx.pinned.len(), 1);
        assert!(ctx.pinned[0].is_pinned);
        assert!(ctx.pinned[0].content.contains("Rust"));
        // The low-importance fact should appear in recent
        assert!(!ctx.recent.is_empty());
    }

    #[test]
    fn resume_nonexistent_scope_returns_not_found() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let config = ResumeConfig {
            scope_path: Some("nonexistent/path".into()),
            ..ResumeConfig::default()
        };
        let err = engine.resume_context(&config).unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));
    }

    // --- Issue #93: surfaced_at for due facts in non-due tiers ---

    #[test]
    fn resume_stamps_surfaced_at_on_pinned_due_fact() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let now = Utc::now();
        let past = now - chrono::Duration::hours(1);

        // Pinned fact that is ALSO due (t_valid in the past).
        // It will land in the pinned tier, not the due tier.
        engine
            .add_fact(
                "CI pipeline uses 30min timeout",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&AddFactOptions {
                    pinned: Some(true),
                    t_valid: Some(past),
                    importance: Some(0.9),
                    ..Default::default()
                }),
                None,
            )
            .unwrap();

        let config = ResumeConfig {
            now,
            ..ResumeConfig::default()
        };
        let ctx = engine.resume_context(&config).unwrap();

        // Fact should appear in pinned tier (not due tier)
        assert_eq!(ctx.pinned.len(), 1);
        assert!(ctx.due.is_empty() || !ctx.due.iter().any(|f| f.content.contains("CI")));

        // Bug: surfaced_at should be stamped because the fact IS due,
        // even though it landed in the pinned tier.
        assert!(
            ctx.pinned[0].surfaced_at.is_some(),
            "pinned-but-due fact must have surfaced_at stamped"
        );
    }

    #[test]
    fn resume_stamps_surfaced_at_on_high_importance_due_fact() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let now = Utc::now();
        let past = now - chrono::Duration::hours(1);

        // High-importance fact that is ALSO due. Not pinned.
        // importance=0.9 → importance_score=0.9, which exceeds the 0.7 threshold.
        // It will land in high_importance tier, not due tier.
        engine
            .add_fact(
                "user prefers tabs over spaces",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&AddFactOptions {
                    importance: Some(0.9),
                    t_valid: Some(past),
                    ..Default::default()
                }),
                None,
            )
            .unwrap();

        let config = ResumeConfig {
            now,
            high_importance_min: 0.7,
            ..ResumeConfig::default()
        };
        let ctx = engine.resume_context(&config).unwrap();

        // Fact should appear in high_importance tier (not due tier)
        assert_eq!(ctx.high_importance.len(), 1);
        assert!(ctx.due.is_empty());

        // Bug: surfaced_at should be stamped because the fact IS due.
        assert!(
            ctx.high_importance[0].surfaced_at.is_some(),
            "high-importance-but-due fact must have surfaced_at stamped"
        );
    }

    #[test]
    fn resume_does_not_stamp_invalidated_pinned_due_fact() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let now = Utc::now();
        let past = now - chrono::Duration::hours(2);
        let past_invalid = now - chrono::Duration::hours(1);

        // Pinned fact that WAS due but is now bi-temporally invalidated:
        // t_valid in the past, t_invalid ALSO in the past (before now).
        engine
            .add_fact(
                "old CI timeout no longer valid",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&AddFactOptions {
                    pinned: Some(true),
                    t_valid: Some(past),
                    t_invalid: Some(past_invalid),
                    importance: Some(0.9),
                    ..Default::default()
                }),
                None,
            )
            .unwrap();

        let config = ResumeConfig {
            now,
            ..ResumeConfig::default()
        };
        let ctx = engine.resume_context(&config).unwrap();

        // Fact lands in pinned tier (it's pinned and not expired)
        assert_eq!(ctx.pinned.len(), 1);

        // But surfaced_at must NOT be stamped — the fact is bi-temporally
        // invalidated (t_invalid <= now), so it's no longer "due".
        assert!(
            ctx.pinned[0].surfaced_at.is_none(),
            "invalidated fact must not have surfaced_at stamped"
        );
    }

    // --- Phase 3b / T6: SearchConfig in EngineConfig ---

    #[test]
    fn engine_config_default_has_no_search_config() {
        let config = EngineConfig::new("test.db".into(), 128);
        assert!(config.search_config.is_none());
    }

    #[test]
    fn engine_config_with_search_config() {
        let mut config = EngineConfig::new("test.db".into(), 128);
        config.search_config = Some(SearchConfig::default());
        assert_eq!(config.search_config.unwrap().ann_threshold, 50_000);
    }

    #[test]
    fn query_nonexistent_scope_returns_empty() {
        use crate::types::ScopeQuery;

        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        // Add a fact at root scope so there's something to find if search were unscoped
        engine
            .add_fact(
                "visible without scope",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        // Query with a scope path that doesn't exist
        let query = SearchQuery {
            text: Some("visible".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: Some(ScopeQuery::Exact("nonexistent/scope".into())),
        };
        let results = engine.query(&query).unwrap();
        assert!(
            results.is_empty(),
            "expected empty results for nonexistent scope, got {}",
            results.len()
        );
    }

    // --- Phase 3b / T8: Engine facade new methods ---

    #[test]
    fn list_due_returns_scheduled_facts() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let past = Utc::now() - chrono::Duration::hours(1);
        let future = Utc::now() + chrono::Duration::hours(1);

        // Past-due fact
        engine
            .add_fact(
                "check release",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&AddFactOptions {
                    t_valid: Some(past),
                    ..Default::default()
                }),
                None,
            )
            .unwrap();

        // Future fact
        engine
            .add_fact(
                "future check",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&AddFactOptions {
                    t_valid: Some(future),
                    ..Default::default()
                }),
                None,
            )
            .unwrap();

        // Regular fact (no t_valid)
        engine
            .add_fact(
                "regular",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let due = engine.list_due(Utc::now(), None).unwrap();
        assert_eq!(due.len(), 1);
        assert!(due[0].content.contains("check release"));

        let next = engine.next_due_time(None).unwrap();
        assert!(next.is_some()); // the future fact

        // Future-dated facts should be invisible to regular search (no valid_at)
        let search = engine
            .query(&SearchQuery {
                text: Some("future check".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();
        assert!(
            search.is_empty(),
            "future-dated facts should not appear in regular search"
        );

        // But past-due facts should be visible
        let search2 = engine
            .query(&SearchQuery {
                text: Some("check release".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();
        assert_eq!(search2.len(), 1, "past-due facts should appear in search");
    }

    #[test]
    fn pin_unpin_fact() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let id = engine
            .add_fact(
                "pinnable",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        assert!(!engine.get_fact(id).unwrap().is_pinned);
        engine.pin_fact(id).unwrap();
        assert!(engine.get_fact(id).unwrap().is_pinned);
        engine.unpin_fact(id).unwrap();
        assert!(!engine.get_fact(id).unwrap().is_pinned);
    }

    #[test]
    fn add_fact_with_explicit_pin() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let opts = AddFactOptions {
            pinned: Some(true),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                "identity",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&opts),
                None,
            )
            .unwrap();
        assert!(engine.get_fact(id).unwrap().is_pinned);
    }

    #[test]
    fn add_fact_with_classifier() {
        struct PinSemantic;
        impl PersistenceClassifier for PinSemantic {
            fn should_pin(&self, fact: &Fact) -> bool {
                fact.fact_type == FactType::Semantic
            }
        }

        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let classifier = PinSemantic;

        let id = engine
            .add_fact(
                "auto-pinned",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                Some(&classifier),
            )
            .unwrap();
        assert!(engine.get_fact(id).unwrap().is_pinned);

        let id2 = engine
            .add_fact(
                "not pinned",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                Some(&classifier),
            )
            .unwrap();
        assert!(!engine.get_fact(id2).unwrap().is_pinned);
    }

    #[test]
    fn explicit_pin_overrides_classifier() {
        struct AlwaysPin;
        impl PersistenceClassifier for AlwaysPin {
            fn should_pin(&self, _fact: &Fact) -> bool {
                true
            }
        }

        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let classifier = AlwaysPin;

        // Explicitly set pinned=false — should override the classifier
        let opts = AddFactOptions {
            pinned: Some(false),
            ..Default::default()
        };
        let id = engine
            .add_fact(
                "not pinned despite classifier",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&opts),
                Some(&classifier),
            )
            .unwrap();
        assert!(!engine.get_fact(id).unwrap().is_pinned);
    }

    // --- execute_query integration tests ---

    #[test]
    fn execute_query_empty_returns_active_facts() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "fact one",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "fact two",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let results = engine.execute_query(&MemoryQuery::new()).unwrap().results;
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.match_type == MatchType::ImportanceRank));
    }

    #[test]
    fn execute_query_text_search() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "Rust systems programming",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "Python machine learning",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let results = engine
            .execute_query(&MemoryQuery::new().text("Rust"))
            .unwrap()
            .results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "Rust systems programming");
        assert_eq!(results[0].match_type, MatchType::Fts);
    }

    #[test]
    fn execute_query_scope_only() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        // Add fact to "project:demo" scope (auto-created by add_fact)
        engine
            .add_fact(
                "scoped fact",
                FactType::Episodic,
                None,
                &embedder,
                Some("project:demo"),
                None,
                None,
            )
            .unwrap();
        // Add fact to root scope
        engine
            .add_fact(
                "root fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let results = engine
            .execute_query(&MemoryQuery::new().scope_exact("project:demo"))
            .unwrap()
            .results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "scoped fact");
    }

    #[test]
    fn execute_query_fact_type_filter() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "episodic",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "semantic",
                FactType::Semantic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let results = engine
            .execute_query(&MemoryQuery::new().fact_type(FactType::Semantic))
            .unwrap()
            .results;
        // fact_type filtering in store path — list_by_importance_score doesn't filter by fact_type,
        // so it should be post-filtered
        assert!(results
            .iter()
            .all(|r| r.fact.fact_type == FactType::Semantic));
    }

    #[test]
    fn execute_query_importance_threshold() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        let opts_low = AddFactOptions {
            importance: Some(0.1),
            ..Default::default()
        };
        let opts_high = AddFactOptions {
            importance: Some(0.9),
            ..Default::default()
        };
        engine
            .add_fact(
                "low importance",
                FactType::Episodic,
                None,
                &embedder,
                None,
                Some(&opts_low),
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "high importance",
                FactType::Episodic,
                None,
                &embedder,
                None,
                Some(&opts_high),
                None,
            )
            .unwrap();

        let results = engine
            .execute_query(&MemoryQuery::new().min_importance_score(0.5))
            .unwrap()
            .results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "high importance");
    }

    #[test]
    fn execute_query_pinned_only() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        let id = engine
            .add_fact(
                "pinned",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine.pin_fact(id).unwrap();

        engine
            .add_fact(
                "normal",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let results = engine
            .execute_query(&MemoryQuery::new().pinned_only())
            .unwrap()
            .results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "pinned");
        assert!(results[0].fact.is_pinned);
    }

    #[test]
    fn execute_query_future_dated_excluded() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        // Regular fact
        engine
            .add_fact(
                "present fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        // Future-dated fact
        let future_opts = AddFactOptions {
            t_valid: Some(Utc::now() + chrono::Duration::hours(24)),
            ..Default::default()
        };
        engine
            .add_fact(
                "future fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                Some(&future_opts),
                None,
            )
            .unwrap();

        // Empty query should NOT return the future-dated fact
        let results = engine.execute_query(&MemoryQuery::new()).unwrap().results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "present fact");

        // Scope-only query should also exclude future-dated facts
        let results2 = engine
            .execute_query(&MemoryQuery::new().min_importance_score(0.0))
            .unwrap()
            .results;
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].fact.content, "present fact");
    }

    #[test]
    fn execute_query_period_mutual_exclusion() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let now = Utc::now();

        let result = engine.execute_query(
            &MemoryQuery::new()
                .valid_at(now)
                .period(now - chrono::Duration::hours(1), now),
        );
        assert!(result.is_err());
    }

    #[test]
    fn execute_query_search_mode_conflict() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();

        // Hybrid requires both text and embedding
        let result = engine.execute_query(
            &MemoryQuery::new()
                .text("test")
                .search_mode(SearchMode::Hybrid),
        );
        assert!(result.is_err());
    }

    #[test]
    fn execute_query_search_mode_inference() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "Rust programming",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        // Text-only → should infer FTS mode
        let results = engine
            .execute_query(&MemoryQuery::new().text("Rust"))
            .unwrap()
            .results;
        assert!(!results.is_empty());
        assert_eq!(results[0].match_type, MatchType::Fts);
    }

    #[test]
    fn execute_query_period_filter() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let now = Utc::now();

        // Fact valid in the past
        let past_opts = AddFactOptions {
            t_valid: Some(now - chrono::Duration::hours(3)),
            t_invalid: Some(now - chrono::Duration::hours(1)),
            ..Default::default()
        };
        engine
            .add_fact(
                "past fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                Some(&past_opts),
                None,
            )
            .unwrap();

        // Fact still valid
        engine
            .add_fact(
                "current fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        // Period query covering only the past window
        let results = engine
            .execute_query(&MemoryQuery::new().period(
                now - chrono::Duration::hours(4),
                now - chrono::Duration::minutes(30),
            ))
            .unwrap()
            .results;

        // Both should match: past fact has [t_valid, t_invalid) overlapping the period,
        // and current fact has NULL t_valid/t_invalid (unbounded, overlaps everything)
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn execute_query_composed_filters() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        let opts_high = AddFactOptions {
            importance: Some(0.9),
            ..Default::default()
        };
        let opts_low = AddFactOptions {
            importance: Some(0.1),
            ..Default::default()
        };

        engine
            .add_fact(
                "Rust high importance",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&opts_high),
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "Rust low importance",
                FactType::Semantic,
                None,
                &embedder,
                None,
                Some(&opts_low),
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "Python high importance",
                FactType::Episodic,
                None,
                &embedder,
                None,
                Some(&opts_high),
                None,
            )
            .unwrap();

        // Text "Rust" + importance >= 0.5 → only "Rust high importance"
        let results = engine
            .execute_query(&MemoryQuery::new().text("Rust").min_importance_score(0.5))
            .unwrap()
            .results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact.content, "Rust high importance");
    }

    #[test]
    fn execute_query_empty_results() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();

        let results = engine
            .execute_query(&MemoryQuery::new().text("nonexistent"))
            .unwrap()
            .results;
        assert!(results.is_empty());
    }

    #[test]
    fn execute_query_default_limit() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        // Add 60 facts (over the default limit of 50)
        for i in 0..60 {
            engine
                .add_fact(
                    &format!("fact {i}"),
                    FactType::Episodic,
                    None,
                    &embedder,
                    None,
                    None,
                    None,
                )
                .unwrap();
        }

        let results = engine.execute_query(&MemoryQuery::new()).unwrap().results;
        assert_eq!(results.len(), 50); // default limit
    }

    // --- Reranker tests ---

    struct ReverseReranker;
    impl Reranker for ReverseReranker {
        fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            let n = candidates.len();
            Ok((0..n).rev().map(|i| (i, candidates[i].score)).collect())
        }
        fn name(&self) -> &str {
            "reverse"
        }
    }

    struct FailingReranker;
    impl Reranker for FailingReranker {
        fn rerank(&self, _query: &str, _candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            Err(MemoryError::Reranker("cross-encoder timeout".into()))
        }
        fn name(&self) -> &str {
            "failing"
        }
    }

    /// Records how many candidates it received, then passes through.
    struct SpyReranker {
        seen_count: std::sync::atomic::AtomicUsize,
    }
    impl SpyReranker {
        fn new() -> Self {
            Self {
                seen_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn seen(&self) -> usize {
            self.seen_count.load(std::sync::atomic::Ordering::Relaxed)
        }
    }
    impl Reranker for SpyReranker {
        fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            self.seen_count
                .store(candidates.len(), std::sync::atomic::Ordering::Relaxed);
            Ok((0..candidates.len())
                .map(|i| (i, candidates[i].score))
                .collect())
        }
        fn name(&self) -> &str {
            "spy"
        }
    }

    #[test]
    fn reranker_none_results_unchanged() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "alpha fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "beta fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let results = engine
            .query(&SearchQuery {
                text: Some("fact".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn reranker_reverses_order() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "alpha fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "beta fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        // Get baseline order (no reranker)
        let baseline_engine = MemoryEngine::open_memory(DIM).unwrap();
        baseline_engine
            .add_fact(
                "alpha fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        baseline_engine
            .add_fact(
                "beta fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let baseline = baseline_engine
            .query(&SearchQuery {
                text: Some("fact".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        let reranked = engine
            .query(&SearchQuery {
                text: Some("fact".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        assert_eq!(baseline.len(), reranked.len());
        assert_eq!(baseline.len(), 2);
        // Reversed order
        assert_eq!(baseline[0].fact.content, reranked[1].fact.content);
        assert_eq!(baseline[1].fact.content, reranked[0].fact.content);
    }

    #[test]
    fn reranker_skipped_for_vector_only_no_text() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "alpha",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "beta",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let results = engine
            .query(&SearchQuery {
                text: None,
                embedding: Some(vec![0.5; DIM]),
                mode: SearchMode::Vector,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        // Reranker should NOT have fired (no text) — order should match vector similarity.
        // Both have identical embeddings, so they're equivalent; just check we got results.
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn reranker_applies_to_fts_only_mode() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "alpha fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "beta fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        // FTS-only with text → reranker should fire
        let results = engine
            .query(&SearchQuery {
                text: Some("fact".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        assert_eq!(results.len(), 2);
        // Results are reversed by ReverseReranker
    }

    #[test]
    fn reranker_applies_to_vector_mode_with_text() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "alpha",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "beta",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        // Vector mode WITH text → reranker should fire
        let results = engine
            .query(&SearchQuery {
                text: Some("alpha".into()),
                embedding: Some(vec![0.5; DIM]),
                mode: SearchMode::Vector,
                limit: 10,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        // Should still get results (vector search ignores text, but reranker fires)
        assert!(!results.is_empty());
    }

    #[test]
    fn rerank_depth_overfetches_then_truncates() {
        let spy = std::sync::Arc::new(SpyReranker::new());
        // Clone Arc into Box<dyn Reranker> — SpyReranker is Send+Sync
        let engine = MemoryEngine::open_memory_with(
            DIM,
            None,
            Some(Box::new(SpyRerankerWrapper(spy.clone()))),
        )
        .unwrap();
        let embedder = MockEmbedder { dim: DIM };

        // Insert 10 facts
        for i in 0..10 {
            engine
                .add_fact(
                    &format!("rerank test fact {i}"),
                    FactType::Episodic,
                    None,
                    &embedder,
                    None,
                    None,
                    None,
                )
                .unwrap();
        }

        let results = engine
            .query(&SearchQuery {
                text: Some("rerank test fact".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 3,
                rerank_depth: Some(8),
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        // Reranker should have seen up to 8 candidates
        assert!(
            spy.seen() > 3,
            "reranker should see more than limit (saw {})",
            spy.seen()
        );
        assert!(
            spy.seen() <= 8,
            "reranker should see at most rerank_depth (saw {})",
            spy.seen()
        );
        // But final output truncated to limit
        assert!(results.len() <= 3);
    }

    /// Wrapper to allow `Arc<SpyReranker>` to be `Box<dyn Reranker>`.
    struct SpyRerankerWrapper(std::sync::Arc<SpyReranker>);
    impl Reranker for SpyRerankerWrapper {
        fn rerank(&self, query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            self.0.rerank(query, candidates)
        }
        fn name(&self) -> &str {
            "spy_wrapper"
        }
    }

    #[test]
    fn reranker_error_propagates() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(FailingReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "test fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let result = engine.query(&SearchQuery {
            text: Some("test".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        });

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MemoryError::Reranker(_)));
    }

    #[test]
    fn reranker_name_accessor() {
        let engine_none = MemoryEngine::open_memory(DIM).unwrap();
        assert_eq!(engine_none.reranker_name(), None);

        let engine_some =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
        assert_eq!(engine_some.reranker_name(), Some("reverse"));
    }

    #[test]
    fn debug_output_includes_reranker() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
        let debug = format!("{engine:?}");
        assert!(
            debug.contains("reverse"),
            "Debug output should include reranker name"
        );
    }

    #[test]
    fn rerank_depth_none_falls_back_to_limit() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        for i in 0..10 {
            engine
                .add_fact(
                    &format!("limit test fact {i}"),
                    FactType::Episodic,
                    None,
                    &embedder,
                    None,
                    None,
                    None,
                )
                .unwrap();
        }

        let results = engine
            .query(&SearchQuery {
                text: Some("limit test fact".into()),
                embedding: None,
                mode: SearchMode::Fts,
                limit: 5,
                rerank_depth: None,
                valid_at: None,
                fact_type: None,
                scope: None,
            })
            .unwrap();

        assert_eq!(
            results.len(),
            5,
            "should respect limit when rerank_depth is None"
        );
    }

    // --- Co-session edge tests ---

    /// Helper: ingest an event with a `session_id` and add a fact linked to it.
    fn add_session_fact(engine: &MemoryEngine, content: &str, session_id: &str) -> (i64, i64) {
        let embedder = MockEmbedder { dim: DIM };
        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"msg": content}),
            source: "test".into(),
            session_id: Some(session_id.into()),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };
        let event_id = engine.ingest(&event).unwrap();
        let fact_id = engine
            .add_fact(
                content,
                FactType::Semantic,
                Some(event_id),
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        (event_id, fact_id)
    }

    #[test]
    fn link_session_facts_creates_bidirectional_edges() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let (_, f1) = add_session_fact(&engine, "fact a", "s1");
        let (_, f2) = add_session_fact(&engine, "fact b", "s1");

        let created = engine.link_session_facts("s1", None).unwrap();
        assert_eq!(created, 2); // A→B and B→A

        // Verify edges in DB
        let co_edges = {
            let conn = engine.pool.read();
            let edges = crate::store::edges::EdgeStore::new(&conn)
                .list_active()
                .unwrap();
            edges
                .into_iter()
                .filter(|e| e.relation_type == "co_session")
                .collect::<Vec<_>>()
        };
        assert_eq!(co_edges.len(), 2);

        // Both directions present
        assert!(co_edges
            .iter()
            .any(|e| e.source_fact_id == f1 && e.target_fact_id == f2));
        assert!(co_edges
            .iter()
            .any(|e| e.source_fact_id == f2 && e.target_fact_id == f1));

        // Weight matches constant
        for e in &co_edges {
            assert!((e.weight - MemoryEngine::CO_SESSION_WEIGHT).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn link_session_facts_three_facts_six_edges() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        add_session_fact(&engine, "a", "s1");
        add_session_fact(&engine, "b", "s1");
        add_session_fact(&engine, "c", "s1");

        let created = engine.link_session_facts("s1", None).unwrap();
        assert_eq!(created, 6); // 3 pairs × 2 directions
    }

    #[test]
    fn link_session_facts_single_fact_noop() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        add_session_fact(&engine, "lonely", "s1");

        let created = engine.link_session_facts("s1", None).unwrap();
        assert_eq!(created, 0);
    }

    #[test]
    fn link_session_facts_empty_session_noop() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let created = engine.link_session_facts("nonexistent", None).unwrap();
        assert_eq!(created, 0);
    }

    #[test]
    fn link_session_facts_idempotent() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        add_session_fact(&engine, "a", "s1");
        add_session_fact(&engine, "b", "s1");

        let first = engine.link_session_facts("s1", None).unwrap();
        assert_eq!(first, 2);

        let second = engine.link_session_facts("s1", None).unwrap();
        assert_eq!(second, 0); // no new edges

        // Total edge count unchanged
        let (_, edge_count) = engine.graph_stats();
        assert_eq!(edge_count, 2);
    }

    #[test]
    fn link_session_facts_graph_degree() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let (_, f1) = add_session_fact(&engine, "a", "s1");
        let (_, f2) = add_session_fact(&engine, "b", "s1");
        let (_, f3) = add_session_fact(&engine, "c", "s1");

        // Before linking — no edges
        assert_eq!(engine.graph_degree(f1), 0);

        engine.link_session_facts("s1", None).unwrap();

        // After: each fact has 2 outgoing + 2 incoming = degree 4
        assert_eq!(engine.graph_degree(f1), 4);
        assert_eq!(engine.graph_degree(f2), 4);
        assert_eq!(engine.graph_degree(f3), 4);
    }

    #[test]
    fn link_session_facts_ignores_expired() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let (_, f1) = add_session_fact(&engine, "active1", "s1");
        add_session_fact(&engine, "active2", "s1");
        let (_, f3) = add_session_fact(&engine, "will_expire", "s1");

        // Expire f3 before linking
        {
            let conn = engine.pool.write();
            FactStore::new(&conn, DIM).expire(f3, Utc::now()).unwrap();
        }

        let created = engine.link_session_facts("s1", None).unwrap();
        assert_eq!(created, 2); // Only f1↔active2, not f3

        // f3 should have no edges
        assert_eq!(engine.graph_degree(f3), 0);
        assert_eq!(engine.graph_degree(f1), 2); // 1 out + 1 in
    }

    // --- Scope-aware session linking tests ---

    /// Helper: add a fact in a specific scope, linked to a session.
    fn add_scoped_session_fact(
        engine: &MemoryEngine,
        content: &str,
        session_id: &str,
        scope_path: &str,
    ) -> (i64, i64) {
        let embedder = MockEmbedder { dim: DIM };
        let event = NewEvent {
            timestamp: Utc::now(),
            event_type: EventType::Interaction,
            payload: serde_json::json!({"msg": content}),
            source: "test".into(),
            session_id: Some(session_id.into()),
            scope_id: 1,
            origin_node_id: "local".into(),
            sequence_id: 0,
            created_at: None,
        };
        let event_id = engine.ingest(&event).unwrap();
        let fact_id = engine
            .add_fact(
                content,
                FactType::Semantic,
                Some(event_id),
                &embedder,
                Some(scope_path),
                None,
                None,
            )
            .unwrap();
        (event_id, fact_id)
    }

    #[test]
    fn link_session_facts_scope_filters_cross_scope() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();

        // Two facts in user:alice, one in user:bob — same session_id
        let (_, f1) = add_scoped_session_fact(&engine, "alice a", "s1", "user:alice");
        let (_, f2) = add_scoped_session_fact(&engine, "alice b", "s1", "user:alice");
        let (_, f3) = add_scoped_session_fact(&engine, "bob c", "s1", "user:bob");

        // Scope-filtered: only link alice's facts
        let created = engine.link_session_facts("s1", Some("user:alice")).unwrap();
        assert_eq!(created, 2); // f1↔f2

        assert_eq!(engine.graph_degree(f1), 2); // 1 out + 1 in
        assert_eq!(engine.graph_degree(f2), 2);
        assert_eq!(engine.graph_degree(f3), 0); // bob excluded
    }

    #[test]
    fn link_session_facts_scope_none_links_all() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();

        add_scoped_session_fact(&engine, "alice a", "s1", "user:alice");
        add_scoped_session_fact(&engine, "bob b", "s1", "user:bob");
        add_scoped_session_fact(&engine, "root c", "s1", "user:charlie");

        // None = global lookup (backward-compatible)
        let created = engine.link_session_facts("s1", None).unwrap();
        assert_eq!(created, 6); // 3 facts × 2 directions
    }

    #[test]
    fn link_session_facts_scope_subtree() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();

        // Create facts at different depths under user:alice
        let (_, f1) = add_scoped_session_fact(&engine, "top", "s1", "user:alice");
        let (_, f2) = add_scoped_session_fact(&engine, "nested", "s1", "user:alice/project:x");
        let (_, f3) = add_scoped_session_fact(&engine, "other", "s1", "user:bob");

        // Subtree from user:alice should include both alice and alice/project:x
        let created = engine.link_session_facts("s1", Some("user:alice")).unwrap();
        assert_eq!(created, 2); // f1↔f2
        assert_eq!(engine.graph_degree(f1), 2);
        assert_eq!(engine.graph_degree(f2), 2);
        assert_eq!(engine.graph_degree(f3), 0);
    }

    #[test]
    fn link_session_facts_scope_not_found() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        add_session_fact(&engine, "a", "s1");

        let result = engine.link_session_facts("s1", Some("user:nonexistent"));
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), MemoryError::NotFound(msg) if msg.contains("scope path"))
        );
    }

    // --- Reranker subset/permutation guard (issue #85) ---

    /// Returns all candidates plus a fabricated fact with a bogus ID.
    /// Returns a single out-of-bounds index.
    struct OutOfBoundsReranker;
    impl Reranker for OutOfBoundsReranker {
        fn rerank(&self, _query: &str, _candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            Ok(vec![(999_999, 1.0)])
        }
        fn name(&self) -> &str {
            "out_of_bounds"
        }
    }

    /// Returns first two candidates with the same index (duplicate).
    struct DuplicatingReranker;
    impl Reranker for DuplicatingReranker {
        fn rerank(&self, _query: &str, candidates: &[SearchResult]) -> Result<Vec<(usize, f64)>> {
            if candidates.len() >= 2 {
                Ok(vec![(0, candidates[0].score), (0, candidates[0].score)])
            } else {
                Ok((0..candidates.len())
                    .map(|i| (i, candidates[i].score))
                    .collect())
            }
        }
        fn name(&self) -> &str {
            "duplicating"
        }
    }

    #[test]
    fn reranker_rejects_out_of_bounds_index() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(OutOfBoundsReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "real fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let result = engine.query(&SearchQuery {
            text: Some("real".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        });

        assert!(result.is_err(), "should reject out-of-bounds index");
        let err = result.unwrap_err();
        assert!(
            matches!(err, MemoryError::Reranker(_)),
            "should be a Reranker error, got: {err}"
        );
        assert!(
            err.to_string().contains("out-of-bounds"),
            "error message should mention out-of-bounds, got: {err}"
        );
    }

    #[test]
    fn reranker_rejects_duplicates() {
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(DuplicatingReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "dup fact alpha",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "dup fact beta",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let result = engine.query(&SearchQuery {
            text: Some("dup".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        });

        assert!(result.is_err(), "should reject duplicates");
        let err = result.unwrap_err();
        assert!(
            matches!(err, MemoryError::Reranker(_)),
            "should be a Reranker error, got: {err}"
        );
        assert!(
            err.to_string().contains("duplicate"),
            "error message should mention duplicate, got: {err}"
        );
    }

    #[test]
    fn reranker_allows_valid_subset() {
        // A well-behaved reranker (ReverseReranker) should still work fine
        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(ReverseReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "guard alpha",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "guard beta",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let result = engine.query(&SearchQuery {
            text: Some("guard".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        });

        assert!(
            result.is_ok(),
            "valid subset should pass: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn reranker_allows_filtering_subset() {
        /// Returns only the first candidate, discarding the rest.
        struct FilteringReranker;
        impl Reranker for FilteringReranker {
            fn rerank(
                &self,
                _query: &str,
                candidates: &[SearchResult],
            ) -> Result<Vec<(usize, f64)>> {
                if candidates.is_empty() {
                    Ok(vec![])
                } else {
                    Ok(vec![(0, candidates[0].score)])
                }
            }
            fn name(&self) -> &str {
                "filtering"
            }
        }

        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(FilteringReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "filterable first",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .add_fact(
                "filterable second",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let result = engine.query(&SearchQuery {
            text: Some("filterable".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        });

        assert!(
            result.is_ok(),
            "filtering subset should pass: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn reranker_rejects_non_finite_score() {
        struct NanScoreReranker;
        impl Reranker for NanScoreReranker {
            fn rerank(
                &self,
                _query: &str,
                candidates: &[SearchResult],
            ) -> Result<Vec<(usize, f64)>> {
                if candidates.is_empty() {
                    Ok(vec![])
                } else {
                    Ok(vec![(0, f64::NAN)])
                }
            }
            fn name(&self) -> &str {
                "nan_score"
            }
        }

        let engine =
            MemoryEngine::open_memory_with(DIM, None, Some(Box::new(NanScoreReranker))).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        engine
            .add_fact(
                "score test fact",
                FactType::Episodic,
                None,
                &embedder,
                None,
                None,
                None,
            )
            .unwrap();

        let result = engine.query(&SearchQuery {
            text: Some("score".into()),
            embedding: None,
            mode: SearchMode::Fts,
            limit: 10,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        });

        assert!(result.is_err(), "should reject NaN score");
        let err = result.unwrap_err();
        assert!(
            matches!(err, MemoryError::Reranker(_)),
            "should be a Reranker error, got: {err}"
        );
        assert!(
            err.to_string().contains("non-finite"),
            "error message should mention non-finite score, got: {err}"
        );
    }

    // --- Batch embedding + batch add_fact tests ---

    #[test]
    fn embed_batch_default_impl_loops_embed() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingEmbedder {
            calls: AtomicUsize,
            dim: usize,
        }

        impl EmbeddingProvider for CountingEmbedder {
            fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec![0.1; self.dim])
            }
        }

        let embedder = CountingEmbedder {
            calls: AtomicUsize::new(0),
            dim: DIM,
        };

        let texts = ["alpha", "beta", "gamma"];
        let result = embedder.embed_batch(&texts).unwrap();

        assert_eq!(result.len(), 3);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 3);
        for emb in &result {
            assert_eq!(emb.len(), DIM);
        }
    }

    #[test]
    fn embed_batch_empty_returns_empty() {
        let embedder = MockEmbedder { dim: DIM };
        let result = embedder.embed_batch(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn add_facts_batch_inserts_all_facts() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        let entries: Vec<BatchFactEntry> = (0..5)
            .map(|i| BatchFactEntry {
                content: format!("batch fact {i}"),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            })
            .collect();

        let ids = engine.add_facts_batch(&entries, &embedder, None).unwrap();
        assert_eq!(ids.len(), 5);

        // All IDs should be unique and positive
        let unique: std::collections::HashSet<i64> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 5);
        assert!(ids.iter().all(|&id| id > 0));

        // Verify facts are actually in the DB
        for (i, &id) in ids.iter().enumerate() {
            let fact = engine.get_fact(id).unwrap();
            assert_eq!(fact.content, format!("batch fact {i}"));
        }
    }

    #[test]
    fn add_facts_batch_empty_returns_empty() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        let ids = engine.add_facts_batch(&[], &embedder, None).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn add_facts_batch_with_scopes() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        let entries = vec![
            BatchFactEntry {
                content: "fact in project/a".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("project/a".into()),
                opts: None,
            },
            BatchFactEntry {
                content: "fact in project/b".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("project/b".into()),
                opts: None,
            },
            BatchFactEntry {
                content: "fact in root".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
        ];

        let ids = engine.add_facts_batch(&entries, &embedder, None).unwrap();
        assert_eq!(ids.len(), 3);

        // Verify scope assignments via fact retrieval
        let f0 = engine.get_fact(ids[0]).unwrap();
        let f2 = engine.get_fact(ids[2]).unwrap();
        // f0 should be in a non-root scope, f2 in root (scope_id=1)
        assert_ne!(f0.scope_id, f2.scope_id);
        assert_eq!(f2.scope_id, 1); // root scope
    }

    #[test]
    fn add_facts_batch_with_classifier() {
        struct AlwaysPin;
        impl PersistenceClassifier for AlwaysPin {
            fn should_pin(&self, _fact: &Fact) -> bool {
                true
            }
        }

        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };
        let classifier = AlwaysPin;

        let entries = vec![BatchFactEntry {
            content: "important fact".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        }];

        let ids = engine
            .add_facts_batch(&entries, &embedder, Some(&classifier))
            .unwrap();
        let fact = engine.get_fact(ids[0]).unwrap();
        assert!(fact.is_pinned);
    }

    #[test]
    fn add_facts_batch_rejects_embedding_count_mismatch() {
        /// Embedder that returns fewer embeddings than requested.
        struct BadBatchEmbedder;
        impl EmbeddingProvider for BadBatchEmbedder {
            fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                Ok(vec![0.5; DIM])
            }
            fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                // Always return exactly 1 embedding regardless of input
                Ok(vec![vec![0.5; DIM]])
            }
        }

        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let entries = vec![
            BatchFactEntry {
                content: "a".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            BatchFactEntry {
                content: "b".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
        ];

        let err = engine
            .add_facts_batch(&entries, &BadBatchEmbedder, None)
            .unwrap_err();
        assert!(
            err.to_string().contains("1 embeddings for 2 entries"),
            "expected count mismatch error, got: {err}"
        );
    }

    #[test]
    fn add_facts_batch_rollback_on_insert_failure() {
        /// Embedder that returns wrong dimension for the last embedding,
        /// causing FactStore::insert to fail mid-transaction.
        struct BadDimBatchEmbedder;
        impl EmbeddingProvider for BadDimBatchEmbedder {
            fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                Ok(vec![0.5; DIM])
            }
            fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                let mut results: Vec<Vec<f32>> = texts.iter().map(|_| vec![0.5; DIM]).collect();
                // Corrupt the last embedding with wrong dimension
                if let Some(last) = results.last_mut() {
                    *last = vec![0.5; DIM + 1]; // wrong dim
                }
                Ok(results)
            }
        }

        let engine = MemoryEngine::open_memory(DIM).unwrap();

        // Use scoped entries to verify scopes also roll back
        let entries = vec![
            BatchFactEntry {
                content: "rollback test 0".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("rollback/scope-a".into()),
                opts: None,
            },
            BatchFactEntry {
                content: "rollback test 1".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: Some("rollback/scope-b".into()),
                opts: None,
            },
            BatchFactEntry {
                content: "rollback test 2".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
        ];

        // Should fail on the last insert (wrong dim)
        let result = engine.add_facts_batch(&entries, &BadDimBatchEmbedder, None);
        assert!(result.is_err());

        // Verify rollback: no facts should be in the DB
        let query = SearchQuery {
            text: Some("rollback test".into()),
            embedding: Some(vec![0.5; DIM]),
            limit: 10,
            mode: SearchMode::Hybrid,
            rerank_depth: None,
            valid_at: None,
            fact_type: None,
            scope: None,
        };
        let results = engine.query(&query).unwrap();
        assert!(
            results.is_empty(),
            "expected no facts after rollback, got {}",
            results.len()
        );

        // Verify scope atomicity: scopes should NOT exist after rollback.
        // A successful add_facts_batch with the same scopes should create
        // them fresh (proving they were rolled back from the failed attempt).
        let good_entries = vec![BatchFactEntry {
            content: "after rollback".into(),
            fact_type: FactType::Semantic,
            source_event_id: None,
            scope: Some("rollback/scope-a".into()),
            opts: None,
        }];
        let good_embedder = MockEmbedder { dim: DIM };
        let ids = engine
            .add_facts_batch(&good_entries, &good_embedder, None)
            .unwrap();
        assert_eq!(ids.len(), 1);
        let fact = engine.get_fact(ids[0]).unwrap();
        // If scopes were leaked, scope_id would already exist.
        // The fact that this succeeds proves the DB is consistent.
        assert_ne!(fact.scope_id, 1, "should be in a non-root scope");
    }

    #[test]
    fn add_facts_batch_temporal_consistency() {
        let engine = MemoryEngine::open_memory(DIM).unwrap();
        let embedder = MockEmbedder { dim: DIM };

        let entries: Vec<BatchFactEntry> = (0..3)
            .map(|i| BatchFactEntry {
                content: format!("temporal {i}"),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            })
            .collect();

        let ids = engine.add_facts_batch(&entries, &embedder, None).unwrap();

        // All facts should have the same t_created (within a reasonable window)
        let facts: Vec<Fact> = ids.iter().map(|&id| engine.get_fact(id).unwrap()).collect();
        let first_created = facts[0].t_created;
        for fact in &facts {
            let diff = (fact.t_created - first_created).num_milliseconds().abs();
            assert!(
                diff == 0,
                "batch facts should share the same timestamp, diff: {diff}ms"
            );
        }
    }
}
