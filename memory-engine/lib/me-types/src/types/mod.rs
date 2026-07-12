//! Core domain types, split by concept (#401). Flat re-exports preserve the
//! `crate::types::*` public API.
mod activity;
/// Cold-storage archive DTOs (`ArchiveManifestEntry`).
#[cfg(feature = "archive")]
pub mod archive;
mod cognitive;
/// Consolidation-pipeline seam types.
pub mod consolidation;
/// Dream-cycle report DTOs (the delta-based `CycleReport` vocabulary, R7).
pub mod cycle_report;
mod events;
mod facts;
/// Forgetting/prune policy (`ForgetPolicy`) and output (`PruneStats`).
pub mod forgetting;
/// Inspection statistics + dump-format DTOs.
pub mod inspect;
/// `MemoryQuery` — relocated from `me-query` (Wave 2 #816 / S4, sub-PR 3a).
///
/// Private: re-exported as the single canonical path [`crate::types::search::MemoryQuery`]
/// rather than flat/duplicated here (this module itself is private, so it cannot carry a
/// resolvable public intra-doc link back to its own contents).
mod memory_query;
mod provenance;
mod relation;
mod scope;
/// Search/query result vocabulary (`SearchResult`/`SearchQuery`/`QueryResponse`…).
pub mod search;
/// Snapshot DTOs (the serde sidecar projections).
///
/// Kept as a namespaced submodule rather than flat-re-exported: these are internal
/// wire types, not part of the flat `crate::types::*` public vocabulary.
pub mod snapshot;

pub use activity::*;
pub use cognitive::*;
pub use events::*;
pub use facts::*;
pub use provenance::*;
pub use relation::*;
pub use scope::*;
