//! The dream-cycle subsystem (Phase 5a).
//!
//! - [`report`] — the delta-based [`CycleReport`] vocabulary (R7).
//!
//! Further submodules (`context`, `apply`, `dbscan`, `default_impl`) are added by
//! later tasks. Public items are re-exported flat from the crate root (`lib.rs`).

mod apply;
mod context;
mod dbscan;
mod default_impl;
mod llm_impl;
mod report;

pub use context::CycleContext;
pub use default_impl::DefaultDreamCycle;
pub use llm_impl::LlmDreamCycle;
pub use report::{
    ApplyResult, CycleAnomaly, CycleDelta, CycleMetadata, CycleOutcome, CycleReport,
    IMPORTANCE_STEP, IdentityOutput, MAX_ADJUSTMENT, SkipReason, TimeWindow,
};
