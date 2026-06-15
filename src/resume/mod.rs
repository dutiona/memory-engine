//! Session bootstrapping: 5-tier context retrieval (pinned, high-importance,
//! due, scope-filtered recent, KB stubs).

pub mod context;

pub use context::{ResumeConfig, ResumeContext, resume_context};
