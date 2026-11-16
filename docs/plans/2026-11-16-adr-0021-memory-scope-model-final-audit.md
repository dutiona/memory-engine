# Final audit: ADR-0021 plan (post-review)

- Scope: post-review.
- Approved plan: `docs/plans/2026-11-16-adr-0021-memory-scope-model.md`, SHA-256
  `e57f688bebe459b33e3e7c54546ea116a120047f024043e6bb1f0947e3f2e31a` (approved by the owner's marker on #1048).
- Audited head: `c2b60e060235df35130bebb3ba93d6e1bbae82d7` (PR #1051, after the `/super-review` round-1 fix).

| # | Plan item | Status | Evidence |
| --- | --- | --- | --- |
| 1 | Rebase, preflight, verified facts, plan commit | Implemented | rebased onto `a932f8e`; ancestry check passed; worktree locked and clean; docs-build baseline 31 warnings with `plans` excluded; private deny file present; every Context citation re-read at its file; P1 proven to fail on probe files (rc 1, 1, 0 as specified); plan commit `c2aa1b6` |
| 2 | Write ADR-0021 and list it in the index | Implemented | commit `c0b265f`; heading and anchor check clean; 35 case rows, 9 mapping rows; docs build 31 warnings, none on the touched pages |
| 3 | Mark ADR-0007 as amended | Implemented | commit `6e4003d`; the `**Amended by:**` grep prints 1 |
| 4 | Mark ADR-0005 as amended | Implemented | commit `ba682c3`; the `**Amended by:**` grep prints 1 |
| 5 | Reconcile with ADR-0020 | Implemented | checked against `origin/docs/1014-adr-0020` at `019e900` (PR #1047 open): path key `<machine>:<path>`, labels, reads, `project` exact match and unknown-key error, `set_level`, `forget` authority, call-record facts all agree; no text change needed; re-checked before READY |
| 6 | Open the PR | Implemented | PR #1051; `Closes #1015`, `Refs dutiona/infra#37`; body scanned by P1 (rc 0) |
| 7 | `/super-review` at the Light gate | Implemented | Codex (headless, resumed plan-review thread) and agy (resumed conversation), the owner's preferred voices; round 1: Codex 1 HIGH, 1 MEDIUM, agy LGTM; fix commit `c2b60e0`; round 2: both LGTM; all relayed to the PR as attributed reviews |
| 8 | `/finish-pr` | Implemented | this run: plan archived on #1048, working copy removed, checklist at the pushed head |
| 9 | Comment on knowledge-base#513 | Implemented after READY | posted once the PR reports READY, as the plan sequences it |

Counts: 9 Implemented, 0 Modified, 0 Dropped, 0 Incomplete, 0 Added. Divergence: 0 %.

Notes:
- The two review findings fixed in `c2b60e0` restore rules the approved plan already states (the call record is part
  of the ADR-0001 log, §12; an unknown `project` key is an error, §5), so they are not divergence.
- The Light gate's advisor is not available in this harness; per the owner's review preference, Codex and agy were the
  voices and no Claude subagent lens ran on the PR.
