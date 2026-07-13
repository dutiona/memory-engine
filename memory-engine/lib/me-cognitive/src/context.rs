//! Retrieve-before-reflect cycle context (R8).
//!
//! [`CycleContext`] is what the engine hands to [`DreamCycle::run`](me_traits::DreamCycle::run).
//! It **holds** a `&dyn DreamCtx` (the capability bag — query/consolidate/promote/
//! forget/`list_undreamt_in_period`/`outcome_counts*`) and adds the retrieved prior
//! state — promoted wisdom and recent cycle metadata — plus the time window to
//! process. Reflecting against accumulated state (rather than the full corpus
//! blindly) is the DC/ACE "retrieve & synthesize" discipline: it lets a cycle avoid
//! re-deriving already-promoted patterns and detect drift against existing wisdom.
//!
//! Wave 2 #816 / S5 (closes #981): before this, `CycleContext` **wrapped** a
//! concrete `DreamContext` struct (`engine: &'a MemoryEngine`) by composition — the
//! ADR 0014 decision #3 shape. That struct lived in the facade, which is why the
//! whole dream-cycle subsystem could not be carved out of it (an L3 → L4 back-edge).
//! S5 inverts the bag into the `me-traits` [`DreamCtx`](me_traits::DreamCtx) trait and
//! `CycleContext` now holds a trait object directly — no facade type, no downcast,
//! and the old `.dream()` indirection is flattened: a consumer calls
//! `ctx.query(...)` / `ctx.promote(...)` directly on the `&dyn CycleCtx` it is handed.
//!
//! The capability bag is borrowed for the duration of the call, so `CycleContext` is
//! intentionally not `Serialize`/`'static`; only the persisted
//! [`CycleMetadata`](super::CycleMetadata) crosses the storage boundary.

use me_types::error::Result;
use me_types::types::Fact;

use super::{CycleMetadata, TimeWindow};

/// Context passed to [`DreamCycle::run`](me_traits::DreamCycle::run).
///
/// The capability bag (query / list / consolidate / forget / promote /
/// `list_undreamt_in_period` / `outcome_counts*`) is available directly on `&dyn
/// CycleCtx` via the [`DreamCtx`](me_traits::DreamCtx) supertrait; the retrieved
/// prior state is exposed by [`CycleContext::prior_wisdom`] /
/// [`CycleContext::prior_reports`] / [`CycleContext::time_window`].
pub struct CycleContext<'a> {
    dream: &'a dyn me_traits::DreamCtx,
    prior_wisdom: Vec<Fact>,
    prior_reports: Vec<CycleMetadata>,
    time_window: TimeWindow,
}

impl<'a> CycleContext<'a> {
    /// Construct a cycle context. The orchestration free functions
    /// ([`crate::run_dream_cycle`]) build this via [`build_cycle_context`](crate::cognitive::build_cycle_context).
    pub(crate) const fn new(
        dream: &'a dyn me_traits::DreamCtx,
        prior_wisdom: Vec<Fact>,
        prior_reports: Vec<CycleMetadata>,
        time_window: TimeWindow,
    ) -> Self {
        Self {
            dream,
            prior_wisdom,
            prior_reports,
            time_window,
        }
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

/// `CycleContext` is the concrete implementation of the
/// [`CycleCtx`](me_traits::CycleCtx) read surface the
/// [`DreamCycle`](me_traits::DreamCycle) contract names — the seam (Wave 2 #816) that
/// keeps the trait layer free of any engine/consolidation type.
/// `list_undreamt_in_period` / `outcome_counts_batch` are **not** implemented here:
/// they resolve by inheritance from the `DreamCtx` supertrait, straight through to
/// the held `dream` reference (`&dyn DreamCtx` auto-derefs to itself — no forwarding
/// impl needed).
#[async_trait::async_trait]
impl me_traits::CycleCtx for CycleContext<'_> {
    fn time_window(&self) -> TimeWindow {
        self.time_window
    }

    fn prior_wisdom(&self) -> &[Fact] {
        &self.prior_wisdom
    }

    fn prior_reports(&self) -> &[CycleMetadata] {
        &self.prior_reports
    }
}

/// `CycleContext` also implements `DreamCtx` directly — by forwarding every method to
/// the held `dream` reference — so it satisfies the [`CycleCtx`](me_traits::CycleCtx)
/// supertrait bound. This is the one place the "fully-qualify to dodge the recursion
/// trap" rule does **not** apply: `self.dream` is a *different* value from `self`, so
/// `self.dream.query(query)` is an ordinary, non-recursive delegation (contrast
/// `impl DreamCtx for MemoryEngine` in the facade, where `self` names the same value
/// twice and an unqualified same-name call would self-recurse).
#[async_trait::async_trait]
impl me_traits::DreamCtx for CycleContext<'_> {
    async fn query(
        &self,
        query: &me_types::types::search::SearchQuery,
    ) -> Result<Vec<me_types::types::search::SearchResult>> {
        self.dream.query(query).await
    }

    async fn list_active_facts(&self, limit: Option<usize>) -> Result<Vec<Fact>> {
        self.dream.list_active_facts(limit).await
    }

    async fn get_fact(&self, id: i64) -> Result<Fact> {
        self.dream.get_fact(id).await
    }

    async fn consolidate(
        &self,
        generator: std::sync::Arc<dyn me_traits::SummaryGenerator>,
        embedder: std::sync::Arc<dyn me_traits::EmbeddingProvider>,
        config: &me_traits::ConsolidationConfig,
    ) -> Result<me_traits::ConsolidationStats> {
        self.dream.consolidate(generator, embedder, config).await
    }

    async fn forget(
        &self,
        policy: &me_types::types::forgetting::ForgetPolicy,
    ) -> Result<me_types::types::forgetting::PruneStats> {
        self.dream.forget(policy).await
    }

    async fn promote(
        &self,
        req: &me_types::types::PromoteRequest,
    ) -> Result<me_types::types::PromotionResult> {
        self.dream.promote(req).await
    }

    async fn list_undreamt_in_period(&self, window: TimeWindow) -> Result<Vec<Fact>> {
        self.dream.list_undreamt_in_period(window).await
    }

    async fn outcome_counts(&self, fact_id: i64) -> Result<me_types::types::OutcomeCounts> {
        self.dream.outcome_counts(fact_id).await
    }

    async fn outcome_counts_batch(
        &self,
        fact_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, me_types::types::OutcomeCounts>> {
        self.dream.outcome_counts_batch(fact_ids).await
    }
}
