# Advisor capability unavailable

Altitude report SHA-256: 91d6ae10c23f1303faa5959c49aa691043368aa0ce10331b8a293fe8e7d0fcce

(Controller metadata: the digest of the final altitude report every fallback reviewer acknowledged; earlier revisions
bound 0fe1139e…, 8b02f136…, bf837855…, 3ce919be…, bce9d20e…, 68ee8b89…, 31793d81…, b750a3ab….)

Reason: `advisor()` is not exposed in this fleet worker's harness (no advisor tool is in its tool list), so Step 3a
cannot run. The selected gate is Full; per `debate-defaults` it drops to its advisor-less form: Codex (headless) +
agy (resume) + one clean-slate Claude subagent (Step 3b). No finding below is attributed to an advisor.

Findings of the fallback reviewers are recorded, attributed, in
`docs/plans/2026-11-17-lean-mcp-surface-subagent-review.md` (Claude subagent) and in the persistent-review state of
`/super-plan` (Codex shared file and the agy round receipts).

## Resolution

No advisor findings exist to resolve: the advisor leg did not run. The fallback reviewers' findings are resolved in
`docs/plans/2026-11-17-lean-mcp-surface-subagent-review.md` (Claude subagent) and in the round changelogs sent to
Codex and agy. Degraded: Step 3a advisor → advisor-less Full (Codex + agy + subagent) (`advisor()` not exposed).
