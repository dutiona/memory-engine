# Advisor capability unavailable

Altitude report SHA-256: d48ba36512a55fbe8715f794a04f9bb6acbc92d489bfca7e0b7f481394724de1

Reason: `advisor()` is not exposed in this harness (the fleet worker's tool list has no advisor tool), in every
round. The selected gate is Full, so it ran in its advisor-less form: Codex (headless, `gpt-6.2-sol` at `medium`) +
agy (`gemini-4-hydrogen-medium`) + one clean-slate Claude Opus subagent, five rounds each. No finding is attributed to
an advisor; each fallback reviewer's findings are in its own artifact (`-external-codex.md`, `-external-agy.md`,
`-subagent-review.md`). The digest above is the final altitude report, recorded as controller metadata; all three
fallback reviewers acknowledged it in round 5.

## Resolution

Five rounds (plan digests 5b442d86, 0f4b532e, f7a6ccfd, 95be6d15, 27da3da7). Final plan SHA-256
27da3da724cf0c8603096ce1844ca56f43ddebfaffd7165ecef6f110e16fe162; final altitude report SHA-256
d48ba36512a55fbe8715f794a04f9bb6acbc92d489bfca7e0b7f481394724de1. Round 5: Codex LGTM, agy LGTM, subagent LGTM, no
findings.

Round 1 (Codex 4 HIGH / 4 MEDIUM / 1 LOW; agy LGTM; subagent 1 BLOCKER / 3 MEDIUM / 7 LOW):
- [BLOCKER/HIGH] Block 3 overclaimed for ME and dropped "recorded with the space" → the shared paragraph states the
  ruling whole, adds "The Memory layer departs from this rule as its paragraph states.", says "prepends the
  instruction, if any", "document vectors under one identity"; the Memory-layer paragraph states the deployment's
  duties for both gaps. Codex's "Other models default to no instruction" not taken: false for ME's code as a shared
  default; the departure sentence carries the gap.
- [MEDIUM] Ruling scope (KB-scoped in #606) → *Reported to Michaël* says so and offers a KB-scoped successor.
- [HIGH] Approval record / post-approval text change → item 2 records each marker with `approve-plan --type
  github-comment`; item 8 routes a text change through a successor plan.
- [HIGH] False post-merge parity → item 9 checks each blob read and compares digests with `APPROVED_ADR_SHA`.
- [MEDIUM] Rebase and line numbers → the base ADR digest is pinned; a moved base stops for Michaël.
- [MEDIUM] Approved-text check after Item 5 → runs before Item 5; final HEAD runs parity plus digest.
- [MEDIUM] Tesseract → required; absent = INCOMPLETE.
- [MEDIUM] Leak check → read-through before every publication; grep is a supplement with stated exit codes and wider
  patterns.
- [MEDIUM] Stale ME design-doc table → item 6, an ME-only boy-scout commit with exact text.
- [MEDIUM] Reconstruction/recommendation wording → adopted.
- [LOW] `artifact_sha256` in Block 2; evidence rows; grep exit note; open-PR re-check; KB re-read list → adopted.
  Subagent LOW 3 (library providers) covered by "the HTTP client".

Round 2 (Codex 2 HIGH / 1 MEDIUM / 1 LOW; agy LGTM; subagent LGTM + 2 LOW):
- [HIGH] Item 8 successor order → rewritten in the state machine's order (supersede-plan, successor plan issue,
  approve-plan; then exact-match replacement).
- [HIGH] Open-PR re-check would match the pair's own PRs → excludes only this pair's two PR numbers.
- [MEDIUM] Memory-layer paragraph read as current behaviour; unproven deployment claim → paragraph opens with what the
  code does and states agreement as a duty; the claim now cites dutiona/infra's quadlet and deploy test.
- [LOW] Knowledge-layer default → stated by model name containing `qwen3-embedding`, ignoring case.
- [LOW] Pin the expected ADR digest → `APPROVED_ADR_SHA` = 2a13502a3a24211bfff7e2627faf4963bd1acd05ba1db024587474cddf080b1b,
  confirmed independently by the subagent and Codex.
- [LOW] Operator-doc sentence in ME's mcp-server.md → not taken: nothing contradicts the ADR, the default meets the
  duty, the pair stays minimal; reported as optional.

Round 3 (Codex 2 HIGH / 1 LOW; agy LGTM; subagent LGTM + 3 LOW):
- [HIGH] Archive target after a successor → "the plan issue recorded for the active approval".
- [HIGH] Successor handoff skipped commit/push → commit, push, read back heads, update bodies, rebind, re-review.
- [LOW] "sets none" misreadable → "leaves it unset, so it uses the pinned instruction".
- [LOW] Successor publication hygiene; indentation → adopted.

Round 4: [LOW] (subagent, optional) "carries the new block" could be misread as a partial successor → arbitrated, not
applied: plan-audit requires a versioned successor to be "a complete plan, not a diff", and applying it would void
three fresh LGTMs over a contingency path for a wording that cannot change the outcome. The subagent accepted the
arbitration in round 5.

Round 5 (revision 5, found by the worker while dry-running the plan issues): three bare issue references in prose
repository-qualified (dutiona/knowledge-base#612, dutiona/memory-engine#1017, dutiona/knowledge-base#606), two lines
rewrapped; ADR blocks and `APPROVED_ADR_SHA` unchanged. All three reviewers LGTM with no findings.

Degradations: `advisor()` unavailable in this harness (Full ran as Codex + agy + subagent).
