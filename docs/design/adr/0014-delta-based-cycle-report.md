# ADR 0014: Delta-based CycleReport + retrieve-before-reflect DreamCycle

**Status:** Accepted (Phase 5a, #49)
**Date:** 2026-06-16

## Context

Phase 5a makes the cognitive pipeline runnable. The trait contract (`DreamCycle`,
`DreamContext`, `CycleReport`) shipped in PR #228, but `CycleReport` was a bag of
counts (`facts_promoted: usize`, …) and `DreamCycle::run` received only the
capability bag. Two research findings reshaped this before the default implementation
landed:

- **Context collapse (DC, arXiv:2504.07952):** a monolithic LLM rewrite of accumulated
  context collapsed 18,282 tokens → 122 in one step, dropping accuracy _below_ the
  no-adaptation baseline. A counts-based report forces a consumer `DreamCycle` to _do_
  the mutation itself and report a tally — there is no auditable, validatable, replayable
  record, and an LLM implementation is free to wholesale-rewrite.
- **Incremental deltas (ACE, arXiv:2510.04618):** structured, itemized updates merged
  deterministically by non-LLM code avoid collapse by construction.
- **Retrieve-before-reflect (DC-RS > DC-Cu):** refining/retrieving prior state before
  generating consistently beats blind cumulative processing.

## Decision

1. **`CycleReport` is a delta log.** `CycleReport { deltas: Vec<CycleDelta>, identity,
metadata }`. `CycleDelta` is a typed, bounded vocabulary — `AddFact`, `AdjustScore`,
   `Quarantine`, `Promote`, `TagOutcome`, `Supersede`. A cycle _proposes_ deltas; the
   engine _validates and applies_ them. `CycleDelta` is `#[non_exhaustive]` (R9/R13 in
   #578 add variants). `CycleReport`/`CycleMetadata` are **not** `#[non_exhaustive]` —
   external `DreamCycle` implementations must construct them, which `#[non_exhaustive]`
   would forbid.

2. **`apply_cycle_report` is all-or-nothing.** Validate every delta against a read-only
   snapshot on the single held write connection, then apply all in one transaction. A
   malformed delta leaves the store byte-identical. `Promote` reuses the shared
   `promote_in_conn` pipeline and `TagOutcome` inserts on the shared transaction —
   neither re-acquires the (non-reentrant) connection lock.

3. **Retrieve-before-reflect `CycleContext`.** `DreamCycle::run` receives a
   `CycleContext` that **wraps** `DreamContext` (preserving the capability bag by
   composition) and adds `prior_wisdom`, `prior_reports`, and the `time_window`. The
   engine owns retrieval; the consumer owns reflection.

4. **Produce/apply split.** `run_dream_cycle` returns the _unapplied_ report;
   `apply_cycle_report` is a separate call. This realizes the issue's pipeline step 4
   ("present patterns for human review before any promotion") — the report is the review
   surface.

5. **`AdjustScore` quantum (`IMPORTANCE_STEP = 0.05`).** `adjustment: i16` is a count of
   quanta (±2/cycle, cumulative); the store delta `adjustment * STEP` is applied to the
   **base `importance`** column, not the materialized `importance_score` (which decay
   recomputes and would overwrite a direct adjustment).

6. **Quarantine = soft-expiry + marker, no migration.** `Quarantine` sets `t_expired`
   (transaction-time) and writes a `quarantine` metadata marker — reusing the
   `t_expired IS NULL` retrieval filter (exclusion is free) while keeping the row for
   mining. `ExpiredReason::Quarantined` + `determine_state` reading the marker keep it
   distinguishable from Ebbinghaus forgetting. The dream-cycled idempotency marker is
   likewise a `metadata` key + a `last_dream_cycle_at` config watermark — schema stays
   at v11.

7. **`Supersede` is a graph edge, not lineage.** `expire(old)` + an
   `EdgeStore` edge `new → old` with `relation_type = "supersedes"`. Routing through
   `lineage` would collide on `UNIQUE(wisdom_fact_id)` when `new` is also a `Promote`
   target and has no natural `provenance` value.

## Consequences

- **Breaking** vs PR #228: `DreamCycle::run` signature and `CycleReport` shape changed.
  Blast radius was core-internal (no CLI/MCP/embed consumer read `CycleReport`).
- The shipped `DefaultDreamCycle` is pure-Rust and deterministic, hence itself immune to
  context collapse; the delta vocabulary's protection is for _consumer_ LLM impls.
- A full `run_dream_cycle` is **not** globally atomic with consolidation, because the
  pure producer does not run consolidation at all — operators schedule `consolidate()`
  separately. Single-transaction cycle + content-based correction detection + abstract
  pattern extraction (R9/R13) are deferred to #578; identity computation to #57.

## References

- ACE (arXiv:2510.04618), DC (arXiv:2504.07952); `docs/design/debate-phase5/synthesis.md`.
- Implementation plan: `docs/plans/2026-06-16-dreamcycle-r7r8-default-impl.md`.
