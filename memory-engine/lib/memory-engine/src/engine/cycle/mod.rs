//! The dream-cycle subsystem (Phase 5a).
//!
//! - the delta-based [`CycleReport`] vocabulary (R7) now lives in
//!   [`crate::types::cycle_report`] (Wave 2 #816).
//!
//! Further submodules (`context`, `apply`, `dbscan`, `default_impl`) are added by
//! later tasks. Public items are re-exported flat from the crate root (`lib.rs`).

mod apply;
mod context;
mod dbscan;
mod default_impl;
mod llm_impl;

pub use crate::types::cycle_report::{
    ApplyResult, CycleAnomaly, CycleDelta, CycleMetadata, CycleOutcome, CycleReport,
    IMPORTANCE_STEP, IdentityOutput, MAX_ADJUSTMENT, SkipReason, TimeWindow,
};
pub use context::CycleContext;
pub use default_impl::DefaultDreamCycle;
pub use llm_impl::LlmDreamCycle;
