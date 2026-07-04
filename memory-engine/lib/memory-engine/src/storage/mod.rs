//! The persistence **port**, now carved into the [`me_storage`] crate (Wave 2 #816, L1).
//!
//! The port trait family — the six bounded-context traits ([`FactGraph`], [`EventLog`],
//! [`SearchIndex`], [`ConsolidationStore`], [`SessionStore`], [`SchemaManager`])
//! aggregated by the [`StorageBackend`] umbrella, the feature-gated `ColdStorage`
//! trait, the closed [`FactFilter`]/[`TemporalFilter`] query vocabulary, and
//! [`MemoryCtx`] — lives in `me-storage`. This module **re-exports** it (both the
//! submodules and the flat trait names) so every existing `crate::storage::graph::FactGraph`
//! and `crate::storage::FactGraph` path keeps resolving.
//!
//! [`SqliteBackend`] (`sqlite`) carved into [`me_backend_sqlite`] in this same sub-PR
//! (2b): this module re-exports it too (`pub use me_backend_sqlite::sqlite;` +
//! `pub use me_backend_sqlite::SqliteBackend;`), so `crate::storage::sqlite::*` and
//! `crate::storage::SqliteBackend` both keep resolving unchanged. `PgBackend`
//! (`postgres`, feature-gated) and the cross-backend `conformance` battery stay here
//! until #634/#635. No SQL string or driver type crosses the port — that contract
//! lives in `me-storage` (traits) / `me-backend-sqlite` (the `SQLite` impl).

#[cfg(feature = "backend-postgres")]
pub mod postgres;
pub use me_backend_sqlite::sqlite;

/// Cross-backend conformance battery (#632) — asserts the `StorageBackend` CONTRACT
/// against `Arc<dyn StorageBackend>` directly. Test-only; see its module docs.
///
/// Gated on `test-util` (not bare `test`) because it drives the `SchemaManager::raw_exec`
/// failure-injection seam, which — since the #816 split moved the trait into `me-storage`
/// — is only present when the `test-util` feature is on (a cross-crate escape hatch cannot
/// ride `cfg(test)`). CI's `--all-features` test job runs it; bare `cargo test` no longer
/// does (run `cargo test --features test-util` locally to exercise it).
#[cfg(all(test, feature = "test-util"))]
mod conformance;

// --- The port surface, re-exported from the me-storage crate. ---
// Submodule re-exports so `crate::storage::<portmod>::Trait` module paths still resolve.
#[cfg(feature = "archive")]
pub use me_storage::cold_storage;
pub use me_storage::{
    backend, capabilities, consolidation, ctx, event_log, filter, graph, schema, search_index,
    session,
};
// Flat trait/type re-exports so `crate::storage::Trait` paths still resolve.
#[cfg(feature = "archive")]
pub use me_storage::ColdStorage;
pub use me_storage::{
    BackendCapabilities, BootstrapIngestOutcome, ConsolidationStore, EventLog, FactFilter,
    FactGraph, LexicalRanker, MemoryCtx, MetadataPredicate, SchemaManager, SearchIndex,
    SessionStore, StorageBackend, TemporalFilter,
};

// --- The concrete backend impls. `PgBackend` remains in the monolith until #634/#635. ---
pub use me_backend_sqlite::SqliteBackend;
#[cfg(feature = "backend-postgres")]
pub use postgres::PgBackend;
