# Advisor capability unavailable

Reason: `advisor()` is not exposed in this worker's harness (no advisor tool in the session's tool list), so Step 3a
could not run in any round. The selected gate is **Full**; its advisor-less form is Codex + agy + one clean-slate
subagent, and all three reviewed every round (ten rounds, rounds 7–10 the delta after Michaël's ruling B; final
reviewed plan SHA-256 `e57f688bebe459b33e3e7c54546ea116a120047f024043e6bb1f0947e3f2e31a`). Recorded here as controller metadata, the
altitude report the fallback reviewers assessed in the final round:

Altitude report SHA-256: 5d24fd5c194894b0cb005b1c9e86bd85cde4ad58f97506c48656d2833f037848

No advisor findings exist. The findings the fallback reviewers produced live in their own artifacts, attributed to
them: `docs/plans/2026-11-16-adr-0021-memory-scope-model-subagent-review.md` (clean-slate Claude subagent, rounds 1–10),
`docs/plans/2026-11-16-adr-0021-memory-scope-model-external-codex.md` (Codex headless, final round; rounds 1–9 in
`-external-codex-r1.md` … `-r9.md`) and `docs/plans/2026-11-16-adr-0021-memory-scope-model-external-agy.md` (agy,
final round; rounds 1–9 in `-external-agy-r1.md` … `-r9.md`). Advisor triage, the advisor tie-break and the
pre-completion advisor check were replaced by the orchestrator's own arbitration, recorded in each artifact's
Resolution.

## Resolution

Degradation recorded: Step 3a → fallback to the Full gate's advisor-less form (Codex + agy + subagent), reason: the
`advisor()` tool is not available to this worker. No advisor finding to resolve.
