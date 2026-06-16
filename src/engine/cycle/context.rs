//! Retrieve-before-reflect cycle context (R8).
//!
//! [`CycleContext`] is what the engine hands to [`DreamCycle::run`](crate::traits::DreamCycle::run).
//! It **wraps** the capability-restricted [`DreamContext`] (so the cycle keeps the
//! query/consolidate/promote write path) and adds the retrieved prior state — promoted
//! wisdom and recent cycle metadata — plus the time window to process. Reflecting
//! against accumulated state (rather than the full corpus blindly) is the DC/ACE
//! "retrieve & synthesize" discipline: it lets a cycle avoid re-deriving already-promoted
//! patterns and detect drift against existing wisdom.
//!
//! The capability bag is preserved by **composition**, not thrown away — `CycleContext`
//! borrows the engine for the duration of the call, so it is intentionally not
//! `Serialize`/`'static`; only the persisted [`CycleMetadata`](super::report::CycleMetadata)
//! crosses the storage boundary.

use crate::engine::cognitive::DreamContext;
use crate::types::Fact;

use super::report::{CycleMetadata, TimeWindow};

/// Context passed to [`DreamCycle::run`](crate::traits::DreamCycle::run).
///
/// Access the capability bag via [`CycleContext::dream`] and the retrieved prior
/// state via [`CycleContext::prior_wisdom`] / [`CycleContext::prior_reports`] /
/// [`CycleContext::time_window`].
pub struct CycleContext<'a> {
    ctx: DreamContext<'a>,
    prior_wisdom: Vec<Fact>,
    prior_reports: Vec<CycleMetadata>,
    time_window: TimeWindow,
}

impl<'a> CycleContext<'a> {
    /// Construct a cycle context. The engine builds this in `run_dream_cycle`.
    pub(crate) fn new(
        ctx: DreamContext<'a>,
        prior_wisdom: Vec<Fact>,
        prior_reports: Vec<CycleMetadata>,
        time_window: TimeWindow,
    ) -> Self {
        Self {
            ctx,
            prior_wisdom,
            prior_reports,
            time_window,
        }
    }

    /// The capability-restricted handle (query / list / consolidate / forget / promote).
    #[must_use]
    pub const fn dream(&self) -> &DreamContext<'a> {
        &self.ctx
    }

    /// Previously promoted wisdom facts (active, pinned). A cycle reads these to
    /// avoid re-detecting already-promoted patterns (generative-output isolation).
    #[must_use]
    pub fn prior_wisdom(&self) -> &[Fact] {
        &self.prior_wisdom
    }

    /// Metadata of recent prior cycles (newest last), reconstructed from the
    /// persisted history ring. Carries window/method info, not the full delta logs.
    #[must_use]
    pub fn prior_reports(&self) -> &[CycleMetadata] {
        &self.prior_reports
    }

    /// The window of facts this cycle was asked to process.
    #[must_use]
    pub const fn time_window(&self) -> TimeWindow {
        self.time_window
    }
}
