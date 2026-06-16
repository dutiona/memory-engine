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
    read_pool_size: usize,
    read_only: bool,
    /// Bound on the wait for a read connection in [`Self::read`]. Defaults to
    /// [`DEFAULT_READ_ACQUIRE_TIMEOUT`]; exposed as a field so a future
    /// `EngineConfig::read_acquire_timeout` can be wired in without touching
    /// the acquire path.
    read_acquire_timeout: Duration,
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

/// RAII guard for in-memory mode: wraps write connection `MutexGuard`.
pub struct WriteAsReadGuard<'a> {
    guard: MutexGuard<'a, Connection>,
}

impl std::ops::Deref for WriteAsReadGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.guard
    }
}

/// Enum over read guard types — file-backed (pooled) vs in-memory (write conn).
pub enum ReadConn<'a> {
    Pooled(ReadGuard<'a>),
    InMemory(WriteAsReadGuard<'a>),
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
            read_pool_size,
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
            read_pool_size: 0,
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
    /// Returns `MemoryError::Database` if any connection or pragma setup fails.
    /// Returns `MemoryError::Migration` if the file does not exist, the schema
    /// is uninitialized, or the schema needs migration.
    /// Returns `MemoryError::UnsupportedEpoch` if the DB epoch is from the future.
    pub fn open_read_only(path: &Path, embed_dim: usize, read_pool_size: usize) -> Result<Self> {
        use crate::store::schema::validate_schema_version;

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
            read_pool_size,
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
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if no read connection becomes available
    /// within `read_acquire_timeout` — turning a previously-unbounded hang
    /// (e.g. a leaked or deadlocked guard) into an observable failure.
    pub fn read(&self) -> Result<ReadConn<'_>> {
        if self.read_pool_size == 0 {
            // In-memory mode: use write connection for reads (serialized)
            let guard = self.write_conn.lock();
            return Ok(ReadConn::InMemory(WriteAsReadGuard { guard }));
        }

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
                    "read pool acquire timed out after {:?} (all {} connections checked out)",
                    self.read_acquire_timeout, self.read_pool_size
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
    pub fn write(&self) -> MutexGuard<'_, Connection> {
        self.write_conn.lock()
    }

    /// Attempt to lock the write connection.
    ///
    /// Returns `MemoryError::ReadOnly` if the pool was opened read-only.
    pub fn try_write(&self) -> Result<MutexGuard<'_, Connection>> {
        if self.read_only {
            return Err(MemoryError::ReadOnly);
        }
        Ok(self.write_conn.lock())
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
    #[must_use]
    #[allow(dead_code)] // observed only by the equivalence test harness
    pub(crate) const fn read_pool_size(&self) -> usize {
        self.read_pool_size
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
