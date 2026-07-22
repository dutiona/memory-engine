//! The Postgres connection pool (#633) — `deadpool-postgres` over `tokio-postgres`.
//!
//! The PG analogue of `me-backend-sqlite`'s `ConnectionPool` (not a dependency of this
//! crate), and its structural opposite: Postgres is MVCC, so there is **no**
//! read-pool/write-mutex split and **no** write serialization — every checked-out
//! client is a full read+write session, and `tokio-postgres` is natively async (no
//! `spawn_blocking`).
//!
//! ## Read-only (two layers, defense-in-depth)
//!
//! 1. **Primary (typed):** the [`PgBackend`](super::PgBackend) write methods check a
//!    Rust-level `read_only` flag and return [`MemoryError::ReadOnly`] *before* issuing
//!    any statement — the analogue of `SQLite`'s `try_write` guard, and the source of the
//!    typed variant the conformance contract asserts.
//! 2. **Backstop (DB-level):** a read-only pool sets `default_transaction_read_only =
//!    on` via the libpq `options` connection parameter, so even a missed guard cannot
//!    mutate. This is connection-level (set once at config time, not per checkout).

use std::str::FromStr as _;

use deadpool_postgres::{Manager, Object, Pool};
use tokio_postgres::{Config as PgConfig, NoTls};

use me_types::error::{MemoryError, Result};

/// A `deadpool-postgres` pool plus the frozen `embed_dim` (the `vector(N)` column
/// width, like `SQLite`'s pool-derived dimension) and a `read_only` flag.
pub struct PgPool {
    pool: Pool,
    embed_dim: usize,
    read_only: bool,
}

impl PgPool {
    /// Build a writable pool from a connection string (a `postgres://…` URL or a libpq
    /// `key=value` DSN). The pool is **lazy** — no connection is opened until [`get`].
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if the connection string is invalid or the pool
    /// cannot be built.
    ///
    /// [`get`]: PgPool::get
    pub fn connect(conn: &str, embed_dim: usize) -> Result<Self> {
        Self::build(conn, embed_dim, false)
    }

    /// Build a read-only pool: every session is opened with
    /// `default_transaction_read_only = on` (the DB-level backstop). The Rust-level
    /// guard in [`PgBackend`](super::PgBackend) is what produces the typed
    /// [`MemoryError::ReadOnly`]; this is defense in depth.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if the connection string is invalid or the pool
    /// cannot be built.
    pub fn connect_read_only(conn: &str, embed_dim: usize) -> Result<Self> {
        Self::build(conn, embed_dim, true)
    }

    fn build(conn: &str, embed_dim: usize, read_only: bool) -> Result<Self> {
        let mut pg_config = PgConfig::from_str(conn)
            .map_err(|e| MemoryError::Pool(format!("invalid postgres connection string: {e}")))?;
        if read_only {
            // libpq `options` GUC — applied to every session this pool hands out.
            pg_config.options("-c default_transaction_read_only=on");
        }
        let manager = Manager::new(pg_config, NoTls);
        let pool = Pool::builder(manager)
            .build()
            .map_err(|e| MemoryError::Pool(format!("failed to build postgres pool: {e}")))?;
        Ok(Self {
            pool,
            embed_dim,
            read_only,
        })
    }

    /// Acquire a pooled client (connecting on first use).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if a connection cannot be acquired (the database
    /// is unreachable, the pool is exhausted, or the connect handshake fails).
    pub async fn get(&self) -> Result<Object> {
        self.pool
            .get()
            .await
            .map_err(|e| MemoryError::Pool(format!("postgres pool get: {e}")))
    }

    /// The embedding dimension this pool's backend was opened at (the `vector(N)`
    /// column width). Frozen at construction.
    #[must_use]
    pub const fn embed_dim(&self) -> usize {
        self.embed_dim
    }

    /// Whether this pool was opened read-only.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}
