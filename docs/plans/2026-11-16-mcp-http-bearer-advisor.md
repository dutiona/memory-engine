# Advisor capability unavailable

Reason: `advisor()` is not exposed in this fleet worker's harness (no advisor tool in its tool list), so Step 3a, the
advisor triage after each round, the tie-break and the pre-completion check could not call it.

Fallback, per the Full gate's advisor-less form (debate-defaults, Review Gate Selection): Codex + agy + one
clean-slate subagent. All three ran:

- clean-slate subagent (Claude, Opus): round 1 on plan `4006dc1c…`, delta rounds 2 and 3, LGTM on the final plan
  `611a7deb…`; findings and resolution in `docs/plans/2026-11-16-mcp-http-bearer-subagent-review.md`;
- Codex (headless, gpt-6.1-sol, thread `01a1e664-a07f-7d21-8954-3298c2649c5d`): round 1 two [MEDIUM], rounds 2 and 3 LGTM;
  `docs/plans/2026-11-16-mcp-http-bearer-external-codex.md`;
- agy (gemini-4-argon-high, conversation `39d9f277-5dd6-4481-9065-b909235a4ad8`): LGTM in all three rounds;
  `docs/plans/2026-11-16-mcp-http-bearer-external-agy.md`.

No finding here is attributed to an advisor. Triage, severity classification and the pre-completion check were done
by the orchestrating worker (Claude Opus 5.5) on technical merit; no reviewer disagreement needed a tie-break (agy's
LGTM and Codex's two [MEDIUM] findings were compatible: both findings were accepted and fixed).

Controller metadata, the altitude report the fallback reviewers assessed in the final round:
Altitude report SHA-256: 52a3efe3d3a5c61e8638f35bcc31aa9b1088a1466f4c96889f97b020992084a7

## Resolution

Advisor absent: recorded as a degradation (`Degraded: super-plan Step 3a advisor → Codex + agy + subagent (advisor()
not exposed in the worker harness)`). The fallback reviewers' findings were all addressed:

- Codex round 1 [MEDIUM] item 5's red already green after item 2 → the in-process Host tests moved into item 2;
  item 5's red is the CLI plumbing (unknown flag → clap exit 2) plus the `parse_allowed_host` unit tests.
- Codex round 1 [MEDIUM] the fail-closed guard is untested → `calling_machine(&Extensions)` is `call_tool`'s first
  `?`, with unit tests for empty `Extensions`, `Parts` without `Machine`, and both.
- Subagent round 1 findings → see that artifact's Resolution section.
- Codex rounds 2 and 3 and agy rounds 1-3: LGTM, nothing to address. The subagent's round-3 [LOW] wording
  finding was arbitrated without a plan change (see that artifact).
