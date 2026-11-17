# Advisor capability unavailable

Altitude report SHA-256: a87264a4b2022d00e1279264929d13e25dae3b340915723a56d333cf9cc04702

Reason: `advisor()` is not exposed in this harness (the tool is absent from the worker's tool list).

The Full gate dropped to its advisor-less form: Codex (headless) + Antigravity + a clean-slate Claude subagent
(Opus). Their findings are recorded in `2026-11-17-1068-offline-build-subagent-review.md` and in the external review
artifacts, attributed to the reviewer that produced them. No finding here is attributed to an advisor.

Round 2 assessed altitude report:

Altitude report SHA-256: be8c26b4300063f0af25f363164967de325c6373201d79cd93113fcfa6747c68

## Resolution

No advisor findings exist (capability unavailable). The fallback reviewers' findings are resolved in their own artifacts: Codex 1 MEDIUM + 1 LOW, the subagent 2 MEDIUM + 4 LOW, Antigravity LGTM; round 2 was LGTM from all three.
