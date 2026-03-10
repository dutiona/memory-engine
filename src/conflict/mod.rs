//! Bi-temporal conflict resolution via consumer-provided [`ConflictArbiter`](crate::traits::ConflictArbiter).

mod temporal;

pub use temporal::resolve_conflict;
