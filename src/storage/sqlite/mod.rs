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
//! - **Async gating (D1):** the whole subtree is `#[cfg(feature = "async")]`;
//!   `spawn_blocking` needs a tokio runtime, so default builds stay runtime-free.
//! - **HNSW (D3):** `vector_search` is brute-force here; HNSW ownership + its
//!   engine-owned dispatch policy move into the backend in `#631`.
//! - **Atomicity (H5):** every trait method is an *independent* write — the engine
//!   composes multi-store transactions *above* this seam, untouched by `#630`.

use std::sync::Arc;

use crate::error::{MemoryError, Result, StorageError};
use crate::pool::ConnectionPool;
use crate::store::upcaster::UpcasterRegistry;

#[cfg(all(feature = "async", feature = "archive"))]
mod cold_storage;
mod consolidation;
mod convert;
mod event_log;
mod graph;
mod schema;
mod search_index;
mod session;

/// The default, in-process `SQLite` implementation of [`StorageBackend`](crate::storage::StorageBackend).
///
/// Holds the pool as `Arc<ConnectionPool>` (the `'static` `spawn_blocking` closures
/// need an owned handle) and the [`UpcasterRegistry`] as `Arc` (cloned into every
/// [`EventLog`](crate::storage::EventLog) closure). `embed_dim` is derived from the
/// pool so the two can never diverge.
pub struct SqliteBackend {
    pool: Arc<ConnectionPool>,
    embed_dim: usize,
    upcaster_registry: Arc<UpcasterRegistry>,
}

impl SqliteBackend {
    /// Wrap an already-opened pool + upcaster registry. The canonical constructor
    /// `#631` will use where it builds the [`ConnectionPool`] today. `embed_dim` is
    /// read from the pool, so a backend's dimension cannot diverge from its pool's.
    #[must_use]
    pub fn from_pool(pool: Arc<ConnectionPool>, upcaster_registry: Arc<UpcasterRegistry>) -> Self {
        let embed_dim = pool.embed_dim();
        Self {
            pool,
            embed_dim,
            upcaster_registry,
        }
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
    /// error the receiver is dropped, the scan's next `blocking_send` fails and it
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

    fn _assert_send_sync() {
        fn f<T: Send + Sync>() {}
        f::<SqliteBackend>();
        f::<UpcasterRegistry>();
        f::<ConnectionPool>();
    }

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
