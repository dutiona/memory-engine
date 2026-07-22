//! Backend-internal inspection functions relocated from the facade (Wave 2 #816 / S2,
//! sub-PR 2b).
//!
//! Only `sqlite::schema`'s `SchemaManager` impl calls these (`statistics`/
//! `dump_state`). `pub` purely so the facade can re-export the two submodules
//! (`pub(crate) use me_backend_sqlite::inspect::{dump, statistics};`) — neither is
//! meant as a stable path for anything outside this workspace.

pub mod dump;
pub mod statistics;
