# Implementation audit (post-review): 2026-11-17-deferred-embedding-v2

- Scope: post-review (final)
- Approved plan: `docs/plans/2026-11-17-deferred-embedding-v2.md`, SHA-256
  `29441a37ea5919d78a66fd5bb3710bee52de2d0a4a7d143a010cfad9aaacbdde` (approved on #1083; D1 = B, option A
  deferred to #1084)
- Implementation head audited: `d898384` (PR #1094, on main `e34d304`)
- Supersedes the pre-review audit's commit references: every commit was rebased since, so the SHAs below are
  the branch's current ones.

| Item | Status | Evidence |
|---|---|---|
| 0 Worktree and plan | Implemented | plan issues #1079 (predecessor) and #1083; `340d17a` |
| 1 Schema v16 | Implemented | `959cecc`; renumbered v16 to v17 on the rebase onto #1023, which took v15 to v16 (intent unchanged) |
| 2 Search-path guards | Implemented | `9c9e687` |
| 3 Maintenance-path guards | Implemented | `f7436af`; review hardenings `8b137fb` (archive never selects a fact without a vector) and `4b71d50` (the watermark holds while any fact, active or not, lacks a vector) |
| 4 Embedder error classification | Modified | `4d0f230`; review added a fifth variant, `EmbedderError::NoAnswer` (timeout, reset, broken body), in `e88fdb9`, so a request that got no answer keeps the key-rejection pause; the plan's four variants classed a timeout as `Failed`, which cleared it |
| 5 Chunking and pooling | Implemented | `074f807`; review hardenings `e7bf0b2` (a `/tokenize` 413 falls back to the byte bound) and `277bd11` (split again by the server's `n_ctx` when it is below the declared limit) |
| 6 Recall's bounded query embed | Implemented | `bdf1be9` |
| 7 Deferred insert | Implemented | `e3cc195`; with `a64f783` the first waiting fact also records the store's dimension (`embed_dim`) while no identity exists, which item 9's change needs |
| 8 Drain steps and retry rules | Implemented | `2479b55`; review hardenings `4b5bd9c` (erase drops queue error text its fact left) and `ef807e3` (a failure settled after its item left the queue records nothing) |
| 9 `status` | Modified | `3f01ec0`; `e12697b` reads in one read transaction; `bbf1c9e`, `94bb98d` tighten the busy-writer test. The plan's `peek_embed_dim_from_db` refusal for a store whose facts all wait ("names the queue", test `a_store_whose_facts_all_wait_names_the_queue`) is gone: with the dimension recorded (item 7, and restore in `79d0c97`, `d898384`), such a store opens for `erase`, `unforget`, `dump`, `stats` and the rest, and the test is now `a_store_whose_facts_all_wait_has_its_dimension` |
| 10 `remember` defers, the drain | Implemented | `df0cb92` |
| 11 Never wait for a hung embed | Implemented | `d71baaf` |
| 12 Documentation | Implemented | `21b6f3b`; review-round doc fixes `3f46851`, `92abcb9`, `033a1b6` |
| 13 Boy-scout cli-inspector notes | Implemented | `15380b1` |
| 14 #634 and the parity proof | Implemented | #634 comment 6964257979 (v17, seven queue keys plus `embed_dim`); Postgres parity at `d898384`: 18 passed |
| 15 Rebase and reconcile | Implemented | rebased onto #1022, #1023 and #1096 (`e34d304`) |
| 16 PR | Implemented | #1094 |
| 17 `/super-review`, `/finish-pr` | Implemented | Full gate, four rounds to LGTM from Codex, agy and the clean-slate subagent; `/finish-pr` at this head |
| 18 Merge | owner | |
| — `style(mcp)` import order | Added | `84badf8`: rustfmt fallout of a rebase resolution |
| — `unforget` of a waiting fact | Added | `5f69449`: `unforget` of a forgotten fact still waiting for its vector failed and rolled back (it decoded the empty placeholder to re-index it) |
| — erase of a waiting fact | Added | `9b803c3`: test that erase leaves no queue row |

Counts: Implemented 15, Modified 2 (4, 9), Dropped 0, Incomplete 0, Added 3, owner 1 (18).
Divergence (Modified + Dropped + Added) / 19 = 5 / 19 = 26 %, under 30 %.

Flagged for the owner: item 9's change of behaviour. The approved plan had CLI commands refuse a store whose
facts all wait and point to `status`; after review they open it. It only widens what works, so it stays in this
audit rather than a successor plan.

Collateral boy-scout commits outside the plan's items, each its own commit: `7eb25ca`, `ec5d39b` (the
proposer's broken rustdoc links), `d311eaf` (api.md's stale signatures and error table), `06ed341` (the
"Unforgettable Facts {Planned}" heading).
