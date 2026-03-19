use std::path::{Path, PathBuf};

use parking_lot::{Condvar, Mutex, MutexGuard};
use rusqlite::Connection;

use crate::error::{MemoryError, Result};
use crate::store::schema::{
    init_schema, migrate, open_connection, open_memory as open_memory_conn,
};

/// A connection pool with N read connections and 1 write connection.
///
/// SQLite WAL supports concurrent readers with a single writer.
/// The pool is bounded — `read()` blocks if all connections are checked out.
///
/// In-memory databases use the write connection for reads (serialized).
pub struct ConnectionPool {
    write_conn: Mutex<Connection>,
    read_conns: Mutex<Vec<Connection>>,
    read_available: Condvar,
    path: Option<PathBuf>,
    embed_dim: usize,
    read_pool_size: usize,
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
    pub fn open(
        path: &Path,
        embed_dim: usize,
        read_pool_size: usize,
        backup_dir: Option<&Path>,
    ) -> Result<Self> {
        let write_conn = open_connection(&path.to_string_lossy())?;
        init_schema(&write_conn)?;
        migrate(&write_conn, backup_dir)?;

        let mut read_conns = Vec::with_capacity(read_pool_size);
        for _ in 0..read_pool_size {
            let conn = open_connection(&path.to_string_lossy())?;
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
        })
    }

    /// Checkout a read connection.
    ///
    /// - File-backed: pops from bounded pool. Blocks via `Condvar` if exhausted.
    /// - In-memory: locks the write connection (serialized but correct).
    pub fn read(&self) -> ReadConn<'_> {
        if self.read_pool_size == 0 {
            // In-memory mode: use write connection for reads (serialized)
            let guard = self.write_conn.lock();
            return ReadConn::InMemory(WriteAsReadGuard { guard });
        }

        let mut conns = self.read_conns.lock();
        while conns.is_empty() {
            self.read_available.wait(&mut conns);
        }
        let conn = conns.pop().expect("condvar woke but pool empty");
        ReadConn::Pooled(ReadGuard {
            conn: Some(conn),
            pool: self,
        })
    }

    /// Lock the write connection.
    pub fn write(&self) -> MutexGuard<'_, Connection> {
        self.write_conn.lock()
    }

    /// Embedding dimension configured for this pool.
    #[must_use]
    pub const fn embed_dim(&self) -> usize {
        self.embed_dim
    }

    /// Whether this pool is file-backed (vs in-memory).
    #[must_use]
    pub fn is_file_backed(&self) -> bool {
        self.path.is_some()
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
        assert!(v.is_some());
    }

    #[test]
    fn pool_memory_read_uses_write_conn() {
        let pool = ConnectionPool::open_memory(4).unwrap();
        let r = pool.read();
        let v = get_config(&r, "schema_version").unwrap();
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

        // Two concurrent reads
        let r1 = pool.read();
        let r2 = pool.read();
        let v1 = get_config(&r1, "test_key").unwrap();
        let v2 = get_config(&r2, "test_key").unwrap();
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
            let _r = pool.read();
        }
        // Re-checkout succeeds (connection was returned)
        {
            let _r = pool.read();
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
}
