use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, MutexGuard};
use rusqlite::Connection;

use crate::error::{MemoryError, MigrationError, Result};
use crate::store::schema::{
    init_schema, migrate, open_connection, open_connection_read_only,
    open_memory as open_memory_conn,
};

/// Default bound on how long [`ConnectionPool::read`] waits for a read
/// connection to become available before failing with [`MemoryError::Pool`].
///
/// Without a bound the acquire path would block forever on the `Condvar` when
/// every read connection is checked out and never returned (e.g. a deadlocked
/// or leaked guard). A finite default turns that silent hang into a clear,
/// observable error. 30s is generous for healthy contention yet short enough
/// to surface a genuine leak.
const DEFAULT_READ_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Validate a caller-supplied `read_pool_size` for a file-backed pool.
///
/// Enforces the lower bound (`>= 1`): a file-backed pool needs at least one
/// read connection, and `0` is *not* an implicit in-memory request — see
/// [`BackendMode`] (#340/#356). Returns the validated value so callers can use
/// it directly.
///
/// # Errors
///
/// Returns [`MemoryError::Pool`] if `read_pool_size == 0`.
fn validate_read_pool_size(read_pool_size: usize) -> Result<usize> {
    if read_pool_size == 0 {
        return Err(MemoryError::Pool(
            "read_pool_size must be >= 1 for a file-backed pool; use \
             ConnectionPool::open_memory for an in-memory database"
                .to_string(),
        ));
    }
    Ok(read_pool_size)
}

/// Storage backend a [`ConnectionPool`] is wired to.
///
/// This replaces the previous `read_pool_size == 0` sentinel (#356): a zero
/// read-pool size used to *imply* in-memory mode, which conflated the two
/// orthogonal facts ("is this in-memory?" vs "how many read connections?") and
/// let a file-backed `open(.., 0, ..)` silently route every read through the
/// write connection (#340). The discriminant is now an explicit variant the
/// read path matches on, so the two modes can never be confused.
enum BackendMode {
    /// File-backed: `read_pool_size` pooled read connections (always `>= 1`,
    /// enforced at construction) plus the exclusive write connection.
    FileBacked { read_pool_size: usize },
    /// In-memory: a single shared connection serves both reads and writes
    /// (reads are serialized through the write `Mutex`).
    InMemory,
}

/// A connection pool with N read connections and 1 write connection.
///
/// `SQLite` WAL supports concurrent readers with a single writer.
/// The pool is bounded — `read()` blocks if all connections are checked out.
///
/// In-memory databases use the write connection for reads (serialized).
pub struct ConnectionPool {
    write_conn: Mutex<Connection>,
    read_conns: Mutex<Vec<Connection>>,
    read_available: Condvar,
    path: Option<PathBuf>,
    #[allow(dead_code)] // complete pool API — used after #108 engine split
    embed_dim: usize,
    /// Which backend this pool drives. The canonical discriminant for the
    /// in-memory vs file-backed read dispatch (replaces the old
    /// `read_pool_size == 0` sentinel — see [`BackendMode`]).
    mode: BackendMode,
    read_only: bool,
    /// Bound on the wait for a read connection in [`Self::read`]. Defaults to
    /// [`DEFAULT_READ_ACQUIRE_TIMEOUT`]; exposed as a field so a future
    /// `EngineConfig::read_acquire_timeout` can be wired in without touching
    /// the acquire path.
    read_acquire_timeout: Duration,
}

// Debug-only, per-thread set of pool addresses on which *this* thread currently
// holds the write guard. Used to detect the in-memory reentrant deadlock (#278)
// without false positives: the marker is keyed by pool identity AND is
// thread-local, so legitimate cross-thread write/read contention never trips it,
// and a write guard on one pool does not implicate reads on another. Compiled
// out entirely in release builds.
#[cfg(debug_assertions)]
thread_local! {
    static WRITE_HELD: std::cell::RefCell<std::collections::HashSet<usize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// RAII guard for the write connection.
///
/// Wraps the underlying `MutexGuard` and `Deref`s to [`Connection`]
/// transparently, so callers use it exactly like the raw guard. In debug builds
/// it additionally records that the current thread holds this pool's write lock
/// (and clears that record on drop) so [`ConnectionPool::read`] can assert
/// against the in-memory reentrant deadlock (#278). In release builds it is a
/// zero-overhead newtype around the `MutexGuard`.
pub struct WriteGuard<'a> {
    guard: MutexGuard<'a, Connection>,
    /// Pool identity, used only by the debug reentrancy detector.
    #[cfg(debug_assertions)]
    pool_addr: usize,
}

impl std::ops::Deref for WriteGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.guard
    }
}

impl std::ops::DerefMut for WriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.guard
    }
}

impl std::fmt::Debug for WriteGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Delegate to the inner `Connection` so `Result<WriteGuard, _>` keeps
        // the `Debug` bound the previous `MutexGuard<Connection>` return type
        // satisfied (e.g. `.unwrap_err()` in tests).
        f.debug_tuple("WriteGuard").field(&*self.guard).finish()
    }
}

#[cfg(debug_assertions)]
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        WRITE_HELD.with(|held| {
            held.borrow_mut().remove(&self.pool_addr);
        });
    }
}

/// RAII guard that returns a read connection to the pool on drop.
pub struct ReadGuard<'a> {
    conn: Option<Connection>,
    pool: &'a ConnectionPool,
}

impl std::ops::Deref for ReadGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("ReadGuard conn already returned")
    }
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.read_conns.lock().push(conn);
            self.pool.read_available.notify_one();
        }
    }
}

/// RAII read guard for in-memory mode: a *serialized* read through the shared
/// write connection's `MutexGuard`.
///
/// The name reflects the semantics, not a capability (#357): in-memory mode has
/// no separate read-only connection, so a "read" simply locks the single
/// read-write `write_conn`. The wrapped [`Connection`] is therefore fully
/// writable and carries **no** `PRAGMA query_only = ON` — unlike the file-backed
/// read connections, which are opened read-only at the `SQLite` level. The guard
/// is named `SerializedReadGuard` (not the misleading `WriteAsReadGuard`) so
/// callers do not mistake it for a read-only handle; the read path holds the
/// write `Mutex`, which is what serializes concurrent in-memory reads.
pub struct SerializedReadGuard<'a> {
    guard: MutexGuard<'a, Connection>,
}

impl std::ops::Deref for SerializedReadGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.guard
    }
}

/// Enum over read guard types — file-backed (pooled) vs in-memory (write conn).
pub enum ReadConn<'a> {
    Pooled(ReadGuard<'a>),
    /// In-memory read: the write connection reused under serialization. The
    /// inner connection is writable and has no `query_only` PRAGMA (#357).
    InMemory(SerializedReadGuard<'a>),
}

impl std::ops::Deref for ReadConn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            ReadConn::Pooled(g) => g,
            ReadConn::InMemory(g) => g,
        }
    }
}

impl ConnectionPool {
    /// Open a file-backed pool with N read connections + 1 write connection.
    ///
    /// All connections go through `open_connection()` which sets all PRAGMAs
    /// (WAL, `foreign_keys`, `busy_timeout`, synchronous).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Pool` if `read_pool_size` is `0`: a file-backed
    /// pool needs at least one read connection, and `0` is *not* a covert
    /// request for in-memory mode (use [`open_memory`](Self::open_memory) for
    /// that). Accepting it would yield a file-backed pool that serializes every
    /// read through the write connection (#340/#356).
    /// Returns `MemoryError::Database` if any connection or schema setup fails.
    /// Returns `MemoryError::Migration` if the schema version cannot be determined
    /// or the stored version is newer than the compiled-in maximum.
    /// Returns `MemoryError::UnsupportedEpoch` if the database was written by a
    /// future version of the engine.
    pub fn open(
        path: &Path,
        embed_dim: usize,
        read_pool_size: usize,
        backup_dir: Option<&Path>,
    ) -> Result<Self> {
        let read_pool_size = validate_read_pool_size(read_pool_size)?;

        let path_str = path.to_string_lossy();
        let write_conn = open_connection(&path_str)?;
        init_schema(&write_conn)?;
        migrate(&write_conn, backup_dir)?;

        let mut read_conns = Vec::with_capacity(read_pool_size);
        for _ in 0..read_pool_size {
            let conn = open_connection(&path_str)?;
            conn.execute_batch("PRAGMA query_only = ON")
                .map_err(MemoryError::Database)?;
            read_conns.push(conn);
        }

        Ok(Self {
            write_conn: Mutex::new(write_conn),
            read_conns: Mutex::new(read_conns),
            read_available: Condvar::new(),
            path: Some(path.to_path_buf()),
            embed_dim,
            mode: BackendMode::FileBacked { read_pool_size },
            read_only: false,
            read_acquire_timeout: DEFAULT_READ_ACQUIRE_TIMEOUT,
        })
    }

    /// Open an in-memory pool (single connection for both reads and writes).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Database` if connection or schema setup fails.
    pub fn open_memory(embed_dim: usize) -> Result<Self> {
        let conn = open_memory_conn()?;
        init_schema(&conn)?;
        migrate(&conn, None)?;
        Ok(Self {
            write_conn: Mutex::new(conn),
            read_conns: Mutex::new(Vec::new()),
            read_available: Condvar::new(),
            path: None,
            embed_dim,
            mode: BackendMode::InMemory,
            read_only: false,
            read_acquire_timeout: DEFAULT_READ_ACQUIRE_TIMEOUT,
        })
    }

    /// Open a file-backed pool in read-only mode.
    ///
    /// The database file must already exist and have been initialized by a prior
    /// read-write open. This constructor validates schema compatibility without
    /// running `init_schema()` or `migrate()`.
    ///
    /// All connections are opened with `SQLITE_OPEN_READ_ONLY`, which enforces
    /// read-only access at the OS level. No `PRAGMA query_only` is set.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Pool` if `read_pool_size` is `0` (same footgun as
    /// [`open`](Self::open) — a file-backed pool needs at least one read
    /// connection; #340/#356).
    /// Returns `MemoryError::Database` if any connection or pragma setup fails.
    /// Returns `MemoryError::Migration` if the file does not exist, the schema
    /// is uninitialized, or the schema needs migration.
    /// Returns `MemoryError::UnsupportedEpoch` if the DB epoch is from the future.
    pub fn open_read_only(path: &Path, embed_dim: usize, read_pool_size: usize) -> Result<Self> {
        use crate::store::schema::validate_schema_version;

        let read_pool_size = validate_read_pool_size(read_pool_size)?;

        // Reject nonexistent or non-file paths before SQLite can act
        if !path.is_file() {
            return Err(MigrationError::Incompatible(format!(
                "database path is not a regular file: {}; cannot open read-only",
                path.display()
            ))
            .into());
        }

        // Open with SQLITE_OPEN_READ_ONLY — no file creation, no WAL mutation
        let path_str = path.to_string_lossy();
        let conn = open_connection_read_only(&path_str)?;
        validate_schema_version(&conn)?;

        let mut read_conns = Vec::with_capacity(read_pool_size);
        for _ in 0..read_pool_size {
            let c = open_connection_read_only(&path_str)?;
            read_conns.push(c);
        }

        Ok(Self {
            write_conn: Mutex::new(conn),
            read_conns: Mutex::new(read_conns),
            read_available: Condvar::new(),
            path: Some(path.to_path_buf()),
            embed_dim,
            mode: BackendMode::FileBacked { read_pool_size },
            read_only: true,
            read_acquire_timeout: DEFAULT_READ_ACQUIRE_TIMEOUT,
        })
    }

    /// Checkout a read connection.
    ///
    /// - File-backed: pops from the bounded pool. If exhausted, waits on a
    ///   `Condvar` for a connection to be returned, up to
    ///   `read_acquire_timeout` (default [`DEFAULT_READ_ACQUIRE_TIMEOUT`]).
    /// - In-memory: locks the write connection (serialized but correct).
    ///
    /// The happy path (a connection available immediately) returns without
    /// waiting and never errors.
    ///
    /// # In-memory reentrancy hazard (#278)
    ///
    /// In in-memory mode this locks the **same** `Mutex` as
    /// [`write`](Self::write)/[`try_write`](Self::try_write). The
    /// `parking_lot::Mutex` is non-reentrant, so calling `read()` on the same
    /// thread that already holds a write guard self-deadlocks (a release-build
    /// hang; a debug-build reentrancy panic). Always drop the write guard before
    /// reading on an in-memory pool. File-backed pools are immune (reads use a
    /// separate connection pool).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if no read connection becomes available
    /// within `read_acquire_timeout` — turning a previously-unbounded hang
    /// (e.g. a leaked or deadlocked guard) into an observable failure.
    pub fn read(&self) -> Result<ReadConn<'_>> {
        // Dispatch on the canonical backend discriminant, never on a derived
        // count (#340/#356). In-memory mode has no pooled read connections, so
        // a read serializes through the single shared write connection.
        let read_pool_size = match self.mode {
            BackendMode::InMemory => {
                // In-memory mode locks the write connection here. The
                // `parking_lot::Mutex` is non-reentrant, so a thread that holds
                // a `write()` guard MUST NOT call `read()` on the same thread —
                // doing so self-deadlocks (#278). Catch the violation at dev
                // time with a per-pool, per-thread marker before the blocking
                // `lock()` would hang forever; compiled out in release.
                #[cfg(debug_assertions)]
                {
                    let pool_addr = std::ptr::from_ref::<Self>(self) as usize;
                    let held = WRITE_HELD.with(|h| h.borrow().contains(&pool_addr));
                    assert!(
                        !held,
                        "reentrant deadlock: read() called on an in-memory pool while this \
                         thread holds its write() guard — read() locks the same non-reentrant \
                         Mutex (#278)"
                    );
                }
                let guard = self.write_conn.lock();
                return Ok(ReadConn::InMemory(SerializedReadGuard { guard }));
            }
            BackendMode::FileBacked { read_pool_size } => read_pool_size,
        };

        let mut conns = self.read_conns.lock();
        // Pre-compute an absolute deadline so the total wait is strictly
        // bounded by `read_acquire_timeout`. `wait_for` would reset the full
        // timeout on every loop iteration, so under repeated spurious wakeups
        // (or notify-then-raced-steal) the thread could block far longer than
        // configured — even indefinitely. `wait_until` shares one fixed
        // ceiling across all iterations.
        let deadline = Instant::now() + self.read_acquire_timeout;
        while conns.is_empty() {
            // `wait_until` returns once notified, or when `deadline` passes.
            // `timed_out()` distinguishes the two; re-check `is_empty()` to
            // guard against spurious wakeups (notified but raced).
            if self
                .read_available
                .wait_until(&mut conns, deadline)
                .timed_out()
                && conns.is_empty()
            {
                return Err(MemoryError::Pool(format!(
                    "read pool acquire timed out after {:?} (all {read_pool_size} connections checked out)",
                    self.read_acquire_timeout
                )));
            }
        }
        let conn = conns.pop().expect("condvar woke but pool empty");
        Ok(ReadConn::Pooled(ReadGuard {
            conn: Some(conn),
            pool: self,
        }))
    }

    /// Lock the write connection.
    ///
    /// # In-memory reentrancy hazard (#278)
    ///
    /// In in-memory mode [`read`](Self::read) locks this same connection. The
    /// `parking_lot::Mutex` is **non-reentrant**, so a thread that holds the
    /// guard returned here MUST NOT call `read()` on the same thread — doing so
    /// self-deadlocks (in release it hangs forever; in debug a reentrancy
    /// assertion fires). File-backed pools are unaffected (reads come from a
    /// separate pool).
    pub fn write(&self) -> WriteGuard<'_> {
        self.make_write_guard(self.write_conn.lock())
    }

    /// Attempt to lock the write connection.
    ///
    /// Returns `MemoryError::ReadOnly` if the pool was opened read-only.
    ///
    /// The same in-memory reentrancy hazard as [`write`](Self::write) applies:
    /// never call [`read`](Self::read) on the same thread while holding the
    /// returned guard on an in-memory pool.
    pub fn try_write(&self) -> Result<WriteGuard<'_>> {
        if self.read_only {
            return Err(MemoryError::ReadOnly);
        }
        Ok(self.make_write_guard(self.write_conn.lock()))
    }

    /// Wrap a freshly-acquired write `MutexGuard` in a [`WriteGuard`], recording
    /// (in debug builds only) that this thread now holds this pool's write lock
    /// so [`read`](Self::read) can detect the in-memory reentrant deadlock.
    fn make_write_guard<'a>(&self, guard: MutexGuard<'a, Connection>) -> WriteGuard<'a> {
        #[cfg(debug_assertions)]
        {
            let pool_addr = std::ptr::from_ref::<Self>(self) as usize;
            WRITE_HELD.with(|held| {
                held.borrow_mut().insert(pool_addr);
            });
            WriteGuard { guard, pool_addr }
        }
        #[cfg(not(debug_assertions))]
        {
            WriteGuard { guard }
        }
    }

    /// Whether this pool was opened in read-only mode.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Embedding dimension configured for this pool.
    #[must_use]
    #[allow(dead_code)] // complete pool API — used after #108 engine split
    pub const fn embed_dim(&self) -> usize {
        self.embed_dim
    }

    /// Whether this pool is file-backed (vs in-memory).
    #[must_use]
    pub const fn is_file_backed(&self) -> bool {
        self.path.is_some()
    }

    /// Number of read connections in the pool. Used by the construction
    /// equivalence harness to prove the builder preserves the old default of 4.
    ///
    /// In-memory pools report `0` (they have no pooled read connections — reads
    /// serialize through the write connection), preserving the observable the
    /// equivalence snapshots pin.
    #[must_use]
    #[allow(dead_code)] // observed only by the equivalence test harness
    pub(crate) const fn read_pool_size(&self) -> usize {
        match self.mode {
            BackendMode::InMemory => 0,
            BackendMode::FileBacked { read_pool_size } => read_pool_size,
        }
    }

    /// Database file path, if file-backed. `None` for in-memory.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::get_config;

    #[test]
    fn pool_open_memory() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        let conn = pool.write();
        let v = get_config(&conn, "schema_version").unwrap();
        drop(conn);
        assert!(v.is_some());
    }

    #[test]
    fn pool_memory_read_uses_write_conn() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        let r = pool.read().unwrap();
        let v = get_config(&r, "schema_version").unwrap();
        drop(r);
        assert!(v.is_some());
    }

    #[test]
    fn pool_file_backed_concurrent_reads() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = ConnectionPool::open(&db_path, 4, 2, None).unwrap();

        // Write something
        {
            let w = pool.write();
            crate::store::schema::set_config(&w, "test_key", "test_value").unwrap();
        }

        // Two concurrent reads: both guards are acquired before either is
        // released, proving the pool grants 2 simultaneous read checkouts.
        let r1 = pool.read().unwrap();
        let r2 = pool.read().unwrap();
        let v1 = get_config(&r1, "test_key").unwrap();
        drop(r1);
        let v2 = get_config(&r2, "test_key").unwrap();
        drop(r2);
        assert_eq!(v1, Some("test_value".into()));
        assert_eq!(v2, Some("test_value".into()));
    }

    #[test]
    fn pool_read_guard_returns_connection() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = ConnectionPool::open(&db_path, 4, 1, None).unwrap();

        // Checkout and return
        {
            let _r = pool.read().unwrap();
        }
        // Re-checkout succeeds (connection was returned)
        {
            let _r = pool.read().unwrap();
        }
    }

    #[test]
    fn pool_embed_dim() {
        let pool = ConnectionPool::open_memory(768).unwrap();
        assert_eq!(pool.embed_dim(), 768);
    }

    #[test]
    fn pool_file_backed_is_file_backed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = ConnectionPool::open(&db_path, 4, 2, None).unwrap();
        assert!(pool.is_file_backed());

        let mem_pool = ConnectionPool::open_memory(4).unwrap();
        assert!(!mem_pool.is_file_backed());
    }

    #[test]
    fn pool_open_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        // First create a valid DB
        let pool = ConnectionPool::open(&db_path, 4, 2, None).unwrap();
        drop(pool);

        // Now open read-only
        let pool = ConnectionPool::open_read_only(&db_path, 4, 2).unwrap();
        assert!(pool.is_file_backed());
        assert!(pool.is_read_only());

        // Read should work
        let r = pool.read().unwrap();
        let v = get_config(&r, "schema_version").unwrap();
        drop(r);
        assert!(v.is_some());
    }

    #[test]
    fn pool_read_only_write_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = ConnectionPool::open(&db_path, 4, 2, None).unwrap();
        drop(pool);

        let pool = ConnectionPool::open_read_only(&db_path, 4, 2).unwrap();
        let err = pool.try_write().unwrap_err();
        assert!(matches!(err, MemoryError::ReadOnly));
    }

    #[test]
    fn pool_open_read_only_nonexistent_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        let result = ConnectionPool::open_read_only(&db_path, 4, 2);
        assert!(matches!(result, Err(MemoryError::Migration(_))));
    }

    #[test]
    fn pool_not_read_only_by_default() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        assert!(!pool.is_read_only());
    }

    /// A file-backed `open()` with `read_pool_size == 0` must be rejected, not
    /// silently produce a pool that serializes every read through the write
    /// connection (#340 conflation footgun, #356 sentinel removal). The
    /// in-memory discriminant is `BackendMode::InMemory`, never a zero read
    /// pool on a file-backed pool.
    #[test]
    fn pool_open_rejects_zero_read_pool_size() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        // `ConnectionPool` is not `Debug`, so match instead of `unwrap_err()`.
        assert!(matches!(
            ConnectionPool::open(&db_path, 4, 0, None),
            Err(MemoryError::Pool(_))
        ));
    }

    /// `open_read_only()` with `read_pool_size == 0` is the same footgun and
    /// must be rejected identically (#340/#356).
    #[test]
    fn pool_open_read_only_rejects_zero_read_pool_size() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        // Create a valid DB first so the read-only open reaches the size check.
        let rw = ConnectionPool::open(&db_path, 4, 2, None).unwrap();
        drop(rw);

        assert!(matches!(
            ConnectionPool::open_read_only(&db_path, 4, 0),
            Err(MemoryError::Pool(_))
        ));
    }

    /// In-memory pools report `read_pool_size() == 0` (the construction
    /// equivalence harness pins this); the `BackendMode` enum must preserve that
    /// observable.
    #[test]
    fn pool_in_memory_reports_zero_read_pool_size() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        assert_eq!(pool.read_pool_size(), 0);
        assert!(!pool.is_file_backed());
    }

    /// In in-memory mode, `read()` locks the (non-reentrant) write `Mutex`, so
    /// holding a `write()` guard and calling `read()` on the same thread would
    /// self-deadlock. A debug-only assertion turns that latent hang into an
    /// immediate, observable panic at dev time (#278). The guard is per-pool and
    /// per-thread, so legitimate cross-thread contention does not trip it.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "reentrant")]
    fn pool_in_memory_read_while_holding_write_panics_in_debug() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        let _w = pool.write();
        // Same-thread reentrant read: must trip the debug reentrancy guard
        // before it can block forever on the non-reentrant Mutex.
        let _r = pool.read();
    }

    /// A held write guard on one pool must NOT make a same-thread read on a
    /// *different* in-memory pool panic — the reentrancy detector is scoped per
    /// pool, not per thread (#278). Guards soundness against false positives.
    #[cfg(debug_assertions)]
    #[test]
    fn pool_in_memory_read_other_pool_while_holding_write_is_ok() {
        let pool_a = ConnectionPool::open_memory(4).unwrap();
        let pool_b = ConnectionPool::open_memory(4).unwrap();
        let _wa = pool_a.write();
        // Reading pool_b while holding pool_a's write guard is safe. Assert
        // inline so the significant-Drop `Ok` guard isn't bound to a local.
        assert!(pool_b.read().is_ok());
    }

    /// After the write guard is dropped, the same thread may read the in-memory
    /// pool again — the per-thread/per-pool marker is cleared on guard drop, so
    /// the detector does not leave a stale "held" state (#278).
    #[cfg(debug_assertions)]
    #[test]
    fn pool_in_memory_read_after_write_dropped_is_ok() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        {
            let _w = pool.write();
        }
        // Assert inline so the significant-Drop `Ok` guard isn't bound to a
        // local (matches the other read-acquire tests in this module).
        assert!(pool.read().is_ok());
    }

    #[test]
    fn pool_path_accessor() {
        // In-memory: path() returns None
        let mem_pool = ConnectionPool::open_memory(4).unwrap();
        assert!(mem_pool.path().is_none());

        // File-backed: path() returns the path used to open the pool
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = ConnectionPool::open(&db_path, 4, 1, None).unwrap();
        assert_eq!(pool.path(), Some(db_path.as_path()));
    }

    #[test]
    fn pool_open_with_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // Opening with a backup_dir must succeed and produce a usable pool
        let pool = ConnectionPool::open(&db_path, 4, 1, Some(&backup_dir)).unwrap();
        assert!(pool.is_file_backed());
        assert!(!pool.is_read_only());

        let r = pool.read().unwrap();
        let v = get_config(&r, "schema_version").unwrap();
        drop(r);
        assert!(v.is_some());
    }

    /// Exhausting the read pool must make `read()` fail within the configured
    /// timeout instead of blocking forever (#573 L16). Uses a 1-connection pool
    /// and a tiny timeout so the test stays fast.
    #[test]
    fn pool_read_acquire_times_out_when_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut pool = ConnectionPool::open(&db_path, 4, 1, None).unwrap();
        pool.read_acquire_timeout = Duration::from_millis(50);

        // Hold the only read connection so the pool is exhausted.
        let _held = pool.read().unwrap();

        let start = std::time::Instant::now();
        // `ReadConn` is not `Debug`, so match instead of `unwrap_err()`. Match
        // inline so the (significant-Drop) `Ok` guard isn't bound to a local.
        let timed_out = matches!(pool.read(), Err(MemoryError::Pool(_)));
        let elapsed = start.elapsed();

        assert!(
            timed_out,
            "expected Err(MemoryError::Pool), got Ok or other"
        );
        // It must return promptly (bounded), not hang. Generous upper bound to
        // tolerate scheduler jitter while still proving the wait is finite.
        assert!(
            elapsed < Duration::from_secs(5),
            "acquire did not return within bound: {elapsed:?}"
        );
    }

    /// The acquire wait must be bounded by an *absolute* deadline, not a
    /// per-wakeup timeout (#573 L16, Gemini review). A storm of spurious
    /// wakeups must not be able to extend the wait past `read_acquire_timeout`.
    ///
    /// Mutation guard: with the old `wait_for(timeout)` each wakeup reset the
    /// full budget, so under this wake-storm `read()` could not return until
    /// the storm stopped (~600ms) — failing the `< 400ms` assert. With
    /// `wait_until(deadline)` the deadline is immune to wakeups and it returns
    /// at ~100ms.
    #[test]
    fn pool_read_acquire_deadline_survives_spurious_wakeups() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut pool = ConnectionPool::open(&db_path, 4, 1, None).unwrap();
        pool.read_acquire_timeout = Duration::from_millis(100);

        // Hold the only read connection so the pool stays exhausted.
        let _held = pool.read().unwrap();

        std::thread::scope(|s| {
            // Spuriously wake the acquire-waiter far longer than the timeout.
            s.spawn(|| {
                let storm_start = Instant::now();
                while storm_start.elapsed() < Duration::from_millis(600) {
                    pool.read_available.notify_one();
                    std::thread::sleep(Duration::from_millis(2));
                }
            });

            let start = Instant::now();
            // `ReadConn` is not `Debug`; match inline so the significant-Drop
            // `Ok` guard isn't bound to a local.
            let timed_out = matches!(pool.read(), Err(MemoryError::Pool(_)));
            let elapsed = start.elapsed();

            assert!(timed_out, "expected Err(MemoryError::Pool), got Ok");
            assert!(
                elapsed < Duration::from_millis(400),
                "acquire ignored the absolute deadline under spurious wakeups: {elapsed:?}"
            );
        });
    }

    /// After a connection is checked out and returned, a subsequent acquire
    /// must succeed (the happy path still works with the bounded wait in place).
    #[test]
    fn pool_read_acquire_succeeds_when_connection_returned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut pool = ConnectionPool::open(&db_path, 4, 1, None).unwrap();
        pool.read_acquire_timeout = Duration::from_millis(50);

        // Take and immediately return the only connection.
        {
            let _r = pool.read().unwrap();
        }
        // Re-acquire succeeds because the connection was returned. Assert
        // inline so the significant-Drop guard isn't bound to a local.
        assert!(pool.read().is_ok());
    }
}
