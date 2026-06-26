//! The default, in-process `SQLite` implementation of the storage port (#630).
//!
//! `SqliteBackend` is the sole owner of every `SQLite`-private concern — the
//! read-pool/write-mutex [`ConnectionPool`] and the event [`UpcasterRegistry`] —
//! and the bounded-trait impls in this module's siblings are a thin, uniform
//! delegation layer over two primitives: [`SqliteBackend::block_read`] and
//! [`SqliteBackend::block_write`], which encapsulate *"acquire a connection and run
//! a sync `rusqlite` closure on a blocking thread"*.
//!
//! ## Why delegation, not absorption
//!
//! The concrete `src/store/*` structs and `src/search/*` free functions stay the
//! SQL's single source of truth (and keep their fast in-process unit tests). This
//! backend *adapts* them: it owns connections and re-homes the borrow→own and
//! sync→async concerns. `#634`'s `PgBackend` reuses none of these bodies, so the
//! SQL must stay below the seam, not be welded into `impl FactGraph`.
//!
//! ## Seam invariants
//!
//! - **Conn selection (D-design):** read methods → [`ConnectionPool::read`]; write
//!   methods → [`ConnectionPool::try_write`] (so a read-only pool rejects writes
//!   with [`MemoryError::ReadOnly`] — Key Design Decision #6, preserved for free).
//! - **No driver type crosses the seam (D4):** a `rusqlite` failure surfaces as
//!   [`MemoryError::Database`] from the concrete store; [`map_seam_err`] maps it to
//!   [`MemoryError::Storage`] wrapping [`StorageError::Backend`] at the boundary.
//!   Semantic variants (`NotFound`, `Migration`, `EmbeddingDimension`, `Conflict`,
//!   `ReadOnly`, `Internal`, …) already have a precise home and pass through.
//! - **Async (D1):** the backend is async-native — `spawn_blocking` needs a tokio
//!   runtime, which is now non-optional (the engine has no synchronous path). The
//!   former `#[cfg(feature = "async")]` gating was removed in #702.
//! - **HNSW (D3):** `vector_search` is brute-force here; HNSW ownership + its
//!   engine-owned dispatch policy move into the backend in `#631`.
//! - **Atomicity (H5):** every trait method is an *independent* write — the engine
//!   composes multi-store transactions *above* this seam, untouched by `#630`.

use std::sync::Arc;

use crate::error::{MemoryError, Result, StorageError};
use crate::pool::ConnectionPool;
#[cfg(feature = "ann")]
use crate::search::ann::HnswStrategy;
use crate::search::strategy::SearchConfig;
#[cfg(feature = "ann")]
use crate::search::strategy::VectorSearchStrategy as _;
use crate::store::upcaster::UpcasterRegistry;

#[cfg(feature = "archive")]
mod cold_storage;
mod consolidation;
mod convert;
mod event_log;
mod graph;
#[cfg(test)]
mod realization;
mod schema;
mod search_index;
mod session;

/// The default, in-process `SQLite` implementation of [`StorageBackend`](crate::storage::StorageBackend).
///
/// Holds the pool as `Arc<ConnectionPool>` (the `'static` `spawn_blocking` closures
/// need an owned handle) and the [`UpcasterRegistry`] as `Arc` (cloned into every
/// [`EventLog`](crate::storage::EventLog) closure). `embed_dim` is derived from the
/// pool so the two can never diverge.
///
/// ## HNSW ownership (Stage B, `ann` feature)
///
/// When the `ann` feature is enabled and a [`SearchConfig`] with
/// `ann_threshold < usize::MAX` is provided via [`SqliteBackend::with_search_config`],
/// an [`HnswStrategy`] is built from the database and stored here. `vector_search`
/// dispatches to HNSW when `active_count() >= ann_threshold`, mirroring the engine's
/// `should_use_hnsw()` predicate exactly. Without a `search_config` (the default),
/// `vector_search` is always brute-force, preserving the `#630` behavior.
///
/// The HNSW index is maintained incrementally: `notify_insert` is called after every
/// successful fact write (post-commit), and `notify_expire` after every expiry or hard
/// delete — matching the engine's post-commit ordering in `ingest.rs` / `cognitive.rs`.
pub struct SqliteBackend {
    pool: Arc<ConnectionPool>,
    embed_dim: usize,
    upcaster_registry: Arc<UpcasterRegistry>,
    /// Optional vector search configuration — drives the HNSW dispatch predicate.
    ///
    /// `None` (the default from `from_pool`) ⇒ `ann_threshold` is effectively
    /// `usize::MAX`, so HNSW never activates and `vector_search` is always
    /// brute-force.
    #[cfg_attr(not(feature = "ann"), allow(dead_code))]
    search_config: Option<SearchConfig>,
    /// Owned HNSW index, wrapped in `Arc` so the `'static` `spawn_blocking`
    /// closures in `vector_search` can clone the reference without unsafe code.
    /// Present only when the `ann` feature is enabled **and**
    /// `with_search_config` was called with `ann_threshold < usize::MAX`.
    #[cfg(feature = "ann")]
    hnsw: Option<Arc<HnswStrategy>>,
}

// Build-time witness (not test-gated): `SqliteBackend` must be `Send + Sync` for
// `Arc<dyn StorageBackend>` and the `#[async_trait]` `Send` futures. A field that
// breaks this fails `cargo build`, not merely `cargo test`.
//
// `HnswStrategy` is `Send + Sync` because its interior `RwLock` (parking_lot) is
// `Send + Sync`, and its `Hnsw<…>` is proven `Send + Sync` in `ann.rs:hnsw_is_send_sync`.
const _: fn() = || {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SqliteBackend>();
};

/// Where the engine's open path sources the HNSW index when building the
/// backend (#631). Keeps the snapshot-vs-rebuild branching below the seam.
pub enum HnswOpenSource {
    /// No/stale sidecar snapshot — build the index from a full DB scan.
    Rebuild,
    /// A validated sidecar snapshot — restore the index from this payload. The
    /// inner `Option` is the sidecar's HNSW blob (`None` when the sidecar
    /// predates HNSW, in which case the index falls back to a DB rebuild).
    Snapshot(Option<crate::engine::snapshot::HnswSnapshot>),
}

impl SqliteBackend {
    /// Wrap an already-opened pool + upcaster registry. The canonical constructor
    /// `#631` will use where it builds the [`ConnectionPool`] today. `embed_dim` is
    /// read from the pool, so a backend's dimension cannot diverge from its pool's.
    ///
    /// The backend produced by this constructor has no `SearchConfig`, so HNSW never
    /// activates and `vector_search` is always brute-force — identical to the `#630`
    /// behavior. Use [`with_search_config`](Self::with_search_config) to opt into HNSW.
    #[must_use]
    pub fn from_pool(pool: Arc<ConnectionPool>, upcaster_registry: Arc<UpcasterRegistry>) -> Self {
        let embed_dim = pool.embed_dim();
        Self {
            pool,
            embed_dim,
            upcaster_registry,
            search_config: None,
            #[cfg(feature = "ann")]
            hnsw: None,
        }
    }

    /// Attach a [`SearchConfig`] and, when the `ann` feature is enabled and
    /// `cfg.ann_threshold < usize::MAX`, build the HNSW index from the database.
    ///
    /// This is the builder that replicates the engine's `init_from_pool` HNSW
    /// construction (`engine/mod.rs:272-284`). It acquires a read connection,
    /// runs the full `SELECT id, embedding FROM facts WHERE t_expired IS NULL`
    /// scan, and constructs an in-memory index.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on a pool or query failure, or
    /// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
    // Non-ann builds see only `self.search_config = Some(cfg); Ok(self)` which
    // clippy flags as `missing_const_for_fn`. It is not const: under `ann` the
    // method runs fallible DB I/O, and `const fn` cannot be conditionally const
    // across feature flags. Suppress the FP on the non-ann build.
    #[allow(
        clippy::missing_const_for_fn,
        reason = "under `ann` this runs fallible DB I/O; \
                  the method cannot be `const` across all feature combos"
    )]
    pub fn with_search_config(mut self, cfg: SearchConfig) -> Result<Self> {
        #[cfg(feature = "ann")]
        {
            self.hnsw = if cfg.ann_threshold < usize::MAX {
                let conn = self.pool.read()?;
                Some(Arc::new(HnswStrategy::build_from_db(
                    &conn,
                    self.embed_dim,
                )?))
            } else {
                None
            };
        }
        self.search_config = Some(cfg);
        Ok(self)
    }

    /// Open-time backend setup (the engine's `init_from_pool` HNSW logic, now
    /// owned below the seam). Sets the `SearchConfig` and, under `ann` with
    /// `ann_threshold < usize::MAX`, materializes the HNSW index from the
    /// requested [`HnswOpenSource`]:
    ///
    /// - [`HnswOpenSource::Snapshot(Some(snap))`] → restore from the sidecar blob.
    /// - [`HnswOpenSource::Snapshot(None)`] or [`HnswOpenSource::Rebuild`] → build
    ///   from a full DB scan.
    ///
    /// Mirrors `engine/mod.rs::try_load_snapshot`'s match arms exactly so the
    /// cutover preserves open-time HNSW behavior bit-for-bit.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on a pool/query failure or
    /// `MemoryError::EmbeddingDimension` on a corrupt snapshot/stored embedding.
    #[cfg_attr(
        not(feature = "ann"),
        allow(
            clippy::missing_const_for_fn,
            unused_variables,
            reason = "non-ann build only stores search_config; HNSW args are inert"
        )
    )]
    pub fn with_open_config(
        mut self,
        search_config: Option<SearchConfig>,
        hnsw_source: HnswOpenSource,
    ) -> Result<Self> {
        #[cfg(feature = "ann")]
        {
            let ann_wanted = search_config
                .as_ref()
                .is_some_and(|c| c.ann_threshold < usize::MAX);
            self.hnsw = if ann_wanted {
                match hnsw_source {
                    HnswOpenSource::Snapshot(Some(snap)) => Some(Arc::new(
                        HnswStrategy::from_snapshot(&snap, self.embed_dim)?,
                    )),
                    HnswOpenSource::Snapshot(None) | HnswOpenSource::Rebuild => {
                        let conn = self.pool.read()?;
                        Some(Arc::new(HnswStrategy::build_from_db(
                            &conn,
                            self.embed_dim,
                        )?))
                    }
                }
            } else {
                None
            };
        }
        self.search_config = search_config;
        Ok(self)
    }

    /// Replicate the engine's `should_use_hnsw()` predicate exactly:
    /// `hnsw.is_some_and(|h| h.active_count() >= ann_threshold)`.
    ///
    /// When the `ann` feature is disabled, or when no `search_config` was set,
    /// this is always `false` (brute-force).
    ///
    /// The extra guard `filter_is_hnsw_compatible` ensures the filter does not
    /// carry predicates that HNSW's `check_fact_filters` cannot honour (pinned,
    /// metadata, ids, non-Active temporal). In those cases the richer brute-force
    /// SQL path is used so no result is incorrectly included or excluded.
    #[cfg(feature = "ann")]
    fn should_use_hnsw(&self, filter: &crate::storage::FactFilter) -> bool {
        use crate::storage::TemporalFilter;
        // Replication of `engine/mod.rs:379-387` + filter-compatibility guard.
        // HNSW's `check_fact_filters` only handles:
        //   t_expired IS NULL  +  fact_type  +  scope_ids
        // Any extra dimension (pinned, metadata, ids, non-Active temporal) must
        // fall through to the full brute-force SQL path.
        let filter_compatible = filter.temporal == TemporalFilter::Active
            && filter.ids.is_none()
            && filter.pinned.is_none()
            && filter.metadata.is_empty();
        if !filter_compatible {
            return false;
        }
        self.hnsw.as_ref().is_some_and(|h| {
            h.active_count()
                >= self
                    .search_config
                    .as_ref()
                    .map_or(usize::MAX, |c| c.ann_threshold)
        })
    }

    #[cfg(not(feature = "ann"))]
    #[allow(
        dead_code,
        clippy::unused_self,
        reason = "non-ann build: method exists for symmetry but is never called; \
                  the ann twin uses self.hnsw / self.search_config"
    )]
    const fn should_use_hnsw(&self, _filter: &crate::storage::FactFilter) -> bool {
        false
    }

    /// Serialize the in-memory HNSW index to a snapshot (reads active embeddings
    /// from the database via a read connection).
    ///
    /// Returns `Ok(None)` if HNSW is not active (no `search_config`, `ann` feature
    /// disabled, or `ann_threshold == usize::MAX`). This is the piece that Stage E
    /// wires into the engine's `close()`/snapshot path.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` on pool or query failure, or
    /// `MemoryError::EmbeddingDimension` if a stored embedding has the wrong size.
    #[cfg(feature = "ann")]
    pub fn hnsw_snapshot(&self) -> Result<Option<crate::engine::snapshot::HnswSnapshot>> {
        let Some(ref hnsw) = self.hnsw else {
            return Ok(None);
        };
        let conn = self.pool.read()?;
        hnsw.to_snapshot(&conn, self.embed_dim).map(Some)
    }

    /// Rebuild the HNSW index from a snapshot produced by
    /// [`hnsw_snapshot`](Self::hnsw_snapshot), discarding the current in-memory
    /// index.
    ///
    /// This replicates `engine/mod.rs:334-353`'s `try_load_snapshot` HNSW path.
    /// Returns `Ok(())` if HNSW is not active (no-op for non-ann builds or
    /// `ann_threshold == usize::MAX`).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::EmbeddingDimension` if a snapshot entry has the wrong
    /// embedding size (corrupt/version-skewed snapshot).
    #[cfg(feature = "ann")]
    pub fn load_hnsw_snapshot(
        &mut self,
        snap: &crate::engine::snapshot::HnswSnapshot,
    ) -> Result<()> {
        // Only meaningful if the config requires ANN.
        let ann_threshold = self
            .search_config
            .as_ref()
            .map_or(usize::MAX, |c| c.ann_threshold);
        if ann_threshold == usize::MAX {
            return Ok(());
        }
        self.hnsw = Some(Arc::new(HnswStrategy::from_snapshot(snap, self.embed_dim)?));
        Ok(())
    }
}

impl SqliteBackend {
    /// Notify the HNSW index that a fact was inserted (post-commit).
    ///
    /// Mirrors the engine's post-commit `notify_insert` calls in `ingest.rs:235-238`
    /// and `cognitive.rs:362-364`. Must be called **after** the write has committed
    /// and the write lock has been released (matching the engine's ordering).
    ///
    /// No-op when the `ann` feature is disabled or when no HNSW index is active.
    #[cfg(feature = "ann")]
    pub(super) fn hnsw_notify_insert(&self, fact_id: i64, embedding: &[f32]) {
        if let Some(ref hnsw) = self.hnsw {
            hnsw.notify_insert(fact_id, embedding);
        }
    }

    /// Notify the HNSW index that a fact was expired or hard-deleted (post-commit).
    ///
    /// Mirrors the engine's `notify_expire` calls. Must be called **after** the write
    /// has committed and the write lock has been released.
    ///
    /// No-op when the `ann` feature is disabled or when no HNSW index is active.
    #[cfg(feature = "ann")]
    pub(super) fn hnsw_notify_expire(&self, fact_id: i64) {
        if let Some(ref hnsw) = self.hnsw {
            hnsw.notify_expire(fact_id);
        }
    }

    /// Rebuild the in-memory HNSW index from the current active facts (#624).
    ///
    /// Called after a same-dim reconstruction promote rewrote every
    /// `facts.embedding` under a new model: the graph (built on the old vectors) is
    /// stale. Runs the CPU-heavy O(N) rebuild on a blocking thread via
    /// [`block_read`](Self::block_read); [`HnswStrategy::rebuild_from_db`] builds the
    /// fresh index under its write lock and swaps it in atomically. No-op when no
    /// HNSW index is active (brute-force mode reads `facts.embedding` directly and is
    /// already correct).
    ///
    /// The `Arc<HnswStrategy>` is cloned into the `'static` blocking closure (a cheap
    /// pointer bump), mirroring `vector_search`'s HNSW dispatch.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Storage`] on a backend failure, or
    /// [`MemoryError::EmbeddingDimension`] if a stored embedding has the wrong width.
    #[cfg(feature = "ann")]
    pub(super) async fn hnsw_rebuild_from_db(&self) -> Result<()> {
        let Some(hnsw) = self.hnsw.clone() else {
            return Ok(()); // no HNSW active → brute-force path needs no rebuild
        };
        self.block_read(move |conn| hnsw.rebuild_from_db(conn))
            .await
    }
}

/// Map a tokio [`JoinError`](tokio::task::JoinError) (a panic or cancellation in the
/// blocking task) to [`MemoryError::Pool`] — byte-identical to
/// `async_engine::join_err`, so a panic surfaces the same way across both seams.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as map_err(map_join) fn pointer"
)]
fn map_join(e: tokio::task::JoinError) -> MemoryError {
    MemoryError::Pool(format!("task join error: {e}"))
}

/// D4: confine `rusqlite` below the seam. A raw driver failure
/// ([`MemoryError::Database`]) becomes opaque [`StorageError::Backend`]; every
/// semantic variant (which has a precise `MemoryError` home) passes through.
fn map_seam_err<T>(r: Result<T>) -> Result<T> {
    match r {
        Err(MemoryError::Database(e)) => {
            Err(MemoryError::Storage(StorageError::Backend(e.to_string())))
        }
        other => other,
    }
}

impl SqliteBackend {
    /// Acquire a READ connection and run `f` on a blocking thread.
    ///
    /// The pool `Arc` is cloned in; the `!Send` read guard is acquired *inside* the
    /// closure (acquiring it on the executor would not compile and would serialize
    /// the runtime). Driver errors are mapped at the boundary by [`map_seam_err`].
    async fn block_read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
    {
        let pool = Arc::clone(&self.pool);
        let out = tokio::task::spawn_blocking(move || {
            let conn = pool.read()?;
            f(&conn)
        })
        .await
        .map_err(map_join)?;
        map_seam_err(out)
    }

    /// Acquire the WRITE connection (via [`ConnectionPool::try_write`], so a
    /// read-only pool yields [`MemoryError::ReadOnly`]) and run `f` on a blocking
    /// thread.
    async fn block_write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
    {
        let pool = Arc::clone(&self.pool);
        let out = tokio::task::spawn_blocking(move || {
            let conn = pool.try_write()?;
            f(&conn)
        })
        .await
        .map_err(map_join)?;
        map_seam_err(out)
    }

    /// Stream rows produced by a blocking `&Connection` scan to a non-`'static`,
    /// borrowing async-side callback, with O(1) peak memory.
    ///
    /// `scan` runs on a blocking thread and `blocking_send`s every row into a cap-1
    /// channel; the async side `recv().await`s and invokes `cb`. The cap-1 bound is
    /// backpressure (the scan stalls when the consumer is slow). On an early `cb`
    /// error the receiver is dropped, so the scan's next `blocking_send` fails (or
    /// the scan finishes naturally if the cursor was already exhausted) and it
    /// stops — and the **callback** error is returned in preference to the scan's
    /// resulting send failure. A mid-scan SQL error (no callback error) is surfaced
    /// from the join handle.
    async fn for_each_streamed<T, S>(
        &self,
        scan: S,
        cb: &mut (dyn FnMut(T) -> Result<()> + Send),
    ) -> Result<()>
    where
        T: Send + 'static,
        S: FnOnce(&rusqlite::Connection, &tokio::sync::mpsc::Sender<T>) -> Result<()>
            + Send
            + 'static,
    {
        let pool = Arc::clone(&self.pool);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<T>(1);
        let handle = tokio::task::spawn_blocking(move || {
            let conn = pool.read()?;
            scan(&conn, &tx)
        });

        let mut cb_err: Option<MemoryError> = None;
        while let Some(row) = rx.recv().await {
            if let Err(e) = cb(row) {
                cb_err = Some(e);
                break;
            }
        }
        drop(rx); // unblock / abort the scan's next blocking_send

        let scan_res = map_seam_err(handle.await.map_err(map_join)?);
        // The callback error wins over the scan's induced send failure; otherwise
        // surface any mid-scan SQL error.
        cb_err.map_or(scan_res, Err)
    }
}

/// Map a "stream consumer dropped" send failure to an internal error. The value is
/// never surfaced (an early callback error always wins in
/// [`SqliteBackend::for_each_streamed`]); it exists only to stop the scan.
fn stream_consumer_dropped() -> MemoryError {
    MemoryError::Internal("storage stream consumer dropped mid-scan".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory backend for tests (`embed_dim` 4).
    pub(super) fn memory_backend() -> SqliteBackend {
        let pool = ConnectionPool::open_memory(4).unwrap();
        SqliteBackend::from_pool(Arc::new(pool), Arc::new(UpcasterRegistry::new()))
    }

    #[tokio::test]
    async fn block_read_runs_and_returns() {
        let be = memory_backend();
        let n: i64 = be
            .block_read(|c| Ok(c.query_row("SELECT 1 + 1", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn block_read_maps_database_error_to_storage_backend() {
        // D4 witness, at the precise boundary where the mapping lives: a Database
        // error returned from the closure must surface as Storage(Backend).
        let be = memory_backend();
        let err = be
            .block_read(|_| -> Result<()> {
                Err(MemoryError::Database(rusqlite::Error::QueryReturnedNoRows))
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::Storage(StorageError::Backend(_))),
            "expected Storage(Backend), got {err:?}"
        );
    }

    #[tokio::test]
    async fn block_read_passes_semantic_variant_through() {
        // A semantic variant must NOT be opacified by the seam remap.
        let be = memory_backend();
        let err = be
            .block_read(|_| -> Result<()> { Err(MemoryError::NotFound("fact 7".into())) })
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn block_read_panic_maps_to_pool() {
        let be = memory_backend();
        let err = be
            .block_read(|_| -> Result<()> { panic!("boom") })
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Pool(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn block_write_on_read_only_pool_yields_read_only() {
        // H7: a write through a read-only pool must yield ReadOnly (via try_write).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.db");
        // Initialize a real file-backed store, then drop it.
        {
            let _rw = ConnectionPool::open(&path, 4, 2, None).unwrap();
        }
        let ro = ConnectionPool::open_read_only(&path, 4, 2).unwrap();
        let be = SqliteBackend::from_pool(Arc::new(ro), Arc::new(UpcasterRegistry::new()));
        let err = be
            .block_write(|c| Ok(c.execute("CREATE TABLE t (x)", [])?))
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::ReadOnly), "got {err:?}");
        // Reads still work on a read-only backend.
        let one: i64 = be
            .block_read(|c| Ok(c.query_row("SELECT 1", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(one, 1);
    }

    #[tokio::test]
    async fn for_each_streamed_delivers_in_order() {
        let be = memory_backend();
        let mut seen = Vec::new();
        be.for_each_streamed(
            |_conn, tx| {
                for i in 0..5_i64 {
                    tx.blocking_send(i).map_err(|_| stream_consumer_dropped())?;
                }
                Ok(())
            },
            &mut |row: i64| {
                seen.push(row);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn for_each_streamed_callback_error_wins_and_stops_early() {
        // H6: callback Err at row k ⇒ that exact Err propagates, and exactly k rows
        // were observed (early stop), not the scan's induced send failure.
        let be = memory_backend();
        let mut count = 0_usize;
        let err = be
            .for_each_streamed(
                |_conn, tx| {
                    for i in 0..100_i64 {
                        tx.blocking_send(i).map_err(|_| stream_consumer_dropped())?;
                    }
                    Ok(())
                },
                &mut |_row: i64| {
                    count += 1;
                    if count == 3 {
                        return Err(MemoryError::Lineage("stop at 3".into()));
                    }
                    Ok(())
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::Lineage(ref m) if m == "stop at 3"),
            "callback error must win, got {err:?}"
        );
        assert_eq!(count, 3, "must stop early at the erroring row");
    }

    #[tokio::test]
    async fn for_each_streamed_surfaces_mid_scan_error() {
        // A scan SQL error (no callback error) is surfaced, remapped to Storage(Backend).
        let be = memory_backend();
        let err = be
            .for_each_streamed(
                |_conn, tx| {
                    tx.blocking_send(0_i64)
                        .map_err(|_| stream_consumer_dropped())?;
                    Err(MemoryError::Database(rusqlite::Error::QueryReturnedNoRows))
                },
                &mut |_row: i64| Ok(()),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::Storage(StorageError::Backend(_))),
            "got {err:?}"
        );
    }
}
