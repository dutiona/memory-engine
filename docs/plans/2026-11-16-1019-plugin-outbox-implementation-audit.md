# Implementation audit (pre-review) — 2026-11-16-1019-plugin-outbox

- Scope: pre-review.
- Approved plan: `docs/plans/2026-11-16-1019-plugin-outbox.md`, SHA-256
  `25aca0c4b3f4a49f44c7b1dd70df79a83915f0f98e2a70e07e85cb4537f44fad` (approval marker on plan issue #1055,
  2026-11-16T22:39:17Z). The plan's bytes are unchanged.
- Implementation head audited: `c0ce6a28ee6ddd7f1b2c39e69ae6a9921c3ba2ae` (branch `feat/1019-plugin-outbox`, rebased on
  `origin/main` 93bcb5d).

## Items

| # | Item | Status | Notes |
|---|---|---|---|
| 1 | Manifest, marketplace, hook registration, harness | **Modified** | D2's handler passes only the subcommand; the options come from the environment (owner's decision below) |
| 2 | Queue each hook input with its envelope | Implemented | The seq is reserved by a noclobber create, not `mkdir` (a technology swap that keeps §12's semantics; separate commit `fix(plugin): reserve the seq with an exclusive create`). `tree_heals`' park half lives in item 5 as `park_heals`, beside the flush it needs |
| 3 | Real `claude -p` capture | Implemented | `stub_server.py` landed with item 5, whose real-curl cases needed it first; `real-claude.sh` landed with item 6's commit by an over-broad `git add`, and its final form with the test commit after the owner's decision |
| 4 | Project facts | Implemented | |
| 5 | Delivery and reactions | Implemented | Also `endpoint_update_fail` and `endpoint_changed_mid_flush`, moved here from item 2 by revision 10 |
| 6 | One flusher, stale locks, pruning | Implemented | |
| 7 | Stable copy, no-environment flush | Implemented | |
| 8 | SessionStart line | Implemented | |
| 9 | Latency | Implemented | Desktop p95 13.2 ms; macos-mini p95 52.0 ms, information only (Q4) |
| 10 | Documentation | Implemented | Includes the approved Q7/Q8 ADR lines and the §12 "exclusive create" wording the owner asked for |
| — | `chore(pm): add area:plugin to the label manifest and the labeling tables` | **Added** | Boy-scout, its own commit: the label the owner created was missing from the manifest the github-pm scripts sync from |

Counts: Implemented 8, Modified 1, Added 1, Dropped 0, Incomplete 0. Divergence (Modified + Dropped + Added) / 10 =
**20%**, under the 30% threshold: no successor plan.

## Deviation from an acceptance criterion (owner's decision)

The issue says: "Configuration comes from non-sensitive `userConfig` (ME URL, dev root) through exec-form hook
arguments." Measured on Claude Code 2.1.326: an exec-form hook whose args reference an unset option as
`${user_config.*}` does not run at all (`Plugin option "dev_root" isn't set`), a `default` of `""` does not count as
set, and `claude plugin install --config dev_root=` is refused as empty. A machine without a dev root would capture
nothing, and no machine would capture before `me_url` is set.

Michaël's decision, relayed by the orchestrator: "Let's go with youre recommendation, option A". The hooks stay exec
form (`/bin/sh ${CLAUDE_PLUGIN_ROOT}/bin/me-hook <subcommand>`), and the helper reads `CLAUDE_PLUGIN_OPTION_ME_URL` and
`CLAUDE_PLUGIN_OPTION_DEV_ROOT`, which Claude Code exports for every option that is set. Every hook runs whatever is
configured, and events queue before `me_url` is set. Commit `fix(plugin): read the plugin's options from the hook
environment`; real-run phase U (dev_root unset) is green. The owner's explicit decision stands in for a successor plan,
at his instruction to record it as an audit deviation.

## Other findings during implementation

- uutils coreutils 0.10.0 `mkdir` (Ubuntu 26.04 default) is not exclusive under concurrency: 9 duplicate reservations
  in 180 against 0 for GNU `mkdir`, macOS `mkdir` and the noclobber create.
- The token file the owner provided holds a `NAME=token` line; `real-claude.sh` accepts that form and a bare token.
- The detached flush outlived `claude -p` in one real run (2 of 4 requests after exit) and not in another (0 of 4);
  the README states both.
