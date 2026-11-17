# Final audit (post-review) — 2026-11-16-1019-plugin-outbox

- Scope: post-review.
- Approved plan: `docs/plans/2026-11-16-1019-plugin-outbox.md`, SHA-256
  `25aca0c4b3f4a49f44c7b1dd70df79a83915f0f98e2a70e07e85cb4537f44fad` (plan issue #1055). Its bytes are unchanged.
- Post-review head audited: `cef5ea9bbc7081632aa7bd6a042125d1fc6b08c4`. Pre-review audit:
  `docs/plans/2026-11-16-1019-plugin-outbox-implementation-audit.md` (head `110086d`).

## What the review changed

`/super-review`, Full gate (Codex headless, Antigravity, a clean-slate Opus lens), three rounds, all three LGTM at
`cef5ea9`. Every change stays inside an existing plan item:

| Finding | Item | Commit |
|---|---|---|
| A header write failing midway published a truncated envelope | 2 | `fix(plugin): abandon an event whose envelope write fails` |
| SessionStart's token test differed from the flush's (empty or unreadable file) | 8 | `fix(plugin): report an unreadable or empty token file at session start` |
| A token file holding only a line feed passed as a token | 5, 8 | `fix(plugin): treat a token file holding only a line feed as no token` |
| `latency.py` measured an unconfigured capture after the options moved to the environment | 9 | `test(plugin): measure the configured capture, and fail when nothing is delivered` |
| `t_lock_unreadable` passed whatever the flush did; `t_lock_live` had one second of margin | 6 | `test(plugin): make the lock cases fail when their rule breaks` |
| The stub's unused delay and outcome header | 3 | `test(plugin): drop the stub's unused delay and outcome header` |
| Comment, README (`me_url`, exit code) and the ADR's worktree-root exception for `.git`-named layouts | 10 | `docs(plugin): correct the lock-break comment, …` |

## Items

Unchanged from the pre-review audit: Implemented 8, Modified 1 (item 1: the options come from the hook environment,
by Michaël's decision "Let's go with youre recommendation, option A"), Added 1 (the `area:plugin` manifest
commit), Dropped 0, Incomplete 0. Divergence **20%**, under the 30% threshold: no successor plan.

## Gates at the post-review head `cef5ea9`

- `claude plugin validate --strict plugins/memory-engine` and `.`: rc 0, rc 0.
- `sh plugins/memory-engine/tests/run.sh`: `# 72 ok, 0 not ok`, rc 0, on dash and on the macOS host's `/bin/sh`
  (bash 3.2.57).
- `real-claude.sh`: 10 checks, 0 not ok; `--marketplace dutiona/memory-engine#feat/1019-plugin-outbox`: 12, 0 not ok.
- `latency.py`: desktop `n=200 p50=10.5 p95=13.0 max=16.9 ms`, 210 requests delivered, rc 0; the macOS host, as
  information, p95 52.2 ms.
- The AGENTS.md cargo block: every command rc 0; `cargo test`: 2002 passed, 0 failed.
