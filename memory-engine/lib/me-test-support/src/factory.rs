//! The `ConformanceBackend` factory trait — the seam a backend implements to gain
//! the entire cross-backend contract battery (#632).
//!
//! `make*` are `async` even though `SqliteBackend` builds synchronously: the #635
//! `PgFactory` will spin up a testcontainer (`async`), and the emission macro
//! `.await`s every `make*`, so the trait's async-ness is what makes that a drop-in.
//! (#633 added `PgBackend`'s `SchemaManager` + pool + migrations, but `PgBackend` is
//! not a full `StorageBackend` until #634+#635, so `PgFactory::make()` stays `todo!()`.)
//!
//! This is the **only** module in this crate permitted to name a concrete backend
//! type (`SqliteBackend`, `ConnectionPool`, `UpcasterRegistry`).
//! Every behavior body seeds and asserts through `Arc<dyn StorageBackend>` /
//! `Arc<dyn ColdStorage>` only (the anti-coupling gate, T14), so the same body runs
//! unchanged against any backend.

use std::sync::Arc;

use async_trait::async_trait;

use me_backend_sqlite::SqliteBackend;
#[cfg(feature = "archive")]
use me_storage::ColdStorage;
use me_storage::StorageBackend;

use super::fixtures::DIM;

/// A backend's test-construction surface. The one thing a backend implements to gain
/// the whole battery (for `PgBackend`, that lands in #635 once it is a full `StorageBackend`).
#[async_trait]
pub trait ConformanceBackend: Send + Sync + 'static {
    /// A fresh, empty, **writable** backend, migrated to HEAD, at `DIM`.
    async fn make(&self) -> Arc<dyn StorageBackend>;

    /// A backend whose writes are rejected with [`MemoryError::ReadOnly`], reads OK.
    ///
    /// [`MemoryError::ReadOnly`]: me_types::error::MemoryError::ReadOnly
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
    /// behavior bodies so #635 never edits a body.
    ///
    /// `test-util`-gated: `raw_exec` is itself gated on the port's `test-util` feature
    /// (a cross-crate crash-injection escape hatch that cannot ride `cfg(test)`), and
    /// this crate forwards `test-util` only as an opt-in feature — so this method exists
    /// exactly when the `#[cfg(all(test, feature = "test-util"))]` battery that calls it
    /// (the atomic-rollback bodies) is compiled. A default `--workspace` build compiles
    /// neither, keeping `raw_exec` off the required-trait-item set globally.
    ///
    /// # Errors
    ///
    /// Returns whatever the backend's `raw_exec` returns on a SQL/backend failure.
    #[cfg(feature = "test-util")]
    async fn break_facts_table(&self, be: &Arc<dyn StorageBackend>) -> me_types::error::Result<()> {
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
        let pool =
            me_backend_sqlite::pool::ConnectionPool::open_memory(DIM).expect("open in-memory pool");
        Arc::new(SqliteBackend::from_pool(
            Arc::new(pool),
            Arc::new(me_storage::UpcasterRegistry::new()),
        ))
    }

    async fn make_read_only(&self) -> Arc<dyn StorageBackend> {
        // Mirrors `src/storage/sqlite/mod.rs:530-551`: init a real file via a RW pool,
        // drop it, reopen read-only. The tempdir is LEAKED for the process
        // (`TempDir::keep`) so the file outlives this fn — a read-only test
        // opens→asserts→exits and the OS reaps it at process end. This avoids
        // threading a teardown guard through the macro and polluting the trait.
        use me_backend_sqlite::pool::ConnectionPool;
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
            Arc::new(me_storage::UpcasterRegistry::new()),
        ))
    }

    #[cfg(feature = "archive")]
    async fn make_with_cold(&self) -> (Arc<dyn StorageBackend>, Arc<dyn ColdStorage>) {
        // One Arc<SqliteBackend> unsized twice → both handles share state
        // (`SqliteBackend` impls both the `StorageBackend` blanket and `ColdStorage`).
        let pool =
            me_backend_sqlite::pool::ConnectionPool::open_memory(DIM).expect("open in-memory pool");
        let be = Arc::new(SqliteBackend::from_pool(
            Arc::new(pool),
            Arc::new(me_storage::UpcasterRegistry::new()),
        ));
        let cold: Arc<dyn ColdStorage> = be.clone();
        let storage: Arc<dyn StorageBackend> = be;
        (storage, cold)
    }

    fn name(&self) -> &'static str {
        "sqlite"
    }
}

/// The inert Postgres factory.
///
/// #635 fills the `todo!()`s and deletes the `#[ignore]`, once `PgBackend` implements
/// all six bounded traits and is a full `StorageBackend`; #633 added only its
/// `SchemaManager` + pool + migrations.
///
/// Compiles under `--features backend-postgres` (the bodies typecheck via the never
/// type); never runs, because the `postgres` suite's emitted tests are `#[ignore]`d
/// until #635.
#[cfg(feature = "backend-postgres")]
pub struct PgFactory;

#[cfg(feature = "backend-postgres")]
#[async_trait]
impl ConformanceBackend for PgFactory {
    async fn make(&self) -> Arc<dyn StorageBackend> {
        todo!("#635: spin up a postgres testcontainer, migrate, return Arc<PgBackend>")
    }

    async fn make_read_only(&self) -> Arc<dyn StorageBackend> {
        todo!("#635: read-only PG role / transaction-scoped read-only")
    }

    #[cfg(feature = "archive")]
    async fn make_with_cold(&self) -> (Arc<dyn StorageBackend>, Arc<dyn ColdStorage>) {
        todo!("#635: PG cold storage (or decide PG has no cold tier)")
    }

    fn name(&self) -> &'static str {
        "postgres"
    }
}
