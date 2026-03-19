//! Inspection APIs for debugging and observability.
//!
//! Provides introspection into engine state: fact explanations, temporal history,
//! event replay, state dumps, and aggregate statistics.

pub mod dump;
pub mod explain;
pub mod replay;
pub mod restore;
pub mod statistics;
pub mod types;

pub use types::*;
