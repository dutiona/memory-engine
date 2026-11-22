# Implementation audit: follow-up group A1 (#1126), plan v2

- Scope: pre-review.
- Approved plan: `docs/plans/2026-11-22-1126-backfill-identity-v2.md`, SHA-256
  `4cff7c9059b9a3ab9c4fcd295f3fdfe6b4e657b0404b8b50432921c5220131e0` (approved on #1134).
- Audited head: `607eb3de40ff2e7585ec61db6515cee6051ee5ea`, on `origin/main` `7ba301e`.

## Items

| Item | Status | Commit | Note |
|---|---|---|---|
| 0 | Implemented | 34c8366 | |
| 1 (#1097) | Implemented | 9199c5e | Green on main as planned: no code change. |
| 2 | Implemented | 8e19e80 | |
| 3 | Implemented | 5e10fab | A clippy fix to its test fixture was squashed in before review. |
| 4 (#1049) | Implemented | e05db0c | |
| 5 (#1065) | Implemented | 72c14a6 | |
| 6 | Implemented | d807051 | Two unused imports and a stale test-module line it left were squashed in before review. |
| 7 | Implemented | 3bbe07d | Landed before item 6; neither depends on the other. |
| 8 (#1081) | Implemented | 19e29e8 | `tracing::warn!` names the fact for a rejected fact that holds a vector; any other error propagates unchanged, carrying its own message. |
| 9 | Implemented | 9e0bd1d | `manage.rs`'s "(= 14)" inside the historical account of the stamp bug is kept: it states what the constant was then. |
| 10 (#1061) | Implemented | 3726b1f | |
| 11 (#1064) | Implemented | 5770335 | Two doc comments that quoted `"ollama"` as an example lost the quotes, so the plan's grep shows only `KNOWN_PROVIDERS`. |
| 12 (#1060) | Implemented | 475400f | The `store_lock.rs` `add-fact` invocation gained the identity flags with the other existing invocations. |
| 13 (#1101) | Implemented | b25b952 | Red was shown with a stubbed loader (both unit tests failed) rather than a compile failure. |
| 14 | Implemented | c8d869c | |
| 15 (#1131) | Implemented | 607eb3d | Red: the import took id 3. `streaming_output_matches_snapshot_schema` pins `sequences`, since the field is `#[serde(default)]`. Goldens reviewed: the empty store marks `scopes: 1`, the populated one `facts: 1, scopes: 1`. |
| 16 | Implemented (no-op so far) | | `origin/main` is still `7ba301e`, which holds #1026; #1127 has not merged. Redone if main moves before the merge. |
| 17 | Pending | | The PR, after this audit and the gate. |
| 18 | Pending | | Review and `/finish-pr`. |

## Counts

Implemented 16 (items 0–16), Modified 0, Dropped 0, Incomplete 0, Added 0; items 17 and 18 are the
lifecycle steps after this audit. Divergence (Modified + Dropped + Added) is 0 of 19 items: 0%.

## Found while implementing

- `crates/memory-engine-mcp/tests/server_process.rs` `a_killed_server_stores_a_resent_event_once` failed once
  in 9 runs on this branch (the resent event was answered `stored`, not `duplicate`); it passed 3 runs alone and
  4 more full-file runs. The branch does not touch the lifecycle endpoint. Recorded as collateral for triage.
