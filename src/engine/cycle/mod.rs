//! The dream-cycle subsystem (Phase 5a).
//!
//! - [`report`] — the delta-based [`CycleReport`] vocabulary (R7).
//!
//! Further submodules (`context`, `apply`, `dbscan`, `default_impl`) are added by
//! later tasks. Public items are re-exported flat from the crate root (`lib.rs`).

pub(crate) mod apply;
pub(crate) mod context;
pub(crate) mod dbscan;
pub(crate) mod default_impl;
pub(crate) mod report;

pub use context::CycleContext;
pub use default_impl::DefaultDreamCycle;
pub use report::{
    ApplyResult, CycleAnomaly, CycleDelta, CycleMetadata, CycleReport, IdentityOutput, TimeWindow,
    IMPORTANCE_STEP, MAX_ADJUSTMENT,
};
