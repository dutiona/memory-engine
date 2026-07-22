//! # me-cognitive
//!
//! The **Cognitive/dream-cycle primitive** (Phase 5a): `DreamCycle` orchestration
//! (produce → review → apply) over [`MemoryCtx`](me_storage::MemoryCtx) plus the
//! [`DreamCtx`](me_traits::DreamCtx) capability trait. Wave 2 (#816), slice S5,
//! sub-PR 2 — closes #981.
//!
//! ## Why this crate exists, and why it's the last public-API break of Wave 2
//!
//! ADR 0014 (Accepted, Phase 5a, #49) decision #3 designed a **capability bag**
//! (`DreamContext`, a struct narrowing `&MemoryEngine` to query/list/consolidate/
//! forget/promote/`list_undreamt_in_period`/`outcome_counts*`) that `CycleContext`
//! wrapped **by composition**, explicitly to preserve it. S1 of this epic re-typed
//! `DreamCycle::run` from a concrete `&CycleContext` to `&dyn CycleCtx` — necessary so
//! `me-traits` (L0.5) never names the engine/consolidation type that owns the cycle's
//! read-set — but that made `DreamContext` **unreachable**: its constructor was
//! `pub(crate)`, its sole accessor was `CycleContext::dream()`, and a `&dyn CycleCtx`
//! cannot be downcast. S1 copied only the two methods the shipped cycles happened to
//! call (`list_undreamt_in_period`, `outcome_counts_batch`) onto `CycleCtx` itself,
//! stranding the other seven with zero call sites — a regression **no ADR amendment
//! recorded** (see the ADR 0014 amendment) and no green build could catch
//! (`dead_code` does not fire on `pub` items). The zero call sites were the
//! **symptom** of that regression, not evidence the capabilities were dead: they are
//! an enabler for open work (#578 `DreamCycle` vNext, #231 promote integration tests,
//! #554/#627 the LLM cycle backend).
//!
//! This crate (S5) restores the contract **and** carves the dream-cycle subsystem out
//! of the facade in the same move — the two were coupled: `DreamContext` held
//! `engine: &'a MemoryEngine`, so as long as the bag was a facade struct, carving the
//! subsystem into any L3 crate would have created an illegal L3 → L4 back-edge (this
//! is why the pre-S5 `me-consolidate` crate doc scoped the dream layer explicitly out
//! — see its own history). Promoting the bag into [`me_traits::DreamCtx`] (with
//! [`me_traits::CycleCtx`] as a supertrait, inheriting rather than re-duplicating
//! `list_undreamt_in_period`/`outcome_counts_batch`) removes the back-edge natively: a
//! trait object needs no downcast and names no engine type. `MemoryEngine` is the
//! (only) implementor, in the facade; this crate never depends on it —
//! `cargo tree -p me-cognitive -i memory-engine` reports no match.
//!
//! ## What moved here, what stayed
//!
//! - [`CycleContext`] — now holds `&dyn DreamCtx` directly (the `.dream()` indirection
//!   is flattened: a consumer calls `ctx.query(...)` / `ctx.promote(...)` directly on
//!   the `&dyn CycleCtx` it is handed).
//! - `dbscan` — the pure DBSCAN clustering core (crate-private; no engine state,
//!   always was crate-local).
//! - [`DefaultDreamCycle`] / [`LlmDreamCycle`] — the shipped `DreamCycle` producers.
//!   Their `run()` bodies needed **zero logic changes**: they call
//!   `ctx.list_undreamt_in_period(...)` / `ctx.outcome_counts_batch(...)` on `&dyn
//!   CycleCtx`, which still resolves — now via the `DreamCtx` supertrait instead of
//!   `CycleCtx`'s own (now-removed) duplicated methods.
//! - [`apply_cycle_report`] — the validate-all-then-apply-all transactional delta
//!   applier, now a free function over `MemoryCtx` + `&RwLock<MemoryGraph>`.
//! - [`run_dream_cycle`] / [`run_dream_cycle_guarded`] — the produce/apply-split
//!   orchestration, now free functions over `MemoryCtx` + `&dyn DreamCtx`.
//!
//! The engine's own `DreamCtx` implementation stays in the facade — it is carried by a
//! private `EngineDreamCtx(&MemoryEngine)` newtype there, **not** by
//! `impl DreamCtx for MemoryEngine` (see the trap below for why). `promote_with_lineage`
//! similarly stays: it caches newly resolved scope ids into the engine's in-memory
//! `ScopeTree`, an engine-owned cache this crate's `MemoryCtx` does not carry (ADR 0018
//! decision #3 — `scope_tree` is a loose per-primitive parameter, not part of the
//! universal ctx).
//!
//! ## ⚠️ The recursion trap (read before writing ANY new `DreamCtx` implementor)
//!
//! **Never `impl DreamCtx for T` when `T` has inherent methods sharing those names.**
//!
//! Five of `DreamCtx`'s nine method names collide with inherent `MemoryEngine` methods
//! (`query`, `list_active_facts`, `get_fact`, `consolidate`, `forget`). In such an impl
//! the body must call the same-named inherent method on `self`; Rust resolves
//! inherent-before-trait, so it works — **only while that inherent method keeps its
//! name**. Rename it and the call silently re-resolves to *the trait method being
//! defined*: unbounded recursion, stack overflow, in the **consumer's** process.
//!
//! Two things make it lethal rather than theoretical, both verified empirically:
//!
//! - **Qualification does NOT protect.** `Self::query(self, q)` and
//!   `MemoryEngine::query(self, q)` have the **same resolution order** as `self.query(q)`.
//!   Qualifying is cosmetic. (An earlier draft of this crate's docs recommended exactly
//!   that — it was wrong.)
//! - **`rustc`'s `unconditional_recursion` lint does NOT fire through `#[async_trait]`**
//!   — the recursive call lands inside the desugared `Box::pin(async move { … })`, not
//!   the function's own CFG. It compiles clean even under `-D warnings`.
//!
//! So no gate in this repo catches it. **The rule is structural, not stylistic:** route
//! the impl through a newtype whose inner type has *no* `DreamCtx` impl in scope, so only
//! the inherent method can ever match and a rename becomes `E0599` at the call site. That
//! is what `EngineDreamCtx` is for.
//!
//! There are exactly **two** `DreamCtx` implementors, and neither can recurse:
//!
//! - **`EngineDreamCtx`** (facade, private) — the newtype above.
//! - **[`CycleContext`]** (this crate) — safe **by construction**, not by the newtype
//!   rule: it has no inherent method sharing a `DreamCtx` name, and every body forwards
//!   to a *different value* (`self.dream`, a held `&dyn DreamCtx`), never to `self` under
//!   the same name.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod apply;
mod cognitive;
mod context;
mod dbscan;
mod default_impl;
mod llm_impl;

#[cfg(test)]
mod test_support;

pub use apply::apply_cycle_report;
pub use cognitive::{
    CALLER_WRITE_CURSOR, INSIGHT_MARKER_KEY, run_dream_cycle, run_dream_cycle_guarded,
};
pub use context::CycleContext;
pub use default_impl::DefaultDreamCycle;
pub use llm_impl::LlmDreamCycle;
pub use me_types::types::cycle_report::{
    ApplyResult, CycleAnomaly, CycleDelta, CycleMetadata, CycleOutcome, CycleReport,
    IMPORTANCE_STEP, IdentityOutput, MAX_ADJUSTMENT, SkipReason, TimeWindow,
};
