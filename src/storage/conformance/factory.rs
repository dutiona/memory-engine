//! The `ConformanceBackend` factory trait — the seam a backend implements to gain
//! the entire cross-backend contract battery (#632).
//!
//! `make*` are `async` even though `SqliteBackend` builds synchronously: #633's
//! `PgBackend` factory spins up a testcontainer (`async`), and the emission macro
//! `.await`s every `make*`, so the trait's async-ness is what makes #633 a drop-in.
//!
//! This is the **only** module in `storage::conformance` permitted to name a
//! concrete backend type (`SqliteBackend`, `ConnectionPool`, `UpcasterRegistry`).
//! Every behavior body seeds and asserts through `Arc<dyn StorageBackend>` /
//! `Arc<dyn ColdStorage>` only (the anti-coupling gate, T14), so the same body runs
//! unchanged against any backend.

use std::sync::Arc;

use async_trait::async_trait;

#[cfg(feature = "archive")]
use crate::storage::ColdStorage;
use crate::storage::{SqliteBackend, StorageBackend};

use super::fixtures::DIM;

/// A backend's test-construction surface. The one thing #633 implements to gain the
/// whole battery.
#[async_trait]
pub trait ConformanceBackend: Send + Sync + 'static {
    /// A fresh, empty, **writable** backend, migrated to HEAD, at `DIM`.
    async fn make(&self) -> Arc<dyn StorageBackend>;

    /// A backend whose writes are rejected with [`MemoryError::ReadOnly`], reads OK.
    ///
    /// [`MemoryError::ReadOnly`]: crate::error::MemoryError::ReadOnly
    async fn make_read_only(&self) -> Arc<dyn StorageBackend>;

    /// A writable backend that ALSO exposes cold storage. The two handles MUST point
    /// at the **same** underlying store (a fact inserted via the [`StorageBackend`]
    /// handle is visible to a `commit_archive_atomic` on the [`ColdStorage`] handle).
    #[cfg(feature = "archive")]
    async fn make_with_cold(&self) -> (Arc<dyn StorageBackend>, Arc<dyn ColdStorage>);

    /// Break the `facts` table so the next write inside an atomic method faults
    /// mid-transaction (the crash-injection seam for the all-or-nothing proofs).
    ///
    /// Default = portable `raw_exec("DROP TABLE facts")`; a backend whose dialect
    /// needs different SQL (e.g. PG `DROP TABLE facts CASCADE` under real FK
    /// constraints) overrides THIS ONE method — keeping crash-injection out of the
    /// behavior bodies so #633 never edits a body.
    ///
    /// # Errors
    ///
    /// Returns whatever the backend's `raw_exec` returns on a SQL/backend failure.
    async fn break_facts_table(&self, be: &Arc<dyn StorageBackend>) -> crate::error::Result<()> {
        be.raw_exec("DROP TABLE facts").await
    }

    /// Stable identity for assertion messages.
    fn name(&self) -> &'static str;
}

/// The always-on `SQLite` factory (in-memory pool).
pub struct SqliteFactory;

#[async_trait]
impl ConformanceBackend for SqliteFactory {
    async fn make(&self) -> Arc<dyn StorageBackend> {
        // Mirrors the oracle helper `src/storage/sqlite/mod.rs:476` (`memory_backend`).
        // `open_memory` runs the migration chain at open, so HEAD is reached without
        // a separate `.migrate()` call.
        let pool = crate::pool::ConnectionPool::open_memory(DIM).expect("open in-memory pool");
        Arc::new(SqliteBackend::from_pool(
            Arc::new(pool),
            Arc::new(crate::store::upcaster::UpcasterRegistry::new()),
        ))
    }

    async fn make_read_only(&self) -> Arc<dyn StorageBackend> {
        // Mirrors `src/storage/sqlite/mod.rs:530-551`: init a real file via a RW pool,
        // drop it, reopen read-only. The tempdir is LEAKED for the process
        // (`TempDir::keep`) so the file outlives this fn — a read-only test
        // opens→asserts→exits and the OS reaps it at process end. This avoids
        // threading a teardown guard through the macro and polluting the trait.
        use crate::pool::ConnectionPool;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("conformance-ro.db");
        {
            let _rw = ConnectionPool::open(&path, DIM, 2, None).expect("init rw file");
        }
        let ro = ConnectionPool::open_read_only(&path, DIM, 2).expect("reopen read-only");
        // `keep()` (tempfile >= 3.27) returns the `PathBuf` directly (NOT a `Result`) and
        // relinquishes cleanup, so the file outlives this fn — a read-only test
        // opens -> asserts -> exits and the OS reaps it at process end.
        let _kept = dir.keep();
        Arc::new(SqliteBackend::from_pool(
            Arc::new(ro),
            Arc::new(crate::store::upcaster::UpcasterRegistry::new()),
        ))
    }

    #[cfg(feature = "archive")]
    async fn make_with_cold(&self) -> (Arc<dyn StorageBackend>, Arc<dyn ColdStorage>) {
        // One Arc<SqliteBackend> unsized twice → both handles share state
        // (`SqliteBackend` impls both the `StorageBackend` blanket and `ColdStorage`).
        let pool = crate::pool::ConnectionPool::open_memory(DIM).expect("open in-memory pool");
        let be = Arc::new(SqliteBackend::from_pool(
            Arc::new(pool),
            Arc::new(crate::store::upcaster::UpcasterRegistry::new()),
        ));
        let cold: Arc<dyn ColdStorage> = be.clone();
        let storage: Arc<dyn StorageBackend> = be;
        (storage, cold)
    }

    fn name(&self) -> &'static str {
        "sqlite"
    }
}

/// The inert Postgres factory (#633 fills the `todo!()`s and deletes the `#[ignore]`).
///
/// Compiles under `--features backend-postgres` (the bodies typecheck via the never
/// type); never runs, because the `postgres` suite's emitted tests are `#[ignore]`d
/// until #633.
#[cfg(feature = "backend-postgres")]
pub struct PgFactory;

#[cfg(feature = "backend-postgres")]
#[async_trait]
impl ConformanceBackend for PgFactory {
    async fn make(&self) -> Arc<dyn StorageBackend> {
        todo!("#633: spin up a postgres testcontainer, migrate, return Arc<PgBackend>")
    }

    async fn make_read_only(&self) -> Arc<dyn StorageBackend> {
        todo!("#633: read-only PG role / transaction-scoped read-only")
    }

    #[cfg(feature = "archive")]
    async fn make_with_cold(&self) -> (Arc<dyn StorageBackend>, Arc<dyn ColdStorage>) {
        todo!("#633: PG cold storage (or decide PG has no cold tier)")
    }

    fn name(&self) -> &'static str {
        "postgres"
    }
}
