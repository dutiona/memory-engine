Reviewer: Codex gpt-6.1-terra (medium effort) via codex-headless-round.sh, thread resumed across 3 rounds

~~~~markdown
Michaël, **LGTM for the round 3 delta**. Confidence is high.

Altitude report SHA-256: d8a10cecd79e4476149af04c15ec45a7e4d1d1c383b870c7d60fd6a0d5f4a99c

The refreshed altitude report’s **proceed** disposition is justified, and its plan digest matches. The transactional generation bump addresses the pinning race; the measurement table now states the serialization conditions explicitly and stops on ambiguous results.

This was a read-only delta review. No tests, builds, or gates were run.

REVIEW COMPLETE
~~~~

## Resolution

Round 1 (plan 815c0ffb…):
- [HIGH] watermark compare-and-set ABA (absent key cleared again by a reconstruction; a value returning to what the
  snapshot read): fixed in revision 2. The value compare-and-set is replaced by `consolidation_generation`, a counter
  every rewind increments; `apply_plan` checks it first in its transaction and discards the whole plan when it changed.
- [HIGH] a reconstruction between compute and apply makes the plan's expirations stale: fixed in revision 2 by the
  same generation check (a reconstruction's `promote_space` rewinds, so it bumps the generation and the plan is
  discarded); test `a_reconstruction_between_load_and_apply_discards_the_stale_plan`.
- [MEDIUM] unpinning leaves duplicates uncompared: fixed in revision 2; `FactStore::set_pinned` rewinds on unpin; tests
  `unpinning_rewinds_the_dedup_watermark`, `dedup_compares_an_unpinned_fact`; the accepted-residual line is removed.
- [MEDIUM] measurement load conflicts with the one-heavy-run rule: resolved in revision 2 as explicit owner point O7
  (one lock hold, bounded load, memory-pressure abort); it stands only if Michaël approves it with the plan.
- [MEDIUM] absent `tc` has no retry count: fixed in revision 2 (an inconclusive row, an event-stream validity rule, the
  count only over runs with a `tc`).

Round 2 (plan bf444030…):
- [HIGH] pinning a planned loser between compute and apply: fixed in revision 3; a real pin transition bumps the
  generation (`bump_consolidation_generation`, no rewind); test
  `pinning_a_planned_loser_between_load_and_apply_discards_the_plan`; the mutation list includes the pin bump.
- [MEDIUM] the M row was ambiguous after the inconclusive row: fixed in revision 3 (M's full conditions written; a
  multi-row or no-row match stops).

Round 3 (plan 41d31a8b…, the final bytes): LGTM, no finding.

Degradations: none. The Codex model is `gpt-6.1-terra` by Michaël's 2026-11-21 roster ruling, not a degradation.
