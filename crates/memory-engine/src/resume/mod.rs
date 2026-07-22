//! Session bootstrapping: 4-tier context retrieval (pinned, high-importance,
//! due, scope-filtered recent).

pub mod context;

pub use context::{ResumeConfig, ResumeContext};
