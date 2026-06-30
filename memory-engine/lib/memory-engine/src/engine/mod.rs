use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use rusqlite::Connection;

use crate::error::{MemoryError, MigrationError, RerankerError, Result};
use crate::graph::MemoryGraph;
use crate::pool::ConnectionPool;
use crate::scope::ScopeTree;
use crate::search::hybrid::{MatchType, SearchResult};
use crate::search::strategy::SearchConfig;
use crate::storage::StorageBackend;
use crate::storage::sqlite::SqliteBackend;
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
mod reconstruct;
mod restore;
mod resume;
mod scheduling;
pub(crate) mod snapshot;

#[cfg(feature = "archive")]
mod archive;

#[cfg(test)]
mod tests;

/// Construction-equivalence golden harness for the builder migration (#541).
#[cfg(test)]
mod equivalence;

/// Which [`StorageBackend`] implementation backs the engine.
///
/// Resolved once in `MemoryEngine::open_from_config`. The in-process
/// [`SqliteBackend`] is the default and only variant today; epic #628 adds a
/// `Postgres` arm (#634).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendKind {
    /// The default in-process `SQLite` backend (read-pool + write-mutex).
    #[default]
    Sqlite,
}

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
    /// Which storage backend to open (default: [`BackendKind::Sqlite`]).
    pub(crate) backend: BackendKind,
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
            backend: BackendKind::Sqlite,
        }
    }

    /// Select the storage backend (default [`BackendKind::Sqlite`]).
    #[must_use]
    pub const fn with_backend(mut self, backend: BackendKind) -> Self {
        self.backend = backend;
        self
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
    /// The single persistence handle. All DB-touching methods await this port
    /// (#631). HNSW + the connection pool live *inside* the backend now.
    storage: Arc<dyn StorageBackend>,
    /// Cold-storage handle for `.pak` archives, the same backend viewed through
    /// the feature-gated [`ColdStorage`](crate::storage::ColdStorage) port.
    #[cfg(feature = "archive")]
    cold: Arc<dyn crate::storage::ColdStorage>,
    embed_dim: usize,
    graph: RwLock<MemoryGraph>,
    scope_tree: RwLock<ScopeTree>,
    reranker: Option<Arc<dyn Reranker>>,
    upcaster_registry: UpcasterRegistry,
    /// Captured at open (the backend hides its pool): drives [`is_file_backed`].
    is_file_backed: bool,
    /// Captured at open (the backend hides its pool): drives [`is_read_only`].
    read_only: bool,
    /// The on-disk database path, captured at open (the backend hides its pool).
    /// `None` for in-memory engines. Drives the archive directory resolution now
    /// that `pool.path()` is no longer reachable from the engine.
    #[cfg_attr(not(feature = "archive"), allow(dead_code))]
    db_path: Option<PathBuf>,
    /// Set by [`flush_snapshot`](MemoryEngine::flush_snapshot) /
    /// [`close`](MemoryEngine::close) once the sidecar snapshot has been flushed;
    /// read by `Drop` to warn if a file-backed engine was dropped without flushing.
    /// `AtomicBool` so a shared owner (`Arc<MemoryEngine>`, e.g. the MCP server) can
    /// flush + mark via `&self` without unwrapping the `Arc`.
    closed: AtomicBool,
    /// The reconstruction **dimension fence** (#742). `0` = not fenced. Set to the
    /// new dimension `D′` after a *different-dimension* [`reconstruct`](Self::reconstruct)
    /// promotes a new embedding space: this handle's cached `embed_dim` is now stale
    /// (its `facts.embedding` blobs are `D′`-wide), so [`ensure_open`](Self::ensure_open)
    /// refuses every embedding-touching read/write with
    /// [`MemoryError::EmbeddingReopenRequired`] until the consumer reopens at `D′`.
    /// `AtomicUsize` keeps the engine `Send + Sync` with `&self` methods and adds no
    /// lock contention. Same-dimension reconstruction never sets it (the cached dim
    /// stays valid — #623 behavior preserved exactly).
    reopen_required: AtomicUsize,
}

impl std::fmt::Debug for MemoryEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEngine")
            .field("embed_dim", &self.embed_dim)
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
        reranker: Option<Arc<dyn Reranker>>,
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
        // `config.backend` selects the implementation. Today only `Sqlite`
        // exists (#634 adds a `Postgres` arm that bypasses `ConnectionPool`).
        let BackendKind::Sqlite = config.backend;
        Self::init_from_pool(
            pool,
            config.embed_dim,
            config.search_config.clone(),
            config.upcaster_registry.clone(),
            reranker,
        )
    }

    /// Shared constructor chokepoint (both the in-memory builder and the
    /// file-backed `open_from_config` funnel here): validate `embed_dim`,
    /// recover the in-memory projections (snapshot fast path, else full scan),
    /// then build the [`SqliteBackend`] that owns the pool + HNSW and expose it
    /// as `Arc<dyn StorageBackend>` (+ the feature-gated cold-storage view).
    ///
    /// Stays **synchronous**: all work here is direct `rusqlite` I/O during
    /// construction (the engine has not yet started awaiting the port).
    fn init_from_pool(
        pool: ConnectionPool,
        embed_dim: usize,
        search_config: Option<SearchConfig>,
        upcaster_registry: UpcasterRegistry,
        reranker: Option<Arc<dyn Reranker>>,
    ) -> Result<Self> {
        use crate::storage::sqlite::HnswOpenSource;

        // 1. Validate embed_dim (must happen first — ensures schema is ready).
        if pool.is_read_only() {
            let conn = pool.read()?;
            Self::validate_embed_dim_against_meta(&conn, embed_dim)?;
        } else {
            let conn = pool.write();
            Self::validate_embed_dim_against_meta(&conn, embed_dim)?;
        }

        let is_file_backed = pool.is_file_backed();
        let read_only = pool.is_read_only();
        let db_path = pool.path().map(std::path::Path::to_path_buf);

        // 2. Recover the in-memory projections. On the snapshot fast path the
        //    sidecar's HNSW payload (if any) is handed to the backend; on the
        //    full-rebuild path the backend builds HNSW from the DB scan.
        let (graph, scope_tree, hnsw_source) = if let Some((graph, scope_tree, hnsw_payload)) =
            Self::try_load_snapshot(&pool, embed_dim)?
        {
            tracing::info!("loaded from snapshot (fingerprint match)");
            (graph, scope_tree, HnswOpenSource::Snapshot(hnsw_payload))
        } else {
            let conn = pool.read()?;
            let graph = MemoryGraph::load_from_db(&conn)?;
            let scope_tree = ScopeTree::load(&conn)?;
            drop(conn);
            (graph, scope_tree, HnswOpenSource::Rebuild)
        };

        // 3. Build the backend; it owns the pool + HNSW from here on.
        let pool = Arc::new(pool);
        let upcaster = Arc::new(upcaster_registry.clone());
        let backend = Arc::new(
            SqliteBackend::from_pool(pool, upcaster)
                .with_open_config(search_config, hnsw_source)?,
        );

        let storage: Arc<dyn StorageBackend> = backend.clone();
        #[cfg(feature = "archive")]
        let cold: Arc<dyn crate::storage::ColdStorage> = backend;
        #[cfg(not(feature = "archive"))]
        drop(backend);

        Ok(Self {
            storage,
            #[cfg(feature = "archive")]
            cold,
            embed_dim,
            graph: RwLock::new(graph),
            scope_tree: RwLock::new(scope_tree),
            reranker,
            upcaster_registry,
            is_file_backed,
            read_only,
            db_path,
            closed: AtomicBool::new(false),
            reopen_required: AtomicUsize::new(0),
        })
    }

    /// Attempt to recover the in-memory projections from a sidecar snapshot.
    ///
    /// Returns `Ok(Some((graph, scope_tree, hnsw_payload)))` if the snapshot
    /// validated against the current DB fingerprint — `hnsw_payload` is the
    /// sidecar's HNSW blob (possibly `None`), forwarded to the backend. Returns
    /// `Ok(None)` if no snapshot exists or it is stale/invalid.
    fn try_load_snapshot(
        pool: &ConnectionPool,
        embed_dim: usize,
    ) -> Result<Option<(MemoryGraph, ScopeTree, Option<snapshot::HnswSnapshot>)>> {
        let Some(db_path) = pool.path() else {
            return Ok(None); // in-memory engine
        };

        let snap_path = snapshot::snapshot_path(db_path);
        let Some((header, payload)) = snapshot::load_from_file(&snap_path, embed_dim) else {
            return Ok(None);
        };

        let conn = pool.read()?;
        let current_fp = snapshot::read_fingerprint(&conn)?;

        if header.fingerprint != current_fp {
            tracing::info!("snapshot stale (fingerprint mismatch), falling back to full rebuild");
            return Ok(None);
        }

        // Referential-validation set (#257): the live set of *existing* fact ids
        // (any `t_expired`), used below to reject any snapshot edge whose endpoint
        // references a fact that does not exist (a phantom-node injection). This is
        // the same population `load_from_db` honors via the SQLite foreign key — all
        // facts, not active-only: an active edge can legitimately point at an expired
        // fact (see `existing_fact_ids` / `MemoryGraph::from_snapshot`). Queried once
        // from the authoritative connection that validated the fingerprint, *after*
        // the fingerprint check (perf #499): on the common stale-snapshot path the
        // early return above skips this full-table scan entirely.
        let existing_fact_ids = crate::store::facts::existing_fact_ids(&conn)?;
        drop(conn);

        // Defense in depth (#412): the fingerprint now matches the live DB, so the
        // sidecar's edge list MUST hold exactly `active_edge_count` edges. A
        // different length is an internally-inconsistent (corrupt/tampered) sidecar
        // — turn the count fingerprint into an explicit bound and discard rather
        // than trust it. `active_edge_count` is non-negative in practice (a COUNT),
        // and `usize::try_from` of a negative value falls through to the mismatch
        // branch, so a malformed fingerprint also rejects.
        let expected_edges = usize::try_from(current_fp.active_edge_count).ok();
        if expected_edges != Some(payload.graph.edges.len()) {
            tracing::warn!(
                snapshot_edges = payload.graph.edges.len(),
                db_active_edges = current_fp.active_edge_count,
                "snapshot edge count disagrees with the validated DB fingerprint, \
                 discarding sidecar and rebuilding from the database"
            );
            return Ok(None);
        }

        // Bound + revalidate the snapshot edge set (#412, #499). On any violation,
        // discard the (rebuildable) sidecar and fall back to a full rebuild from
        // the authoritative DB rather than failing the open.
        let graph = match MemoryGraph::from_snapshot(&payload.graph, &existing_fact_ids) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "snapshot graph failed edge revalidation, rebuilding from the database"
                );
                return Ok(None);
            }
        };
        let scope_tree = ScopeTree::from_snapshot(&payload.scope_tree);
        Ok(Some((graph, scope_tree, payload.hnsw)))
    }

    /// Returns the name of the active reranker, if any.
    #[must_use]
    pub fn reranker_name(&self) -> Option<&str> {
        self.reranker.as_ref().map(|r| r.name())
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
    /// first embedding *write*, below the seam in the atomic insert paths via
    /// `store::embedding_meta::record_if_absent`).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::EmbeddingDimension`] if the provider's dimension differs
    /// from this engine's, or [`MemoryError::EmbeddingModelMismatch`] if its fingerprint
    /// disagrees with the store's recorded identity.
    pub async fn verify_embedding_identity(&self, provider: &dyn EmbeddingProvider) -> Result<()> {
        self.verify_embedding_fingerprint(&provider.fingerprint())
            .await
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
    pub async fn verify_embedding_fingerprint(
        &self,
        candidate: &EmbeddingFingerprint,
    ) -> Result<()> {
        if candidate.dim != self.embed_dim {
            return Err(MemoryError::EmbeddingDimension {
                expected: self.embed_dim,
                actual: candidate.dim,
            });
        }
        self.storage.check_embedding_compatible(candidate).await
    }

    /// Whether this engine was opened in read-only mode.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Flush the in-memory projections to the backend's sidecar snapshot, taking
    /// only `&self`.
    ///
    /// After #631 the engine no longer holds the pool/HNSW, so snapshot assembly
    /// (DB fingerprint + HNSW) lives below the port; this hands the engine's two
    /// projections down via [`SchemaManager::write_engine_snapshot`](crate::storage::SchemaManager::write_engine_snapshot) and marks the
    /// engine flushed so `Drop` won't warn.
    ///
    /// Unlike [`close`](Self::close) this needs no `&mut`, so a **shared owner**
    /// (`Arc<MemoryEngine>`, e.g. the long-lived MCP server that cannot unwrap the
    /// `Arc` to get `&mut`) can persist the sidecar on shutdown — or periodically.
    /// Idempotent and safe to call repeatedly.
    ///
    /// No-op for in-memory or read-only engines (`Ok(false)`). `Ok(true)` when a
    /// snapshot was written.
    ///
    /// Also a no-op (`Ok(false)`) when the handle is **fenced** by a different-dim
    /// reconstruction (#742): the in-memory HNSW it would serialize was built at the
    /// old dimension, while the DB (and thus the sidecar's fingerprint/`embed_dim`
    /// header) is now `D′`. The reopen at `D′` rebuilds the index from the DB anyway
    /// (the `embed_dim` header gate at `snapshot::load_from_file` already rejects a
    /// stale-dim sidecar), and under `ann` the assembly would otherwise *fail*
    /// re-reading the now-`D′` `facts.embedding` at the old dim. Short-circuiting
    /// avoids that spurious error and the wasted work. The consumer can still flush +
    /// drop a fenced engine cleanly.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` if the fingerprint read or sidecar write
    /// fails.
    pub async fn flush_snapshot(&self) -> Result<bool> {
        // Fenced by a different-dim reconstruction → no sidecar (see doc above).
        if self.reopen_required.load(Ordering::Acquire) != 0 {
            self.closed.store(true, Ordering::Release);
            return Ok(false);
        }
        // Build the owned snapshots, then await — the read guards are temporaries
        // dropped at the end of each statement, so none is held across `.await`.
        let graph_snap = self.graph.read().to_snapshot();
        let scope_snap = self.scope_tree.read().to_snapshot();
        let wrote = self
            .storage
            .write_engine_snapshot(graph_snap, scope_snap)
            .await?;
        self.closed.store(true, Ordering::Release);
        Ok(wrote)
    }

    /// Flush the sidecar snapshot and finalize the engine (the exclusive-owner
    /// shutdown path).
    ///
    /// Thin `&mut` wrapper over [`flush_snapshot`](Self::flush_snapshot): call this
    /// before dropping a file-backed engine you own exclusively (e.g. a CLI command).
    /// A shared `Arc<MemoryEngine>` owner that cannot obtain `&mut` should call
    /// [`flush_snapshot`](Self::flush_snapshot) instead. `Drop` cannot run either (it
    /// is `async` and touches the port) and only logs a warning if neither was
    /// called — the dropped-without-flush sidecar is rebuilt from the DB on next open.
    ///
    /// No-op for in-memory or read-only engines (`Ok(false)`). `Ok(true)` when a
    /// snapshot was written.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` if the fingerprint read or sidecar write
    /// fails.
    pub async fn close(&mut self) -> Result<bool> {
        self.flush_snapshot().await
    }

    /// Stamp `surfaced_at` for the given fact IDs and return the DB-authoritative
    /// `(id, timestamp)` pairs. Shared by `list_due()` and `resume_context()`.
    async fn stamp_surfaced_facts(
        &self,
        fact_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Result<std::collections::HashMap<i64, DateTime<Utc>>> {
        let stamped = self.storage.stamp_facts_surfaced(fact_ids, now).await?;
        Ok(stamped.into_iter().collect())
    }

    /// Mirror a freshly-resolved scope chain (leaf → root, excluding root) into
    /// the in-memory [`ScopeTree`] after the DB rows exist.
    ///
    /// The in-memory tree must mirror the DB, so we insert the entire chain —
    /// leaf **and all ancestors up to (but excluding) the root** — not just the
    /// leaf. Inserting only the leaf would leave `resolve_path` (which walks
    /// `children` from root) unable to traverse the missing intermediate links,
    /// making any depth > 1 scope query return zero results in-session even
    /// though the facts are correctly persisted. [`ScopeTree::insert`] is
    /// idempotent by id, so re-inserting shared ancestors is a no-op.
    ///
    /// The port walk (`get_scope`) is awaited up front into an owned `Vec`; the
    /// `scope_tree` write guard is taken only afterward, so no lock is held
    /// across `.await` (keeps the future `Send`).
    async fn cache_scope_chain(&self, leaf_id: i64) -> Result<()> {
        let mut nodes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut current = Some(leaf_id);
        while let Some(node_id) = current {
            if node_id == ScopeTree::root_id() || !seen.insert(node_id) {
                break;
            }
            let node = self.storage.get_scope(node_id).await?;
            current = node.parent_id;
            nodes.push(node);
        }
        {
            let mut tree = self.scope_tree.write();
            for node in nodes {
                tree.insert(node);
            }
        }
        Ok(())
    }

    // --- Public API: Config ---

    /// Read a config value by key.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub async fn get_config(&self, key: &str) -> Result<Option<String>> {
        self.storage.get_config(key).await
    }

    /// Write a config value (upsert).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the engine was opened read-only.
    /// Returns `MemoryError::Database` on write failure.
    pub async fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.storage.set_config(key, value).await
    }

    /// Embedding dimension configured for this engine.
    #[must_use]
    pub const fn embed_dim(&self) -> usize {
        self.embed_dim
    }

    /// The new dimension a different-dimension reconstruction (#742) promoted to,
    /// if this handle is **fenced** — `Some(new_dim)` means the engine refuses
    /// embedding-touching operations until reopened at `new_dim`; `None` means it
    /// is serving normally. The pull-side companion to the push-side
    /// [`MemoryError::EmbeddingReopenRequired`] the gated methods return; a consumer
    /// can poll this after a [`reconstruct`](Self::reconstruct) instead of catching
    /// the error.
    #[must_use]
    pub fn reopen_required(&self) -> Option<usize> {
        match self.reopen_required.load(Ordering::Acquire) {
            0 => None,
            new_dim => Some(new_dim),
        }
    }

    /// Read-fence guard (#742): `Err(EmbeddingReopenRequired)` once a different-dim
    /// reconstruction has fenced this handle, else `Ok(())`. Called at the entry of
    /// every embedding-touching public method so a stale-dimension read surfaces an
    /// actionable error rather than a low-level `EmbeddingDimension` from a blob of
    /// the wrong width.
    fn ensure_open(&self) -> Result<()> {
        match self.reopen_required.load(Ordering::Acquire) {
            0 => Ok(()),
            new_dim => Err(MemoryError::EmbeddingReopenRequired { new_dim }),
        }
    }

    /// Test-only: arm the reconstruction dimension fence directly, so the read-safety
    /// net (#742 Phase 1) can be exercised without driving a full different-dim
    /// reconstruction. Production code arms it only inside [`reconstruct`](Self::reconstruct).
    #[cfg(test)]
    fn force_reopen_fence(&self, new_dim: usize) {
        self.reopen_required.store(new_dim, Ordering::Release);
    }

    /// Whether this engine is file-backed (vs in-memory).
    #[must_use]
    pub const fn is_file_backed(&self) -> bool {
        self.is_file_backed
    }

    /// Test-only accessor for the storage port.
    ///
    /// The `pool`/`write_conn()`/`with_read()` internals the pre-#631 tests reached
    /// through are gone; tests that need to seed or inspect store state directly do
    /// so through the same `Arc<dyn StorageBackend>` the engine uses (e.g.
    /// `engine.storage().insert_fact(&new_fact).await`).
    #[cfg(test)]
    pub(crate) fn storage(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
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
    pub async fn ensure_scope_path(&self, path: &str) -> Result<i64> {
        let id = self.storage.ensure_scope_path(path).await?;
        self.cache_scope_chain(id).await?;
        Ok(id)
    }

    // --- Public API: Direct data access ---

    /// Get a fact by id.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::NotFound` if the fact doesn't exist.
    pub async fn get_fact(&self, id: i64) -> Result<Fact> {
        self.ensure_open()?;
        self.storage.get_fact(id).await
    }

    /// List active (non-expired) facts, optionally limited.
    ///
    /// When `limit` is `Some(n)`, a SQL `LIMIT` clause avoids materializing
    /// the entire corpus. `None` returns all active facts.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        self.ensure_open()?;
        self.storage.list_active_facts(limit).await
    }

    /// List summaries by consolidation level.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on query failure.
    pub async fn list_summaries(
        &self,
        level: &ConsolidationLevel,
    ) -> Result<Vec<crate::types::Summary>> {
        self.ensure_open()?;
        self.storage.list_summaries_by_level(level).await
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

/// Map a `tokio::task::spawn_blocking` join failure (a panic or cancellation in an
/// offloaded consumer-trait call — embed/rerank/summarize/classify/propose) to a
/// `MemoryError`. Shared by every engine module that offloads a sync consumer trait.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as map_err(spawn_join_err) fn pointer"
)]
pub(super) fn spawn_join_err(e: tokio::task::JoinError) -> MemoryError {
    MemoryError::Internal(format!("offloaded task failed: {e}"))
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
        // The sidecar flush is now async ([`close`]) and touches the port — it
        // cannot run from `Drop`. If a file-backed engine is dropped without
        // `close()`, the in-memory snapshot is simply not written and is rebuilt
        // from the DB on next open (correct, just slower). Warn so the missing
        // `close()` is visible.
        if self.is_file_backed && !self.closed.load(Ordering::Acquire) {
            tracing::warn!(
                "MemoryEngine dropped without close()/flush_snapshot(): sidecar \
                 snapshot not flushed; it will be rebuilt from the database on next open"
            );
        }
    }
}

/// Property-based coverage for the pure bi-temporal filter helpers (#450).
///
/// `passes_temporal_cutoff` and `fact_overlaps_period` encode interval
/// containment / overlap algebra — exactly the place `<` vs `<=` off-by-one
/// errors hide and example tests routinely miss. These proptests pin the
/// helpers to their algebraic spec and to monotonicity laws. Placed at the
/// end of the file so no non-test items follow a `#[cfg(test)]` module
/// (`clippy::items_after_test_module`).
#[cfg(test)]
mod proptest_temporal {
    use chrono::{DateTime, Utc};
    use proptest::prelude::*;

    use super::{fact_overlaps_period, passes_temporal_cutoff};
    use crate::types::{Fact, FactType};

    /// Build a `Fact` carrying only the valid-time fields the helpers read;
    /// every other field is an inert placeholder.
    fn make_fact(t_valid: Option<DateTime<Utc>>, t_invalid: Option<DateTime<Utc>>) -> Fact {
        Fact {
            id: 1,
            content: String::new(),
            content_hash: String::new(),
            embedding: vec![],
            fact_type: FactType::Semantic,
            t_created: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            t_expired: None,
            t_valid,
            t_invalid,
            source_event_id: None,
            base_importance: 0.5,
            access_count: 0,
            last_accessed: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            metadata: serde_json::Value::Null,
            scope_id: 1,
            is_pinned: false,
            importance_score: 0.0,
            surfaced_at: None,
        }
    }

    prop_compose! {
        /// Timestamps as whole seconds in a wide but always-valid range, so
        /// `from_timestamp` never returns `None`.
        fn arb_ts()(s in 0i64..=4_000_000_000) -> DateTime<Utc> {
            DateTime::<Utc>::from_timestamp(s, 0).unwrap()
        }
    }

    /// Reference spec for `passes_temporal_cutoff`, written independently of the
    /// implementation so the proptest is a genuine cross-check, not a tautology.
    fn spec_passes(
        t_valid: Option<DateTime<Utc>>,
        t_invalid: Option<DateTime<Utc>>,
        cutoff: DateTime<Utc>,
    ) -> bool {
        let valid_ok = t_valid.is_none_or(|tv| tv <= cutoff);
        let invalid_ok = t_invalid.is_none_or(|ti| ti > cutoff);
        valid_ok && invalid_ok
    }

    /// Reference spec for `fact_overlaps_period`.
    fn spec_overlaps(
        t_valid: Option<DateTime<Utc>>,
        t_invalid: Option<DateTime<Utc>>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> bool {
        t_valid.is_none_or(|tv| tv < end) && t_invalid.is_none_or(|ti| ti > start)
    }

    proptest! {
        /// The helper agrees with its independent spec for all inputs (catches
        /// any `<`/`<=`/`>`/`>=` off-by-one regression).
        #[test]
        fn cutoff_matches_spec(
            t_valid in proptest::option::of(arb_ts()),
            t_invalid in proptest::option::of(arb_ts()),
            cutoff in arb_ts(),
        ) {
            let fact = make_fact(t_valid, t_invalid);
            prop_assert_eq!(
                passes_temporal_cutoff(&fact, cutoff),
                spec_passes(t_valid, t_invalid, cutoff)
            );
        }

        /// With `t_invalid` unbounded, passing is monotone in `cutoff`: once a
        /// fact is valid at `cutoff` it stays valid at every later instant.
        #[test]
        fn cutoff_monotone_when_open_ended(
            t_valid in proptest::option::of(arb_ts()),
            cutoff in arb_ts(),
            delta in 0i64..=1_000_000,
        ) {
            let fact = make_fact(t_valid, None);
            let later = DateTime::<Utc>::from_timestamp(cutoff.timestamp() + delta, 0).unwrap();
            if passes_temporal_cutoff(&fact, cutoff) {
                prop_assert!(passes_temporal_cutoff(&fact, later));
            }
        }

        /// A fact whose validity starts strictly after `cutoff` never passes.
        #[test]
        fn cutoff_rejects_future_t_valid(
            cutoff in arb_ts(),
            delta in 1i64..=1_000_000,
            t_invalid in proptest::option::of(arb_ts()),
        ) {
            let t_valid = DateTime::<Utc>::from_timestamp(cutoff.timestamp() + delta, 0).unwrap();
            let fact = make_fact(Some(t_valid), t_invalid);
            prop_assert!(!passes_temporal_cutoff(&fact, cutoff));
        }

        /// The helper agrees with its independent spec for all inputs.
        #[test]
        fn overlap_matches_spec(
            t_valid in proptest::option::of(arb_ts()),
            t_invalid in proptest::option::of(arb_ts()),
            a in arb_ts(),
            b in arb_ts(),
        ) {
            // Normalize to a non-empty half-open window [start, end).
            let (start, end) = if a < b { (a, b) } else { (b, a) };
            prop_assume!(start < end);
            let fact = make_fact(t_valid, t_invalid);
            prop_assert_eq!(
                fact_overlaps_period(&fact, start, end),
                spec_overlaps(t_valid, t_invalid, start, end)
            );
        }

        /// Overlap is monotone under window widening: if a fact overlaps
        /// `[start, end)` it overlaps any superset window `[start', end')` with
        /// `start' <= start` and `end' >= end`.
        #[test]
        fn overlap_monotone_under_widening(
            t_valid in proptest::option::of(arb_ts()),
            t_invalid in proptest::option::of(arb_ts()),
            a in arb_ts(),
            b in arb_ts(),
            grow_left in 0i64..=1_000_000,
            grow_right in 0i64..=1_000_000,
        ) {
            let (start, end) = if a < b { (a, b) } else { (b, a) };
            prop_assume!(start < end);
            let fact = make_fact(t_valid, t_invalid);
            if fact_overlaps_period(&fact, start, end) {
                let wider_start =
                    DateTime::<Utc>::from_timestamp(start.timestamp() - grow_left, 0).unwrap();
                let wider_end =
                    DateTime::<Utc>::from_timestamp(end.timestamp() + grow_right, 0).unwrap();
                prop_assert!(fact_overlaps_period(&fact, wider_start, wider_end));
            }
        }

        /// A fact unbounded on both valid-time ends overlaps every non-empty
        /// window.
        #[test]
        fn unbounded_fact_overlaps_everything(a in arb_ts(), b in arb_ts()) {
            let (start, end) = if a < b { (a, b) } else { (b, a) };
            prop_assume!(start < end);
            let fact = make_fact(None, None);
            prop_assert!(fact_overlaps_period(&fact, start, end));
        }
    }
}

/// Property-based coverage for the conflict-resolution DB↔graph consistency
/// invariant (#437).
///
/// After `MemoryEngine::resolve_conflict` succeeds, the active edges held by
/// the in-memory [`MemoryGraph`] must exactly mirror the active edges committed
/// to the DB — for any [`CrudDecision`] and for any number of pre-existing
/// edges incident to the old fact. Placed at the end of the file so no
/// non-test items follow a `#[cfg(test)]` module
/// (`clippy::items_after_test_module`).
#[cfg(test)]
mod proptest_conflict {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::MemoryEngine;
    use crate::graph::MemoryGraph;
    use crate::traits::{ConflictArbiter, CrudDecision};
    use crate::types::{Fact, FactType, NewEdge, NewFact, RelationType};

    const DIM: usize = 4;

    /// Minimal fixed-decision arbiter — mirrors the one in `mod tests` but
    /// declared locally so `proptest_conflict` stays self-contained and never
    /// accidentally breaks when `mod tests` changes.
    struct FixedArbiter {
        decision: CrudDecision,
    }
    impl ConflictArbiter for FixedArbiter {
        fn arbitrate(&self, _: &Fact, _: &Fact) -> crate::error::Result<CrudDecision> {
            Ok(self.decision)
        }
    }

    /// Build a `NewFact` with a distinct content string and a fixed embedding.
    fn make_fact(content: &str) -> NewFact {
        NewFact::builder(content, vec![0.5_f32; DIM], FactType::Semantic)
            .content_hash(blake3::hash(content.as_bytes()).to_hex().as_str()[..32].to_string())
            .build()
    }

    /// Canonical edge key for set-equality comparisons: `(source, target, relation)`.
    type EdgeKey = (i64, i64, RelationType);

    /// Enumerate all edges currently held by the in-memory graph as a set of
    /// `(source, target, relation_type)` triples.
    ///
    /// Uses `MemoryGraph::to_snapshot()` (the same projection the engine save/
    /// restore path relies on) rather than a per-node neighbor walk, which would
    /// count only outgoing edges and lose the target-side membership.  This is
    /// **not** a tautology: the snapshot is built from the petgraph `DiGraph`
    /// that the conflict-resolution path writes to via `graph.write()`, while the
    /// DB set comes from a fresh SQL `SELECT` — divergence between the two is
    /// exactly the class of bug #437 is tracking.
    fn graph_edge_set(engine: &MemoryEngine) -> HashSet<EdgeKey> {
        engine
            .graph
            .read()
            .to_snapshot()
            .edges
            .into_iter()
            .map(|e| (e.source, e.target, e.relation_type))
            .collect()
    }

    proptest! {
        // 64 cases: each builds a fresh in-memory engine, so the cost is dominated
        // by engine construction (~SQLite init). The `prop_oneof!` over 4 decisions
        // gives only *statistical* per-arm coverage, so 64 (vs 32) drives the
        // probability of starving any one arm to ~(3/4)^64 ≈ 1e-8 while keeping the
        // suite well under 5 s.
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// DB↔graph edge-set consistency after `resolve_conflict`.
        ///
        /// **Property:** for any `CrudDecision` and any number of pre-existing
        /// edges (0–3) incident to the old fact, the set of `(source, target,
        /// relation_type)` triples in the in-memory graph equals the set of
        /// the same triples from `StorageBackend::list_active_edges()` after
        /// `resolve_conflict` returns `Ok`.
        ///
        /// This is a genuine cross-check: the graph is mutated by independent
        /// `graph.write()` code (in `conflict.rs`) while the DB is written by
        /// `resolve_conflict_atomic` inside the storage backend — a regression in
        /// either path produces a divergent set that this test catches.
        #[test]
        fn graph_db_edge_consistency_after_resolve_conflict(
            decision in prop_oneof![
                Just(CrudDecision::Add),
                Just(CrudDecision::Update),
                Just(CrudDecision::Delete),
                Just(CrudDecision::Noop),
            ],
            n_extra_edges in 0usize..=3,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt builds");

            rt.block_on(async {
                let engine = MemoryEngine::builder(DIM)
                    .build()
                    .expect("engine builds");

                // Insert the "old" fact that resolve_conflict targets.
                let old_fact = make_fact("old fact content");
                let old_id = engine
                    .storage()
                    .insert_fact(&old_fact)
                    .await
                    .expect("old fact inserts");

                // Insert `n_extra_edges` side facts and connect them to `old_id`
                // with pre-existing active edges, then sync the in-memory graph.
                // This exercises Update/Delete cascade, which must expire these
                // edges in both the DB and the graph simultaneously.
                let now = chrono::Utc::now();
                for i in 0..n_extra_edges {
                    let side_content = format!("side fact {i}");
                    let side_fact = make_fact(&side_content);
                    let side_id = engine
                        .storage()
                        .insert_fact(&side_fact)
                        .await
                        .expect("side fact inserts");
                    engine
                        .storage()
                        .insert_edge(&NewEdge {
                            source_fact_id: old_id,
                            target_fact_id: side_id,
                            relation_type: "related".into(),
                            weight: 1.0,
                            t_created: now,
                            t_expired: None,
                            scope_id: 1,
                        })
                        .await
                        .expect("pre-existing edge inserts");
                }
                // Sync the in-memory graph to match the DB state after edge
                // insertions.  (The engine starts with an empty graph; raw
                // `insert_edge` calls bypass the engine's graph mirror.)
                if n_extra_edges > 0 {
                    let active = engine
                        .storage()
                        .list_active_edges()
                        .await
                        .expect("list_active_edges pre-resolve");
                    *engine.graph.write() = MemoryGraph::from_active_edges(&active);
                }

                // Candidate fact presented to the arbiter as the "new" version.
                let candidate = make_fact("candidate fact content");

                // Resolve the conflict; ignore the return value — the property
                // holds on the state the engine reaches, not on the return type.
                let _resolution = engine
                    .resolve_conflict(
                        &FixedArbiter { decision },
                        old_id,
                        &candidate,
                    )
                    .await
                    .expect("resolve_conflict succeeds");

                // --- Cross-check: graph set == DB set ---
                //
                // Collect the in-memory graph's edges via `to_snapshot()` (a
                // petgraph `edge_references()` projection — entirely independent
                // of the DB path below).
                let graph_set: HashSet<EdgeKey> = graph_edge_set(&engine);

                // Collect the DB's active edges via a fresh SQL SELECT.
                let db_edges = engine
                    .storage()
                    .list_active_edges()
                    .await
                    .expect("list_active_edges post-resolve");
                let db_set: HashSet<EdgeKey> = db_edges
                    .into_iter()
                    .map(|e| (e.source_fact_id, e.target_fact_id, e.relation_type))
                    .collect();

                // The sets must be identical.  Divergence means either:
                //   • the graph mirror missed an edge the DB committed, or
                //   • the graph mirror kept an edge the DB expired.
                // `prop_assert!` with a pre-formatted string avoids format-capture
                // restrictions inside the `concat!`-expanded `prop_assert_eq!` macro.
                prop_assert!(
                    graph_set == db_set,
                    "{}",
                    format!(
                        "in-memory graph edge set diverges from DB active edges after \
                         resolve_conflict(decision={decision:?}, n_extra_edges={n_extra_edges})\n\
                         graph_only={:?}\ndb_only={:?}",
                        graph_set.difference(&db_set).collect::<Vec<_>>(),
                        db_set.difference(&graph_set).collect::<Vec<_>>(),
                    )
                );

                Ok(()) as proptest::test_runner::TestCaseResult
            })?;
        }
    }
}
