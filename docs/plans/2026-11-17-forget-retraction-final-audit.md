# Final audit (post-review): forget as a reversible retraction (#1023)

Scope: `post-review`.
Approved plan: `docs/plans/2026-11-17-forget-retraction.md`, SHA-256
`756ffce9c47c3256a32cf8403d72323667c1e773f0661028eea794b233ff52cc` (approved on plan issue #1087).
Implementation head audited: `96d68f9` (the reviewed code; this file is the only later change).
Pre-review audit: `docs/plans/2026-11-17-forget-retraction-implementation-audit.md` (head `7775ebb`).

## Review

`/super-review`, Full gate, Default severity, four rounds on PR #1091.

| Round | Head | agy (gemini-3.8-flash-high) | OpenCode (hy4-flash-free) | Vibe (Mistral) | Opus 5.5 lens |
|---|---|---|---|---|---|
| 1 | `f0cfa0d` | 1 LOW | 1 LOW | 4 LOW | 1 MEDIUM, 8 LOW |
| 2 | `6213073` | LGTM | LGTM | 2 LOW | LGTM, 3 LOW |
| 3 | `9313824` | — | — | — | LGTM (pre-completion check) |
| 4 | `96d68f9` | — | — | — | LGTM, 1 LOW: the pre-review audit's item 11 note is stale. That file is immutable, so this file's item 11 row supersedes it. |

Codex was degraded: its usage limit refused round 1, with a reset at 12:20 UTC. Every finding was fixed except two that were rebutted (below). The PR comments record each round.

## Items

The pre-review statuses stand: items 0 to 17 are Implemented. Item 18 is now Implemented: the parity note is on #634, the PR is #1091, the review is done, and the prune rename is #1089. The review adds these intent-preserving variations to the ones already named:

| Item | Variation | Why it preserves the intent |
|---|---|---|
| 10 (D10) | Erase **deletes** every activity that promoted a taken fact, for both targets. D10's text says to keep one outside the bucket and set its link to NULL. The dead unlink after the DELETE is gone (aa51617). | D10's own reason holds: an activity's `args` and `result_summary` carry the erased content, and D11 promises no copy in the live store. The port docs and the CLI reference already describe the deletion. |
| 6 (D9) | The facade calls the port even when no Home resolves, and the port answers `NotFound`. D9 says to answer `NotFound` without a port call. | The call record is written in the forget's own transaction (item 5's variation), and ADR-0021 §12 needs one record per call. Skipping the port would drop the record. |
| 11 | Erase validates its target, and the write is granted, before the sidecar is touched (b3b2f19). The sidecar is removed inside the transaction, before the commit, and only when a fact is taken (96d68f9). | The crash-safety reason holds, because no commit ever precedes the removal. A refused or empty erasure leaves the file alone. |

Rebutted review findings:
- The `fact_ids` record keys (Vibe): items 5 and 12 already name them.
- The empty Homes reaching the port (Vibe): see item 6 above.

Counts: 19 Implemented, 0 Modified, 0 Dropped, 0 Added. Divergence is **0%**, and no variation changes an owner decision.
