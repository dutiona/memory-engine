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
//! What stays here (until S2 carves the backends into `me-backend-{sqlite,postgres}`):
//! the concrete impls [`SqliteBackend`] (`sqlite`) and `PgBackend` (`postgres`,
//! feature-gated), plus the cross-backend `conformance` battery. No SQL string or
//! driver type crosses the port — that contract now lives in `me-storage`.

#[cfg(feature = "backend-postgres")]
pub mod postgres;
pub mod sqlite;

/// Cross-backend conformance battery (#632) — asserts the `StorageBackend` CONTRACT
/// against `Arc<dyn StorageBackend>` directly. Test-only; see its module docs.
#[cfg(test)]
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
    BackendCapabilities, ConsolidationStore, EventLog, FactFilter, FactGraph, LexicalRanker,
    MemoryCtx, MetadataPredicate, SchemaManager, SearchIndex, SessionStore, StorageBackend,
    TemporalFilter,
};

// --- The concrete backend impls (remain in the monolith until S2). ---
#[cfg(feature = "backend-postgres")]
pub use postgres::PgBackend;
pub use sqlite::SqliteBackend;
