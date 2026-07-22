//! # me-backend-postgres — sub-PR 3 (Wave 2 #816 / S2)
//!
//! The `PostgreSQL` implementation of the storage port (#633, epic #628 stage B1),
//! carved out of the `memory-engine` facade.
//!
//! `PgBackend` is the structural twin of `SqliteBackend` (in the sibling
//! `me-backend-sqlite` crate — not a dependency of this one), and its async-native
//! opposite. `SQLite` delegates every method through `block_read`/`block_write`
//! (`spawn_blocking` + a `!Send` `rusqlite` guard + a read-pool/write-mutex pool);
//! Postgres is MVCC and `tokio-postgres` is natively async, so the seam here is a
//! single `PgBackend::with_client` — *acquire a pooled client, run an async closure,
//! map driver errors at the boundary* — with **no** `spawn_blocking` and **no** write
//! serialization.
//!
//! ## Scope of #633 (honest boundary)
//!
//! This is the **skeleton**: the pool, a fresh migration chain producing the live v14
//! logical schema (see the `migrations` module), and the
//! [`SchemaManager`](me_storage::SchemaManager) lifecycle/identity/config surface. It
//! is **NOT** a full [`StorageBackend`](me_storage::StorageBackend): the umbrella's
//! blanket impl requires all six bounded traits, and `PgBackend` implements only
//! `SchemaManager` here. Data CRUD (`FactGraph`/`EventLog`/`ConsolidationStore`/
//! `SessionStore`) is #634; lexical+vector search (`SearchIndex`, the HNSW vector
//! index) and the conformance-arm flip are #635. So `PgBackend` cannot back a
//! `MemoryEngine` yet, and the facade's `PgFactory::make()` stays `todo!()` until
//! #635.
//!
//! ## Seam invariants (mirroring `SQLite`'s, minus the blocking concerns)
//!
//! - **No driver type crosses the seam:** a `tokio_postgres::Error` is mapped to
//!   [`MemoryError::Storage`] wrapping [`StorageError::Backend`] by `pg_err` at the
//!   boundary; semantic variants (`Migration`, `EmbeddingDimension`, `ReadOnly`,
//!   `Internal`, …) have a precise home and are constructed directly.
//! - **Read-only:** write methods check `read_only` first and return
//!   [`MemoryError::ReadOnly`] (the typed, primary guard); the pool's
//!   `default_transaction_read_only` is the DB-level backstop (see the `pool` module).
//! - **Transactions:** the multi-statement migration chain runs in one transaction —
//!   Postgres DDL is transactional (a genuine win over `SQLite`'s per-statement DDL).

// Panic-safety gate (#725, workspace lints). This crate's own `#[cfg(test)]` unit
// tests are exempt — a panic there is the intended failure signal.
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::future::Future;

use me_types::error::{MemoryError, Result, StorageError};

mod migrations;
mod pool;
mod schema;

// Gated on `test-util` (not bare `test`): the live-PG suite drives the
// `SchemaManager::raw_exec` failure-injection seam, which since #816 A1 lives behind the
// `test-util` feature (a cross-crate trait method can't ride `cfg(test)`). Every test here
// is `#[ignore]` (needs a Docker Postgres), so run it with
// `--features backend-postgres,test-util -- --ignored`.
#[cfg(all(test, feature = "test-util"))]
mod tests;

pub use pool::PgPool;

/// The `PostgreSQL` backend (#633): a `deadpool-postgres` pool + a fresh v14 migration
/// chain + the [`SchemaManager`](me_storage::SchemaManager) lifecycle surface.
///
/// `embed_dim` and `read_only` are mirrored from the pool at construction so a
/// backend's dimension / read-only-ness cannot diverge from its pool's. There is **no**
/// HNSW field (PG uses server-side `pgvector`, not the in-memory `hnsw` crate), so the
/// `ann`-feature machinery that the `SQLite` struct carries is simply absent here.
pub struct PgBackend {
    pool: PgPool,
    embed_dim: usize,
    read_only: bool,
}

// Build-time witness (not test-gated): `PgBackend` must be `Send + Sync` for the
// `#[async_trait]` `Send` futures and the eventual `Arc<dyn StorageBackend>` (#635).
// `deadpool_postgres::Pool` is `Send + Sync` (Arc-based) and the other fields are
// `Copy`, so a field that breaks this fails `cargo build`, not merely `cargo test`.
const _: fn() = || {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PgBackend>();
};

impl PgBackend {
    /// Wrap an already-built [`PgPool`]. `embed_dim` / `read_only` are read from the
    /// pool. Does **not** migrate — use [`connect`](Self::connect) for the
    /// connect-then-migrate convenience.
    #[must_use]
    pub const fn from_pool(pool: PgPool) -> Self {
        let embed_dim = pool.embed_dim();
        let read_only = pool.is_read_only();
        Self {
            pool,
            embed_dim,
            read_only,
        }
    }

    /// Connect to Postgres and run the fresh v14 migration chain to HEAD (idempotent —
    /// a re-connect against an at-HEAD database is a no-op), returning a ready backend.
    ///
    /// `conn` is a `postgres://…` URL or a libpq `key=value` DSN. `embed_dim` fixes the
    /// `vector(N)` column width (pgvector needs a concrete dimension at DDL time).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if the connection cannot be established,
    /// [`MemoryError::Migration`] if the schema is from a newer/incompatible version,
    /// or [`MemoryError::Storage`] on a backend failure during migration.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo() -> me_types::error::Result<()> {
    /// use me_backend_postgres::PgBackend;
    ///
    /// // Connects, then runs the fresh v14 migration chain to HEAD.
    /// let backend = PgBackend::connect(
    ///     "postgres://user:pw@localhost:5432/memory",
    ///     384, // embed_dim — fixes the pgvector vector(N) column width
    /// )
    /// .await?;
    /// # let _ = backend;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(conn: &str, embed_dim: usize) -> Result<Self> {
        let backend = Self::from_pool(PgPool::connect(conn, embed_dim)?);
        backend.run_migrations().await?;
        Ok(backend)
    }

    /// Connect read-only (sessions set `default_transaction_read_only = on`). Does
    /// **not** migrate (a read-only handle cannot write) — instead it *validates* the
    /// schema is at HEAD and compatible, mirroring `SQLite`'s read-only open path. So a
    /// read-only handle must be opened against an already-migrated database.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Pool`] if the connection cannot be established, or
    /// [`MemoryError::Migration`] if the database is uninitialized, needs migration, or
    /// is from a newer/incompatible version.
    pub async fn connect_read_only(conn: &str, embed_dim: usize) -> Result<Self> {
        let backend = Self::from_pool(PgPool::connect_read_only(conn, embed_dim)?);
        // Read-only open path: validate compatibility (epoch + version + the vector(N)
        // dimension) but never migrate — a read-only handle cannot write. Mirrors
        // SQLite's read-only open, which runs `validate_schema_version` rather than
        // `migrate`.
        backend
            .with_client(move |client| async move {
                migrations::validate_schema_version(&client, embed_dim).await
            })
            .await?;
        Ok(backend)
    }

    /// The native-async seam: acquire a pooled client and run `f` against it. The
    /// client is **moved** into the closure (owned, so `f` may open a transaction via
    /// `&mut`), and the future it returns is awaited on the executor — no
    /// `spawn_blocking`. This is the PG analogue of `SqliteBackend::block_read` /
    /// `block_write`; #634/#635 program every CRUD/search method against it.
    ///
    /// (A generic transaction wrapper `with_client_tx` is intentionally deferred to
    /// #634, where its `Transaction<'_>`-borrows-the-client lifetime shape can be
    /// settled against many transactional call sites rather than `migrate` alone.)
    async fn with_client<T, F, Fut>(&self, f: F) -> Result<T>
    where
        T: Send,
        F: FnOnce(deadpool_postgres::Object) -> Fut + Send,
        Fut: Future<Output = Result<T>> + Send,
    {
        let client = self.pool.get().await?;
        f(client).await
    }

    /// Run the fresh v14 migration chain to HEAD (delegated to [`migrations::migrate`]).
    /// Read-only-guarded: a read-only backend cannot migrate.
    async fn run_migrations(&self) -> Result<()> {
        self.read_only_guard()?;
        let embed_dim = self.embed_dim;
        self.with_client(
            |mut client| async move { migrations::migrate(&mut client, embed_dim).await },
        )
        .await
    }

    /// The primary, typed read-only guard: returns [`MemoryError::ReadOnly`] when this
    /// backend was opened read-only, mirroring `ConnectionPool::try_write`. Called at
    /// the top of every write method before any statement is issued.
    const fn read_only_guard(&self) -> Result<()> {
        if self.read_only {
            Err(MemoryError::ReadOnly)
        } else {
            Ok(())
        }
    }
}

/// Confine `tokio-postgres` below the seam (mirrors `sqlite::map_seam_err`): a raw
/// driver failure becomes the opaque [`StorageError::Backend`]. Semantic variants are
/// constructed directly by the methods, never routed through here.
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as a `.map_err(pg_err)` fn pointer, which passes the error by value"
)]
pub(crate) fn pg_err(e: tokio_postgres::Error) -> MemoryError {
    MemoryError::Storage(StorageError::Backend(e.to_string()))
}
