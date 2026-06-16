# Advisor Review — MCP Cognitive Endpoints (#225)

## Availability

`advisor()` is **not available** in this environment. Per the user's standing budget preference
(prefer adversarial subagent reviewers over external-model quota), the advisor's critical-review
role was covered by **two independent clean-slate subagent reviewers** (Step 3b mandatory + one
adversarial specialist):

- `…-subagent-review.md` — general-purpose, fresh-eyes (line-ref verification + structural completeness).
- (adversarial rust-specialist findings folded into the same artifact's "Adversarial" section.)

The plan is complex (>5 files, >1 subsystem), which triggers Step 4; the full Codex/Gemini/agy tmux
loop was deliberately substituted by the two subagents above per the budget preference. The
`gemini-code-assist[bot]` will also auto-review the PR.

## Resolution

Not applicable — `advisor()` did not run. Findings + resolutions are recorded in the subagent-review
artifact. Net: the adversarial pass found 1 BLOCKER (T3 `extra_conditions` can't carry a bind param;
switched to a standalone query + trusted-literal `json_type` predicate) + 2 HIGH (T7 non-object
metadata normalization; T4 explicit early-return on unknown scope) + several LOW (re-export line,
`#[must_use]`, CycleError mis-tier note) — all addressed in the plan before approval.
