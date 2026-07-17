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
//! [`SqliteBackend`] (`sqlite`) carved into [`me_backend_sqlite`] in sub-PR 2b: this
//! module re-exports it too (`pub use me_backend_sqlite::sqlite;` + `pub use
//! me_backend_sqlite::SqliteBackend;`), so `crate::storage::sqlite::*` and
//! `crate::storage::SqliteBackend` both keep resolving unchanged. `PgBackend` carved
//! into `me_backend_postgres` in this same sub-PR (3): `pub use
//! me_backend_postgres::PgBackend;` (behind the `backend-postgres` feature — not an
//! intra-doc link, since the crate is an optional dependency absent from a default
//! build) keeps `crate::storage::PgBackend` resolving — there is no
//! `crate::storage::postgres` submodule path to preserve (nothing outside this file
//! ever referenced one). The cross-backend conformance battery moved to the
//! `me-test-support` crate (Wave 2 #816 / S3, sub-PR 1) — it dev-deps this crate's
//! ports directly and no longer lives under this module. No SQL string or driver
//! type crosses the port — that contract lives in `me-storage` (traits) /
//! `me-backend-sqlite` / `me-backend-postgres` (the concrete impls).

pub use me_backend_sqlite::sqlite;

// --- The port surface, re-exported from the me-storage crate. ---
// Submodule re-exports so `crate::storage::<portmod>::Trait` module paths still resolve.
#[cfg(feature = "archive")]
pub use me_storage::cold_storage;
pub use me_storage::{
    backend, capabilities, consolidation, ctx, event_log, filter, graph, offload, schema,
    search_index, session,
};
// Flat trait/type re-exports so `crate::storage::Trait` paths still resolve.
#[cfg(feature = "archive")]
pub use me_storage::ColdStorage;
pub use me_storage::{
    BackendCapabilities, BootstrapIngestOutcome, ConsolidationStore, EventLog, FactFilter,
    FactGraph, LexicalRanker, MemoryCtx, MetadataPredicate, SchemaManager, SearchIndex,
    SessionStore, StorageBackend, TemporalFilter, spawn_join_err,
};

// --- The concrete backend impls. ---
#[cfg(feature = "backend-postgres")]
pub use me_backend_postgres::PgBackend;
pub use me_backend_sqlite::SqliteBackend;
