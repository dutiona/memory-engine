//! Core domain types, split by concept (#401). Flat re-exports preserve the
//! `crate::types::*` public API.
mod activity;
mod cognitive;
/// Dream-cycle report DTOs (the delta-based `CycleReport` vocabulary, R7).
pub mod cycle_report;
mod events;
mod facts;
mod provenance;
mod relation;
mod scope;
/// Search/query result vocabulary (`SearchResult`/`SearchQuery`/`QueryResponse`…).
pub mod search;
/// Snapshot DTOs (the serde sidecar projections). Kept as a namespaced submodule
/// rather than flat-re-exported: these are internal wire types, not part of the
/// flat `crate::types::*` public vocabulary.
pub mod snapshot;

pub use activity::*;
pub use cognitive::*;
pub use events::*;
pub use facts::*;
pub use provenance::*;
pub use relation::*;
pub use scope::*;
