//! Core domain types, split by concept (#401). Flat re-exports preserve the
//! `crate::types::*` public API.
mod activity;
mod cognitive;
mod events;
mod facts;
mod provenance;
mod scope;

pub use activity::*;
pub use cognitive::*;
pub use events::*;
pub use facts::*;
pub use provenance::*;
pub use scope::*;
