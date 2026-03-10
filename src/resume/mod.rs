//! Session bootstrapping: 3-tier context retrieval (identity, core, recent).

pub(crate) mod context;

pub use context::{ResumeConfig, ResumeContext, resume_context};
