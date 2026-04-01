use std::path::PathBuf;

use chrono::{DateTime, Utc};
use parking_lot::{MutexGuard, RwLock};
use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::graph::MemoryGraph;
use crate::pool::ConnectionPool;
use crate::scope::ScopeTree;
use crate::search::hybrid::{MatchType, SearchResult};
use crate::search::strategy::{BruteForce, SearchConfig, VectorSearchStrategy};
use crate::store::facts::FactStore;
use crate::store::schema::{get_config, set_config};
use crate::store::scopes::ScopeStore;
use crate::store::summaries::SummaryStore;
use crate::store::upcaster::UpcasterRegistry;
use crate::traits::Reranker;
use crate::types::{ConsolidationLevel, Fact};

mod bootstrap;
mod conflict;
mod consolidation;
mod forgetting;
mod graph;
mod ingest;
mod inspect;
mod query;
mod restore;
mod resume;
mod scheduling;
pub(crate) mod snapshot;

/// Data loaded from a sidecar snapshot, ready to be assembled into a `MemoryEngine`.
struct SnapshotData {
    graph: MemoryGraph,
    scope_tree: ScopeTree,
    #[cfg(feature = "ann")]
    hnsw_strategy: Option<crate::search::ann::HnswStrategy>,
}

#[cfg(feature = "archive")]
mod archive;

#[cfg(feature = "archive")]
mod archive;

#[cfg(test)]
mod tests;

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
    ///
    /// Tries the snapshot fast path first (file-backed engines only). If the
    /// sidecar validates against the current DB fingerprint, loads from it.
    /// Otherwise falls back to full SQLite scan.
    fn init_from_pool(
        pool: ConnectionPool,
        embed_dim: usize,
        search_config: Option<SearchConfig>,
        upcaster_registry: UpcasterRegistry,
        reranker: Option<Box<dyn Reranker>>,
    ) -> Result<Self> {
        // 1. Validate embed_dim (must happen first — ensures schema is ready).
        if pool.is_read_only() {
            let conn = pool.read();
            Self::validate_embed_dim(&conn, embed_dim)?;
        } else {
            let conn = pool.write();
            Self::validate_or_set_embed_dim(&conn, embed_dim)?;
        }

        // 2. Try snapshot fast path (file-backed engines only).
        if let Some(loaded) = Self::try_load_snapshot(&pool, embed_dim, &search_config)? {
            tracing::info!("loaded from snapshot (fingerprint match)");
            return Ok(Self {
                pool,
                embed_dim,
                graph: RwLock::new(loaded.graph),
                scope_tree: RwLock::new(loaded.scope_tree),
                vector_strategy: Box::new(BruteForce),
                reranker,
                #[cfg(feature = "ann")]
                hnsw_strategy: loaded.hnsw_strategy,
                search_config,
                upcaster_registry,
            });
        }

        // 3. Full rebuild from SQLite (current behavior).
        let (graph, scope_tree) = {
            let conn = pool.read();
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

    /// Attempt to load in-memory structures from a sidecar snapshot.
    ///
    /// Returns `Ok(Some(data))` if the snapshot validated against the current
    /// DB fingerprint. Returns `Ok(None)` if no snapshot exists or it's
    /// stale/invalid. Returns `Err` only for unexpected DB query failures.
    fn try_load_snapshot(
        pool: &ConnectionPool,
        embed_dim: usize,
        #[cfg_attr(not(feature = "ann"), allow(unused_variables))] search_config: &Option<
            SearchConfig,
        >,
    ) -> Result<Option<SnapshotData>> {
        let Some(db_path) = pool.path() else {
            return Ok(None); // in-memory engine
        };

        let snap_path = snapshot::snapshot_path(db_path);
        let Some((header, payload)) = snapshot::load_from_file(&snap_path, embed_dim) else {
            return Ok(None);
        };

        let conn = pool.read();
        let current_fp = snapshot::read_fingerprint(&conn)?;
        drop(conn);

        if header.fingerprint != current_fp {
            tracing::info!("snapshot stale (fingerprint mismatch), falling back to full rebuild");
            return Ok(None);
        }

        let graph = MemoryGraph::from_snapshot(&payload.graph);
        let scope_tree = ScopeTree::from_snapshot(&payload.scope_tree);

        #[cfg(feature = "ann")]
        let hnsw_strategy = match (search_config, payload.hnsw) {
            (Some(cfg), Some(ref hnsw_snap)) if cfg.ann_threshold < usize::MAX => Some(
                crate::search::ann::HnswStrategy::from_snapshot(hnsw_snap, embed_dim)?,
            ),
            (Some(cfg), None) if cfg.ann_threshold < usize::MAX => {
                // Snapshot was created without HNSW data (e.g. non-ann build),
                // but current config requires ANN. Fall back to DB rebuild.
                let conn = pool.read();
                Some(crate::search::ann::HnswStrategy::build_from_db(
                    &conn, embed_dim,
                )?)
            }
            _ => None,
        };

        Ok(Some(SnapshotData {
            graph,
            scope_tree,
            #[cfg(feature = "ann")]
            hnsw_strategy,
        }))
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
        self.hnsw_strategy.as_ref().is_some_and(|hnsw| {
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

    /// Write a snapshot of in-memory state to the sidecar file.
    ///
    /// No-op for in-memory engines or read-only engines.
    /// Returns `Ok(false)` if skipped, `Ok(true)` if written.
    ///
    /// # Preconditions
    ///
    /// Assumes single-writer semantics: only one `MemoryEngine` instance per
    /// database file. If multiple instances share the same file, the snapshot
    /// may reflect stale in-memory state while the fingerprint matches the DB.
    ///
    /// # No-panic contract
    ///
    /// This method never panics. All internal operations use checked access
    /// and propagate errors via `Result`. Safe to call from `Drop`.
    pub fn write_snapshot(&self) -> Result<bool> {
        let Some(db_path) = self.pool.path() else {
            return Ok(false);
        };
        if self.pool.is_read_only() {
            return Ok(false);
        }

        let conn = self.pool.read();
        let fingerprint = snapshot::read_fingerprint(&conn)?;

        let graph_snap = self.graph.read().to_snapshot();
        let scope_snap = self.scope_tree.read().to_snapshot();

        #[cfg(feature = "ann")]
        let hnsw_snap = self
            .hnsw_strategy
            .as_ref()
            .map(|h| h.to_snapshot(&conn, self.embed_dim))
            .transpose()?;
        #[cfg(not(feature = "ann"))]
        let hnsw_snap: Option<snapshot::HnswSnapshot> = None;

        let header = snapshot::SnapshotHeader {
            format_version: snapshot::FORMAT_VERSION,
            fingerprint,
            embed_dim: self.embed_dim,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let payload = snapshot::SnapshotPayload {
            graph: graph_snap,
            scope_tree: scope_snap,
            hnsw: hnsw_snap,
        };

        snapshot::write_to_file(&header, &payload, &snapshot::snapshot_path(db_path))?;
        Ok(true)
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

impl Drop for MemoryEngine {
    fn drop(&mut self) {
        if let Err(e) = self.write_snapshot() {
            tracing::warn!(error = %e, "failed to write snapshot on shutdown");
        }
    }
}
