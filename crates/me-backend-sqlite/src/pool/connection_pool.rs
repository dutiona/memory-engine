use me_types::error::StorageError;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, MutexGuard};
use rusqlite::Connection;

use crate::store::schema::{
    init_schema, migrate, open_connection, open_connection_read_only,
    open_memory as open_memory_conn,
};
use me_types::error::{MemoryError, MigrationError, Result};

/// Default bound on how long [`ConnectionPool::read`] waits for a read
/// connection to become available before failing with [`MemoryError::Pool`].
///
/// Without a bound the acquire path would block forever on the `Condvar` when
/// every read connection is checked out and never returned (e.g. a deadlocked
/// or leaked guard). A finite default turns that silent hang into a clear,
/// observable error. 30s is generous for healthy contention yet short enough
/// to surface a genuine leak.
const DEFAULT_READ_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on a file-backed pool's `read_pool_size`.
///
/// Each pooled read connection is a real OS file descriptor and an `SQLite`
/// connection object, so the size flows from caller-supplied config straight
/// into `Vec::with_capacity` + a connection-open loop. Without a ceiling an
/// excessive value (e.g. `usize::MAX`, or millions) over-allocates and exhausts
/// the process FD table before any useful work (#415, CWE-770/789). 256 is far
/// above any realistic read-concurrency need for an in-process embedded library
/// (default is 4) yet bounds the worst case. The value is consumer-controlled,
/// not network-attacker-controlled, which bounds the severity — but enforcing
/// the invariant at the pool boundary makes it hold regardless of caller.
const MAX_READ_POOL: usize = 256;

/// Validate a caller-supplied `read_pool_size` for a file-backed pool.
///
/// Enforces both bounds:
/// - lower (`>= 1`): a file-backed pool needs at least one read connection, and
///   `0` is *not* an implicit in-memory request — see [`BackendMode`]
///   (#340/#356);
/// - upper (`<= MAX_READ_POOL`): reject (rather than silently clamp) an
///   excessive value before it can exhaust file descriptors / over-allocate, so
///   the misconfiguration is surfaced not hidden (#415).
///
/// Returns the validated value so callers can use it directly.
///
/// # Errors
///
/// Returns [`MemoryError::Pool`] if `read_pool_size == 0` or
/// `read_pool_size > MAX_READ_POOL`.
fn validate_read_pool_size(read_pool_size: usize) -> Result<usize> {
    if read_pool_size == 0 {
        return Err(MemoryError::Pool(
            "read_pool_size must be >= 1 for a file-backed pool; use \
             ConnectionPool::open_memory for an in-memory database"
                .to_string(),
        ));
    }
    if read_pool_size > MAX_READ_POOL {
        return Err(MemoryError::Pool(format!(
            "read_pool_size {read_pool_size} exceeds the maximum of {MAX_READ_POOL}; \
             each read connection is a real file descriptor"
        )));
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
    /// Test-only deterministic synchronization hook for the block-then-wake
    /// path. Invoked inside [`Self::read`] while the `read_conns` `Mutex` is held
    /// and found empty, immediately *before* `wait_until` atomically releases the
    /// lock and parks. Because the lock is still held when this fires, any other
    /// thread that subsequently tries to lock `read_conns` (e.g. to return a
    /// connection) cannot proceed until this thread has parked — turning the
    /// "is the waiter parked yet?" race into a deterministic lock handoff. `None`
    /// (and zero-cost) outside tests.
    #[cfg(test)]
    on_acquire_park: Option<Box<dyn Fn() + Send + Sync>>,
}

// Debug-only, per-thread set of pool addresses on which *this* thread currently
// holds the in-memory write `Mutex` (via a `write()`/`try_write()` guard OR an
// in-memory `read()` guard — both lock the *same* non-reentrant `write_conn`).
// Used to detect the in-memory reentrant deadlock (#278) without false
// positives: the marker is keyed by pool identity AND is thread-local, so
// legitimate cross-thread write/read contention never trips it, and a guard on
// one pool does not implicate reads on another. Compiled out entirely in release
// builds.
//
// A `HashSet` cannot represent the same pool held *twice* by one thread, but
// that can never happen for in-memory pools: holding any guard that locks
// `write_conn` and acquiring a second guard on the same pool/thread would
// deadlock on the non-reentrant `Mutex` — exactly the violation the marker
// catches *before* the second lock. So at most one live marker per pool is
// possible, and a plain set is sufficient (insert is idempotent, drop removes).
#[cfg(debug_assertions)]
thread_local! {
    static WRITE_HELD: std::cell::RefCell<std::collections::HashSet<usize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Insert this pool's address into the per-thread `WRITE_HELD` marker (debug
/// only). Both [`WriteGuard`] and the in-memory [`SerializedReadGuard`] register
/// here because both hold the single non-reentrant `write_conn` `Mutex`.
#[cfg(debug_assertions)]
fn mark_write_held(pool_addr: usize) {
    WRITE_HELD.with(|held| {
        held.borrow_mut().insert(pool_addr);
    });
}

/// Remove this pool's address from the per-thread `WRITE_HELD` marker (debug
/// only), called from the `Drop` of whichever guard holds `write_conn`.
#[cfg(debug_assertions)]
fn unmark_write_held(pool_addr: usize) {
    WRITE_HELD.with(|held| {
        held.borrow_mut().remove(&pool_addr);
    });
}

/// Assert (debug only) that this thread does not already hold a guard locking
/// `write_conn` for the given in-memory pool, before a path that would lock it
/// again and self-deadlock on the non-reentrant `Mutex` (#278).
#[cfg(debug_assertions)]
fn assert_not_reentrant(pool_addr: usize) {
    let held = WRITE_HELD.with(|h| h.borrow().contains(&pool_addr));
    assert!(
        !held,
        "reentrant deadlock: a write-connection lock was requested on a thread that \
         already holds a guard locking this pool's write connection — i.e. read() (on \
         an in-memory pool), write(), or try_write() called while already holding a \
         write() guard or a prior in-memory read() guard. `parking_lot::Mutex` is \
         non-reentrant, so the second lock would hang forever (#278)"
    );
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
        unmark_write_held(self.pool_addr);
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
    /// Pool identity, used only by the debug reentrancy detector. An in-memory
    /// read holds the same `write_conn` `Mutex` as a write guard, so it registers
    /// in `WRITE_HELD` too — making the #278 assertion cover read-then-read, not
    /// just write-then-read.
    #[cfg(debug_assertions)]
    pool_addr: usize,
}

impl std::ops::Deref for SerializedReadGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.guard
    }
}

#[cfg(debug_assertions)]
impl Drop for SerializedReadGuard<'_> {
    fn drop(&mut self) {
        unmark_write_held(self.pool_addr);
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
    /// Returns `MemoryError::Pool` if `read_pool_size` exceeds `MAX_READ_POOL`
    /// (256): each read connection is a real OS file descriptor, so an excessive
    /// value is rejected (not clamped) before it can exhaust the FD table /
    /// over-allocate (#415, CWE-770/789).
    /// Returns `MemoryError::Storage` if any connection or schema setup fails.
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
                .map_err(StorageError::backend)?;
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
            #[cfg(test)]
            on_acquire_park: None,
        })
    }

    /// Open an in-memory pool (single connection for both reads and writes).
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::Storage` if connection or schema setup fails.
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
            #[cfg(test)]
            on_acquire_park: None,
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
    /// connection; #340/#356), or if it exceeds `MAX_READ_POOL` (256): each read
    /// connection is a real OS file descriptor, so an oversized value is rejected
    /// rather than allowed to exhaust the FD table (#415, CWE-770/789).
    /// Returns `MemoryError::Storage` if any connection or pragma setup fails.
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
            #[cfg(test)]
            on_acquire_park: None,
        })
    }

    /// Checkout a read connection.
    ///
    /// - File-backed: pops from the bounded pool. If exhausted, waits on a
    ///   `Condvar` for a connection to be returned, up to
    ///   `read_acquire_timeout` (default `DEFAULT_READ_ACQUIRE_TIMEOUT`).
    /// - In-memory: locks the write connection (serialized but correct).
    ///
    /// The happy path (a connection available immediately) returns without
    /// waiting and never errors.
    ///
    /// # In-memory reentrancy hazard (#278)
    ///
    /// In in-memory mode this locks the **same** `Mutex` as
    /// [`write`](Self::write)/[`try_write`](Self::try_write) — and as a *prior*
    /// in-memory `read()`, since an in-memory read guard
    /// (`SerializedReadGuard`) holds that very `write_conn` lock. The
    /// `parking_lot::Mutex` is non-reentrant, so calling `read()` on a thread
    /// that already holds **any** guard locking `write_conn` (a write guard OR an
    /// earlier in-memory read guard) self-deadlocks (a release-build hang; a
    /// debug-build reentrancy panic). Drop every such guard before reading again
    /// on an in-memory pool. File-backed pools are immune — reads use a separate
    /// connection pool, so neither read-then-read nor write-then-read contends on
    /// one lock.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if no read connection becomes available
    /// within `read_acquire_timeout` — turning a previously-unbounded hang
    /// (e.g. a leaked or deadlocked guard) into an observable failure.
    ///
    /// # Panics
    ///
    /// In a debug build, panics on the in-memory reentrancy violation described
    /// above (`assert_not_reentrant`); compiled out in release builds, where the
    /// same violation instead hangs (see above).
    pub fn read(&self) -> Result<ReadConn<'_>> {
        // Dispatch on the canonical backend discriminant, never on a derived
        // count (#340/#356). In-memory mode has no pooled read connections, so
        // a read serializes through the single shared write connection.
        let read_pool_size = match self.mode {
            BackendMode::InMemory => {
                // In-memory mode locks the write connection here. The
                // `parking_lot::Mutex` is non-reentrant, so a thread that holds
                // ANY guard locking `write_conn` — a `write()`/`try_write()`
                // guard OR a prior in-memory `read()` guard — MUST NOT call
                // `read()` again on the same thread: doing so re-locks the same
                // Mutex and self-deadlocks (#278). Catch the violation at dev
                // time with a per-pool, per-thread marker before the blocking
                // `lock()` would hang forever; compiled out in release.
                //
                // The in-memory read guard registers in the SAME marker as the
                // write guard (insert below; removed in its `Drop`), so the
                // assertion covers read-then-read as well as write-then-read.
                #[cfg(debug_assertions)]
                let pool_addr = std::ptr::from_ref::<Self>(self) as usize;
                #[cfg(debug_assertions)]
                assert_not_reentrant(pool_addr);
                let guard = self.write_conn.lock();
                #[cfg(debug_assertions)]
                mark_write_held(pool_addr);
                return Ok(ReadConn::InMemory(SerializedReadGuard {
                    guard,
                    #[cfg(debug_assertions)]
                    pool_addr,
                }));
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
            // Test-only: signal "about to park" while still holding `conns`, so a
            // returning thread's `read_conns.lock()` blocks until `wait_until`
            // (below) releases the lock — a deterministic park handoff, no sleep.
            #[cfg(test)]
            if let Some(hook) = self.on_acquire_park.as_ref() {
                hook();
            }
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

    /// Lock the write connection **unconditionally**, bypassing the
    /// `read_only` guard.
    ///
    /// `pub(crate)` by design (#416): on a read-only pool this hands out the
    /// `write_conn` with no `read_only` check, so a write attempt would surface
    /// a raw `SQLite` error instead of the typed [`MemoryError::ReadOnly`].
    /// Restricting visibility keeps that footgun off the public API — external
    /// callers must go through [`try_write`](Self::try_write) (or the engine's
    /// `write_conn` helper), which enforces the guard. Internal callers use this
    /// only on pools known to be writable (restore/init paths). A read-only pool
    /// additionally sets `query_only`/`SQLITE_OPEN_READ_ONLY`, so this is
    /// defense-in-depth, not a live vulnerability.
    ///
    /// # In-memory reentrancy hazard (#278)
    ///
    /// In in-memory mode [`read`](Self::read) locks this **same** connection —
    /// the `SerializedReadGuard` it returns holds `write_conn`'s lock to
    /// serialize reads. The `parking_lot::Mutex` is **non-reentrant**, so the
    /// hazard runs in *both* directions on one thread, and both self-deadlock
    /// (release: hangs forever; debug: the shared reentrancy marker fires):
    /// - holding the guard returned here, then calling `read()`; and
    /// - holding an in-memory `read()` guard, then calling `write()` or
    ///   [`try_write`](Self::try_write) — the re-lock blocks on `write_conn`
    ///   (the `try` in `try_write` is the `read_only` check, *not* a `try_lock`,
    ///   so it blocks rather than failing fast).
    ///
    /// Drop every guard locking `write_conn` before re-entering on either side.
    /// File-backed pools are unaffected (reads come from a separate pool).
    // Widened pub(crate) -> pub (Wave 2 #816, me-backend-sqlite carve, sub-PR 2a).
    // `storage/sqlite/` joined this crate in sub-PR 2b, but this stays `pub`: the
    // facade's own engine/mod.rs (open-path init), engine/restore.rs (restore-into
    // paths), and search/hybrid.rs (a test helper) call it directly across the crate
    // boundary too — NOT just the storage/sqlite test modules the sub-PR 2a comment
    // anticipated. `me-backend-sqlite` is `publish = false` (workspace-internal only,
    // never a public dependency), so this does not expose the #416 footgun outside
    // the workspace.
    pub fn write(&self) -> WriteGuard<'_> {
        #[cfg(debug_assertions)]
        assert_not_reentrant(std::ptr::from_ref::<Self>(self) as usize);
        self.make_write_guard(self.write_conn.lock())
    }

    /// Attempt to lock the write connection.
    ///
    /// # Errors
    ///
    /// Returns `MemoryError::ReadOnly` if the pool was opened read-only.
    ///
    /// The same in-memory reentrancy hazard as [`write`](Self::write) applies in
    /// both directions: never call [`read`](Self::read) while holding the guard
    /// returned here, and never call `try_write()` while already holding an
    /// in-memory [`read`](Self::read) guard. Either re-locks the non-reentrant
    /// `write_conn` `Mutex` on the same thread and self-deadlocks — `try_write`
    /// blocks on `write_conn.lock()` (the `try` only gates `read_only`), it does
    /// not fail fast. File-backed pools are immune.
    pub fn try_write(&self) -> Result<WriteGuard<'_>> {
        if self.read_only {
            return Err(MemoryError::ReadOnly);
        }
        #[cfg(debug_assertions)]
        assert_not_reentrant(std::ptr::from_ref::<Self>(self) as usize);
        Ok(self.make_write_guard(self.write_conn.lock()))
    }

    /// Wrap a freshly-acquired write `MutexGuard` in a [`WriteGuard`], recording
    /// (in debug builds only) that this thread now holds this pool's write lock
    /// so [`read`](Self::read) can detect the in-memory reentrant deadlock.
    // In release builds the debug-only marking is compiled out, leaving `&self`
    // unused and the fn const-eligible; the documented (debug-profile) clippy
    // gate is clean, so allow these only for the release profile.
    #[cfg_attr(
        not(debug_assertions),
        allow(clippy::unused_self, clippy::missing_const_for_fn)
    )]
    fn make_write_guard<'a>(&self, guard: MutexGuard<'a, Connection>) -> WriteGuard<'a> {
        #[cfg(debug_assertions)]
        {
            let pool_addr = std::ptr::from_ref::<Self>(self) as usize;
            mark_write_held(pool_addr);
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

    /// Companion to [`pool_read_only_write_returns_error`]: `write()`
    /// **intentionally** bypasses the `read_only` guard that `try_write()`
    /// enforces. It is infallible — it locks the underlying connection without
    /// erroring or panicking even on a read-only pool — and is `pub(crate)` so
    /// only internal code (operating on pools known to be writable) can reach
    /// it (#416/#472). User-facing writes must go through `try_write()`. This
    /// test pins that contract so a future change to `write()` (e.g. adding a
    /// guard, or repurposing it) is a deliberate, reviewed decision rather than
    /// a silent behavior shift. The underlying `SQLite` connection still refuses
    /// actual mutations (`SQLITE_OPEN_READ_ONLY`), so the bypass cannot corrupt
    /// a read-only database — it only changes *which* layer reports the refusal.
    #[test]
    fn pool_read_only_write_bypasses_guard_infallibly() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let rw = ConnectionPool::open(&db_path, 4, 2, None).unwrap();
        drop(rw);

        let pool = ConnectionPool::open_read_only(&db_path, 4, 2).unwrap();
        assert!(pool.is_read_only());

        // `write()` does NOT check `read_only`: it locks and returns a guard
        // without error or panic. A read query through it still works (the
        // connection is open); only a *write* statement would be refused by
        // SQLite at execution time, which is the point of using `try_write()`
        // for user-facing operations.
        let w = pool.write();
        let v = get_config(&w, "schema_version").unwrap();
        assert!(v.is_some());
        drop(w);
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

    /// A `read_pool_size` above [`MAX_READ_POOL`] must be rejected, not
    /// attempted: each connection is a real OS file descriptor, so an
    /// excessive value would exhaust the FD table / over-allocate before any
    /// useful work (#415, CWE-770/789). Rejecting (vs silently clamping)
    /// surfaces the misconfiguration instead of hiding it.
    #[test]
    fn pool_open_rejects_oversized_read_pool_size() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        assert!(matches!(
            ConnectionPool::open(&db_path, 4, MAX_READ_POOL + 1, None),
            Err(MemoryError::Pool(_))
        ));
    }

    /// `read_pool_size == MAX_READ_POOL` (the boundary) is accepted; only
    /// strictly-greater values are rejected (#415).
    #[test]
    fn pool_open_accepts_read_pool_size_at_cap() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = ConnectionPool::open(&db_path, 4, MAX_READ_POOL, None).unwrap();
        assert_eq!(pool.read_pool_size(), MAX_READ_POOL);
    }

    /// `open_read_only()` enforces the same upper bound as `open()` (#415).
    #[test]
    fn pool_open_read_only_rejects_oversized_read_pool_size() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let rw = ConnectionPool::open(&db_path, 4, 2, None).unwrap();
        drop(rw);
        assert!(matches!(
            ConnectionPool::open_read_only(&db_path, 4, MAX_READ_POOL + 1),
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

    /// The reentrancy detector must also catch the **read-then-read** arm: an
    /// in-memory `read()` guard holds the same non-reentrant `write_conn` lock as
    /// a write guard, so a second same-thread `read()` self-deadlocks identically
    /// (#278). Holding a [`SerializedReadGuard`] and calling `read()` again must
    /// trip the debug assertion before it can hang.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "reentrant")]
    fn pool_in_memory_read_while_holding_read_panics_in_debug() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        let _r1 = pool.read().unwrap();
        // Same-thread reentrant read while already holding an in-memory read
        // guard: must trip the debug reentrancy guard before blocking forever.
        let _r2 = pool.read();
    }

    /// `write()` re-locks the same non-reentrant `write_conn`, so a second
    /// same-thread `write()` while already holding a write guard self-deadlocks
    /// identically to the read arms. The detector must trip here too — the panic
    /// guard is at the `write()` entry point, not only `read()` (#278).
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "reentrant")]
    fn pool_write_while_holding_write_panics_in_debug() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        let _w1 = pool.write();
        // Same-thread reentrant write: must trip the debug reentrancy guard
        // before blocking forever on the non-reentrant Mutex.
        let _w2 = pool.write();
    }

    /// `try_write()` shares `write()`'s reentrancy hazard: requesting it while
    /// already holding a write guard would deadlock on the non-reentrant
    /// `write_conn`. The debug assertion must fire before the lock attempt (#278).
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "reentrant")]
    fn pool_try_write_while_holding_write_panics_in_debug() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        let _w = pool.write();
        // Same-thread reentrant try_write: tripped before it can hang.
        let _t = pool.try_write();
    }

    /// After an in-memory read guard is dropped, the same thread may read again —
    /// the `SerializedReadGuard` `Drop` clears the per-thread/per-pool marker, so
    /// the detector leaves no stale "held" state for the read path either (#278).
    #[cfg(debug_assertions)]
    #[test]
    fn pool_in_memory_read_after_read_dropped_is_ok() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        {
            let _r = pool.read().unwrap();
        }
        // Assert inline so the significant-Drop `Ok` guard isn't bound to a
        // local (matches the other read-acquire tests in this module).
        assert!(pool.read().is_ok());
    }

    /// A held in-memory read guard on one pool must NOT make a same-thread read
    /// on a *different* in-memory pool panic — the read-path marker is scoped per
    /// pool, not per thread, exactly like the write-path one (#278).
    #[cfg(debug_assertions)]
    #[test]
    fn pool_in_memory_read_other_pool_while_holding_read_is_ok() {
        let pool_a = ConnectionPool::open_memory(4).unwrap();
        let pool_b = ConnectionPool::open_memory(4).unwrap();
        let _ra = pool_a.read().unwrap();
        assert!(pool_b.read().is_ok());
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

    /// Cross-thread block-then-wake: thread B blocks on an exhausted 1-conn
    /// pool, the main thread drops the only checked-out connection (firing
    /// `notify_one`), and B must then **acquire and use** that connection —
    /// proving the `Condvar` wakeup wakes a genuinely-blocked waiter that then
    /// succeeds, not merely that the timeout path returns (#473).
    ///
    /// The single-threaded `pool_read_acquire_succeeds_when_connection_returned`
    /// never blocks (take/drop/reacquire on one thread), and the existing
    /// multithreaded tests prove only the *timeout* path. This is the missing
    /// case: a real blocked thread woken by a real return.
    ///
    /// **Determinism** (no sleep/timing heuristic): the park is synchronized via
    /// the `on_acquire_park` hook, which fires inside `read()` *while the
    /// `read_conns` `Mutex` is still held*, immediately before `wait_until`
    /// atomically releases it and parks. The main thread waits for that hook to
    /// run, then calls `drop(held)` — whose `read_conns.lock()` **cannot** acquire
    /// the lock until B has parked and released it. So the connection is returned
    /// (and `notify_one` fired) only after B is genuinely blocked on the
    /// `Condvar`. The old version relied on `sleep(50ms)` to *guess* B had
    /// parked; if the scheduler delayed B past the window, `drop(held)` returned
    /// the connection before B's first `is_empty()` check and B never blocked —
    /// the notify-wake path went unexercised yet the test still passed. This
    /// version proves the wake path or hangs (timeout-bounded), never green-
    /// without-proof.
    #[test]
    fn pool_read_blocks_then_wakes_and_succeeds_across_threads() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut pool = ConnectionPool::open(&db_path, 4, 1, None).unwrap();
        // Generous timeout: this test proves the *success-after-wake* path, so
        // it must never reach the timeout. Default (30s) is ample; keep it.

        // Deterministic park signal: flipped by the acquire-park hook while B
        // still holds `read_conns`, just before `wait_until` releases it.
        let b_parking = Arc::new(AtomicBool::new(false));
        {
            let flag = Arc::clone(&b_parking);
            pool.on_acquire_park = Some(Box::new(move || {
                flag.store(true, Ordering::SeqCst);
            }));
        }

        // Seed a row so the woken reader can prove its connection is live.
        {
            let w = pool.write();
            crate::store::schema::set_config(&w, "woke", "yes").unwrap();
        }

        // Hold the only read connection on the main thread: the pool is now
        // exhausted, so any other reader must block.
        let held = pool.read().unwrap();

        std::thread::scope(|s| {
            let reader = s.spawn(|| {
                // Blocks until `held` is dropped on the main thread; on wake it
                // must acquire the returned connection and read the seeded row.
                let r = pool
                    .read()
                    .expect("woken reader must acquire, not time out");
                get_config(&r, "woke").unwrap()
            });

            // Spin until B has locked `read_conns`, found it empty, and run the
            // park hook (still holding the lock). After this, B is committed to
            // `wait_until`; the lock handoff below makes the park-before-notify
            // ordering deterministic — no sleep needed.
            while !b_parking.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }

            // `drop(held)` returns the connection: `read_conns.lock()` inside the
            // drop blocks until B's `wait_until` releases the lock by parking, so
            // the push + `notify_one` land only once B is genuinely parked on the
            // `Condvar`. This wakes a real blocked waiter (not the timeout path).
            drop(held);

            let value = reader.join().expect("reader thread panicked");
            assert_eq!(
                value,
                Some("yes".to_string()),
                "woken reader read the wrong value through its acquired connection"
            );
        });
    }
}
