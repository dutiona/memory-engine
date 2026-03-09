# Phase 3b Planning Prompt

Use this prompt to start a new Claude Code session for planning and implementing Phase 3b.

---

## Prompt

```
I need you to plan the implementation of Phase 3b ("Temporal Memory & Agent Lifecycle") for memory-engine.

## Context

memory-engine is a Rust library for AI agent long-term memory. Read these files first:

1. `docs/ROADMAP.md` — full project history, phases 1-3 complete/in-progress
2. `docs/plans/2026-03-09-future-phases-design.md` — the approved design doc for Phase 3b (section "Phase 3b: Temporal Memory & Agent Lifecycle")
3. The Phase 3 worktree at `.worktrees/phase3/` — this is the current codebase you'll be building on

## What Phase 3b delivers

All design decisions are already made. No brainstorming needed. The 6 features are:

1. **Unforgettable flag** — `is_pinned: bool` on facts, forgetting bypass, `PersistenceClassifier` trait
2. **Future memory** — `t_valid` filter so facts with future dates surface when due
3. **Scheduling API** — `drain_due(now)` + `next_due_time()` new methods
4. **`resume_context()` rework** — tiered retrieval pipeline (pinned → high-importance → due → scope-filtered → KB stubs)
5. **Materialized importance score** — persist `importance_score: f64` on facts, update on access/consolidate/forget
6. **Event envelope forward-compat** — add `origin_node_id`, `sequence_id`, advisory `created_at` to events table

## What I need from you

Use /super-plan (or the writing-plans skill) to create a detailed implementation plan. The plan should:

- Work on the Phase 3 worktree branch (`feat/memory-engine-phase3`)
- Account for the existing schema migration framework (v1→v2 already done in Phase 3)
- Order tasks by dependency (e.g., schema migration before trait, trait before engine wiring)
- Include test requirements for each task
- Be specific about which files need changes (read the codebase first)

Do NOT implement anything yet. Just plan.
```

---

## Notes for the side session

- The Phase 3 worktree is at `.worktrees/phase3/` on branch `feat/memory-engine-phase3`
- Schema is currently at v2 (Phase 3 added scope_id). Phase 3b will migrate to v3.
- The `AddFactOptions` builder already exists — `is_pinned` can be added there
- `resume_context()` already exists but is basic — it needs a full rework, not a patch
- The event envelope change is a schema migration + struct change, no behavioral change
- All trait patterns are established: `EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter` — follow the same pattern for `PersistenceClassifier`
