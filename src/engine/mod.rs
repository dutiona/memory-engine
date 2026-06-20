use std::path::PathBuf;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use rusqlite::Connection;

use crate::error::{MemoryError, MigrationError, RerankerError, Result};
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
use crate::traits::{EmbeddingProvider, Reranker};
use crate::types::{ConsolidationLevel, EmbeddingFingerprint, Fact};

mod activity;
pub(crate) mod activity_filter;
mod bootstrap;
pub mod builder;
pub(crate) mod cognitive;
mod conflict;
mod consolidation;
pub(crate) mod cycle;
mod dormant;
mod forgetting;
mod graph;
mod ingest;
mod inspect;
mod lineage;
mod outcome;
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

#[cfg(test)]
mod tests;

/// Construction-equivalence golden harness for the builder migration (#541).
#[cfg(test)]
mod equivalence;

/// Configuration for opening a file-backed [`MemoryEngine`].
///
/// `EngineConfig` is the plain-data transport the restore family and the async
/// wrapper consume; the ergonomic front door for constructing an engine is
/// [`MemoryEngine::builder`], which assembles one of these internally for the
/// file path. Build a config with [`EngineConfig::new`] and the chained
/// `with_*` setters:
///
/// ```
/// use memory_engine::EngineConfig;
/// let config = EngineConfig::new("agent.db".into(), 384)
///     .with_read_only(true)
///     .with_read_pool_size(2);
/// ```
///
/// `#[non_exhaustive]`: fields may be added in minor releases, so the struct
/// cannot be built by struct literal from outside this crate — use `new` +
/// `with_*`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EngineConfig {
    pub(crate) path: PathBuf,
    pub(crate) embed_dim: usize,
    /// Number of read connections in the pool (default: 4).
    pub(crate) read_pool_size: usize,
    /// Optional search configuration for ANN strategy dispatch.
    pub(crate) search_config: Option<SearchConfig>,
    /// Optional directory for WAL-safe pre-migration backups.
    /// When set, `VACUUM INTO` creates a backup before running migrations.
    pub(crate) backup_dir: Option<PathBuf>,
    /// Upcaster registry for event payload versioning.
    /// Defaults to empty (all event types at revision 1).
    pub(crate) upcaster_registry: UpcasterRegistry,
    /// Open in read-only mode: skip init/migration, reject writes.
    pub(crate) read_only: bool,
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

    /// Set the read connection pool size for a file-backed engine (default 4).
    ///
    /// Must be in `[1, 256]`. A value of `0` or one above the cap (256) is
    /// **rejected at open time** with [`MemoryError::Pool`] — `0` is not a covert
    /// in-memory request (#340/#356) and each read connection is a real OS file
    /// descriptor, so an oversized value is refused rather than allowed to
    /// exhaust the FD table (#415). Ignored for in-memory engines, which serve
    /// reads through the single shared connection.
    ///
    /// [`MemoryError::Pool`]: crate::MemoryError::Pool
    #[must_use]
    pub const fn with_read_pool_size(mut self, size: usize) -> Self {
        self.read_pool_size = size;
        self
    }

    /// Set the search configuration for ANN strategy dispatch.
    #[must_use]
    pub const fn with_search_config(mut self, config: SearchConfig) -> Self {
        self.search_config = Some(config);
        self
    }

    /// Set the directory for WAL-safe pre-migration backups.
    #[must_use]
    pub fn with_backup_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.backup_dir = Some(dir.into());
        self
    }

    /// Set the upcaster registry for event-payload versioning.
    #[must_use]
    pub fn with_upcaster_registry(mut self, registry: UpcasterRegistry) -> Self {
        self.upcaster_registry = registry;
        self
    }

    /// Open in read-only mode: skip init/migration, reject writes.
    #[must_use]
    pub const fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
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
    /// Shared file-backed open path: select the pool per `read_only`, then
    /// initialize. This is the single `EngineConfig -> pool -> init_from_pool`
    /// seam used by [`MemoryEngineBuilder`](builder::MemoryEngineBuilder)'s file
    /// state, the restore family, and the async wrapper. It carries the optional
    /// reranker explicitly because `EngineConfig` does not (and cannot — the
    /// reranker is not `Clone`) hold one.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Migration` if the stored `embed_dim` doesn't match.
    pub(crate) fn open_from_config(
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

    /// Shared constructor logic: validate `embed_dim`, load graph and scope tree.
    ///
    /// Tries the snapshot fast path first (file-backed engines only). If the
    /// sidecar validates against the current DB fingerprint, loads from it.
    /// Otherwise falls back to full `SQLite` scan.
    fn init_from_pool(
        pool: ConnectionPool,
        embed_dim: usize,
        search_config: Option<SearchConfig>,
        upcaster_registry: UpcasterRegistry,
        reranker: Option<Box<dyn Reranker>>,
    ) -> Result<Self> {
        // 1. Validate embed_dim (must happen first — ensures schema is ready).
        if pool.is_read_only() {
            let conn = pool.read()?;
            Self::validate_embed_dim_against_meta(&conn, embed_dim)?;
        } else {
            let conn = pool.write();
            Self::validate_embed_dim_against_meta(&conn, embed_dim)?;
        }

        // 2. Try snapshot fast path (file-backed engines only).
        if let Some(loaded) = Self::try_load_snapshot(&pool, embed_dim, search_config.as_ref())? {
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
            let conn = pool.read()?;
            let graph = MemoryGraph::load_from_db(&conn)?;
            let scope_tree = ScopeTree::load(&conn)?;
            drop(conn);
            (graph, scope_tree)
        };

        // HNSW build is read-only — always use a read connection.
        #[cfg(feature = "ann")]
        let hnsw_strategy = if let Some(ref cfg) = search_config {
            if cfg.ann_threshold < usize::MAX {
                let conn = pool.read()?;
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
        #[cfg_attr(not(feature = "ann"), allow(unused_variables))] search_config: Option<
            &SearchConfig,
        >,
    ) -> Result<Option<SnapshotData>> {
        let Some(db_path) = pool.path() else {
            return Ok(None); // in-memory engine
        };

        let snap_path = snapshot::snapshot_path(db_path);
        let Some((header, payload)) = snapshot::load_from_file(&snap_path, embed_dim) else {
            return Ok(None);
        };

        let conn = pool.read()?;
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
                let conn = pool.read()?;
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
    // Not `const`: with the `ann` feature, `should_use_hnsw` is a non-const
    // method (it inspects runtime HNSW state), so this cannot be const across
    // the whole feature matrix even though clippy sees it as const-able under
    // default features.
    #[allow(clippy::missing_const_for_fn)]
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
        // `self` is unused without the `ann` feature; the ann-enabled twin
        // inspects `self.hnsw_strategy`, so both variants must share a
        // `&self` signature for the call sites to compile in either config.
        let _ = self;
        false
    }

    /// Validate the configured `embed_dim` against the persisted embedding identity
    /// — read-only, never writes. Shared by the read-write and read-only open paths.
    ///
    /// If an `embedding_meta` tuple is recorded (#613), its `dim` MUST equal the
    /// runtime `embed_dim` from `EngineConfig`. If none is recorded yet (a fresh
    /// store, or one that has only ingested events and never embedded a fact), this
    /// is a no-op: the identity is established lazily on the first embedding write
    /// (ADR 0015 §2), not at open — open holds no `EmbeddingProvider` and so cannot
    /// write it. A read-only open of an un-embedded store is therefore `Ok`: an
    /// empty store has no identity to disagree with, and the runtime dimension always
    /// comes from `EngineConfig`.
    fn validate_embed_dim_against_meta(conn: &Connection, embed_dim: usize) -> Result<()> {
        if let Some(fp) = crate::store::embedding_meta::load(conn)?
            && fp.dim != embed_dim
        {
            return Err(MigrationError::EmbedDimMismatch {
                stored: fp.dim,
                requested: embed_dim,
            }
            .into());
        }
        Ok(())
    }

    /// Record the embedding identity on the first embedding write (#613, ADR 0015 §2).
    ///
    /// Idempotent and cheap after the first call: [`embedding_meta::record_if_absent`]
    /// returns the stored tuple without writing once an identity exists. Every code
    /// path that embeds-then-persists (ingest, batch ingest) calls this **before**
    /// inserting the derived vector, under the same write transaction, so the store's
    /// identity is never older than its first vector.
    ///
    /// Free-function persist paths that hold no `&self` (consolidation, bootstrap)
    /// call [`embedding_meta::record_if_absent`] directly with their `embed_dim`; this
    /// method is the thin `&self` convenience for the engine's own ingest methods.
    ///
    /// **This is the seam #614 extends** — enforcement is added inside
    /// `record_if_absent`; this method's call sites do not change when it lands.
    ///
    /// [`embedding_meta::record_if_absent`]: crate::store::embedding_meta::record_if_absent
    ///
    /// # Errors
    ///
    /// Propagates [`embedding_meta::record_if_absent`] errors, including
    /// `MemoryError::EmbeddingDimension` when the provider's declared `dim` disagrees
    /// with this engine's vector dimension.
    pub(crate) fn record_embedding_identity(
        &self,
        conn: &Connection,
        embedder: &dyn EmbeddingProvider,
    ) -> Result<()> {
        let fp = embedder.fingerprint();
        crate::store::embedding_meta::record_if_absent(conn, &fp, self.embed_dim)?;
        Ok(())
    }

    /// Verify a provider's identity matches the store's recorded embedding identity —
    /// a fail-fast check for consumer startup (#614, §Design.2).
    ///
    /// The query path embeds at the consumer layer and hands the engine a *pre-computed*
    /// vector, so the engine cannot fingerprint-check per query. This one-shot check
    /// catches a misconfigured provider — one whose vector space differs from the one
    /// the store was built with — **before** any query silently returns
    /// wrong-vector-space results. On a **fresh** store (no recorded identity) there is
    /// no model to disagree with, but the provider's `dim` must still match this engine's
    /// configured dimension — otherwise every subsequent write/query would fail on
    /// `EmbeddingDimension`, so we fail fast here too (mirroring `record_if_absent`).
    ///
    /// This is read-only and does not establish the identity (that happens lazily on the
    /// first embedding *write*, via [`record_embedding_identity`](Self::record_embedding_identity)).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingDimension`] if the provider's dimension differs
    /// from this engine's, or [`MemoryError::EmbeddingModelMismatch`] if its fingerprint
    /// disagrees with the store's recorded identity.
    pub fn verify_embedding_identity(&self, provider: &dyn EmbeddingProvider) -> Result<()> {
        self.verify_embedding_fingerprint(&provider.fingerprint())
    }

    /// Verify a **declared** embedding fingerprint is compatible with the store's
    /// recorded identity, without writing (#615, §Design.3).
    ///
    /// This is the read-only check behind a pre-computed `memory_query` submission: the
    /// caller declares the identity of the model that produced the query vector, and we
    /// reject it if it disagrees with the store's space — otherwise the query would
    /// silently retrieve against a foreign vector space. Same semantics as
    /// [`verify_embedding_identity`](Self::verify_embedding_identity), but for a caller
    /// declaration rather than a live provider. A fresh store (no identity) accepts any
    /// same-dimension fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingDimension`] if `candidate.dim` differs from this
    /// engine's dimension, or [`MemoryError::EmbeddingModelMismatch`] if it disagrees
    /// with the store's recorded identity.
    pub fn verify_embedding_fingerprint(&self, candidate: &EmbeddingFingerprint) -> Result<()> {
        if candidate.dim != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: candidate.dim,
            });
        }
        self.with_read(|conn| crate::store::embedding_meta::check_compatible(conn, candidate))
    }

    /// Whether this engine was opened in read-only mode.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
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
    /// # Errors
    ///
    /// Returns `MemoryError::Database` or `MemoryError::Io` if reading the DB
    /// fingerprint or writing the snapshot file fails.
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

        let conn = self.pool.read()?;
        let fingerprint = snapshot::read_fingerprint(&conn)?;

        #[cfg(feature = "ann")]
        let hnsw_snap = self
            .hnsw_strategy
            .as_ref()
            .map(|h| h.to_snapshot(&conn, self.embed_dim))
            .transpose()?;
        drop(conn);
        #[cfg(not(feature = "ann"))]
        let hnsw_snap: Option<snapshot::HnswSnapshot> = None;

        let graph_snap = self.graph.read().to_snapshot();
        let scope_snap = self.scope_tree.read().to_snapshot();

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
        let conn = self.pool.read()?;
        f(&conn)
    }

    /// Lock the write connection and return the guard directly.
    /// Callers use this when they need to hold the write lock across
    /// multiple operations (e.g., DB mutation + cache update).
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    fn write_conn(&self) -> Result<crate::pool::WriteGuard<'_>> {
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
        drop(conn);
        Ok(stamped.into_iter().collect())
    }

    /// Ensure a scope path exists using an already-held connection.
    ///
    /// Shared helper for [`ensure_scope_path()`] and [`add_fact()`] to avoid
    /// duplicating scope resolution logic.
    ///
    /// `ensure_path` creates *every* segment of a multi-level path in the DB but
    /// returns only the leaf id. The in-memory [`ScopeTree`] must mirror the DB,
    /// so we insert the entire newly-resolved chain — leaf **and all ancestors up
    /// to (but excluding) the root** — not just the leaf. Inserting only the leaf
    /// would leave `resolve_path` (which walks `children` from root) unable to
    /// traverse the missing intermediate links, making any depth > 1 scope query
    /// (`scope_subtree`/`scope_exact`/…) return zero results in-session even
    /// though the facts are correctly persisted. [`ScopeTree::insert`] is
    /// idempotent by id, so re-inserting shared ancestors is a no-op.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "write lock must be held for the entire leaf-to-root walk to prevent interleaved inserts from racing on the same path"
    )]
    fn ensure_scope_with_conn(&self, conn: &Connection, path: &str) -> Result<i64> {
        let scope_store = ScopeStore::new(conn);
        let id = scope_store.ensure_path(path)?;

        // Walk leaf → root via parent_id, caching every node into the tree.
        // Stop at the root (always present) and guard against a malformed
        // parent cycle so a hostile DB can't spin this loop forever.
        let mut tree = self.scope_tree.write();
        let mut seen = std::collections::HashSet::new();
        let mut current = Some(id);
        while let Some(node_id) = current {
            if node_id == ScopeTree::root_id() || !seen.insert(node_id) {
                break;
            }
            let node = scope_store.get(node_id)?;
            current = node.parent_id;
            tree.insert(node);
        }
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
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
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
    pub const fn is_file_backed(&self) -> bool {
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
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
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
            return Err(RerankerError::OutputTooLong {
                output_len: output.len(),
                input_len: num_candidates,
            }
            .into());
        }

        let mut seen = HashSet::with_capacity(output.len());

        for &(idx, score) in output {
            if idx >= num_candidates {
                return Err(RerankerError::OutOfBoundsIndex {
                    index: idx,
                    num_candidates,
                }
                .into());
            }
            if !seen.insert(idx) {
                return Err(RerankerError::DuplicateIndex { index: idx }.into());
            }
            if !score.is_finite() {
                return Err(RerankerError::NonFiniteScore { score, index: idx }.into());
            }
        }

        Ok(())
    }

    /// Resolve scope IDs from an optional scope path.
    /// Returns [`root_id`] when scope is None, or ancestor IDs when scope exists.
    fn resolve_scope_ids(&self, scope: Option<&str>) -> Result<Vec<i64>> {
        let tree = self.scope_tree.read();
        match scope {
            Some(path) => {
                let id = tree
                    .resolve_path(path)
                    .ok_or_else(|| MemoryError::NotFound(format!("scope path: {path}")))?;
                Ok(tree.ancestors(id))
            }
            None => Ok(vec![ScopeTree::root_id()]),
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
    if let Some(t_valid) = fact.t_valid
        && t_valid > cutoff
    {
        return false;
    }
    if let Some(t_invalid) = fact.t_invalid
        && t_invalid <= cutoff
    {
        return false;
    }
    true
}

/// Check if a fact's `[t_valid, t_invalid)` interval overlaps `[start, end)`.
fn fact_overlaps_period(fact: &Fact, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    // (t_valid IS NULL OR t_valid < end) AND (t_invalid IS NULL OR t_invalid > start)
    fact.t_valid.is_none_or(|tv| tv < end) && fact.t_invalid.is_none_or(|ti| ti > start)
}

/// Wrap a `Fact` into a `SearchResult` with `MatchType::ImportanceRank`.
const fn fact_to_search_result(fact: Fact) -> SearchResult {
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
