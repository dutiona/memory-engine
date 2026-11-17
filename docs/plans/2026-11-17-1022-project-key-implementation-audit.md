# Implementation audit (pre-review): #1022

- Approved plan: `docs/plans/2026-11-17-1022-project-key.md`, SHA-256
  `dd796ce3cabe0e3b24f7bc1bcfae4989b96dbad6b29417f2b7e1d19181628400`, approved on the plan issue
  https://github.com/dutiona/memory-engine/issues/1082 (marker comment 6959320792).
- Implementation head audited: `c03f4b9bde2555fa860cf62b3245b3bc639257fe`.
- Scope: pre-review.

## Items

| Item | Status | Evidence |
|---|---|---|
| 0. Plan on the branch | Implemented | `baf9352` |
| 1. Decode the five facts and strip userinfo | Implemented | `2b726ae` |
| 2. Derive the key, rules v1; one Home type | Implemented | `48936bc` |
| 3. Scoped writes: `insert_event_at`, `move_fact_atomic` | Implemented | `16ece17` |
| 4. Every call reads its facts and records them | Implemented | `3e43137` |
| 5. `recall` reads the session's project | Implemented | `b79dfdf` |
| 6. `remember(content, level?)`: Home, receipt, hint | Implemented | `16f9f9a` |
| 7. ADR-0021 §8 amendment | Implemented | `fe6c641`, cites dutiona/infra#37 comment 6959376409 |
| 8. `set_level(id, level)` | Implemented | `dc617c9` |
| 9. Documentation | Implemented | `e6f9f27` |
| 10. Boy-scout: stale line references | Implemented | `c03f4b9` |
| 11. Reconcile with #1021 and #1023 | Implemented | `origin/main` is still `7a22c07`: nothing to reconcile |
| 12. Verification | Implemented | every command of the plan's Verification section exited 0 at `c03f4b9` |
| 13. PR | Not yet due | after this audit |
| 14. `/super-review` | Not yet due | |
| 15. `/finish-pr` | Not yet due | |
| 16. Merge | Not yet due | Michaël only |
| Added: reference toctree | Added | `98b9fbc`, boy-scout: `mcp-server.md`, `cli-inspector.md` and `project-management.md` were in no toctree |

Counts: 13 Implemented, 0 Modified, 0 Dropped, 0 Incomplete, 1 Added, 4 not yet due. Divergence: 1 of 17 original
items (Modified + Dropped + Added), 5.9%, under the 30% threshold.

## Intent-preserving variations (not Modified)

- Item 4 planned `CallContext::own_home()` and `Home::own`; both landed in item 5, their first caller, under the plan's
  own rule that each method lands with its first caller so no commit carries dead code under `-D warnings`.
- Item 4's `call_record` returns the `NewEvent`, and `record(ctx, call, tool, detail, home)` appends it with
  `ingest_at`, as planned; `CallContext::bucket()` was added as the helper every bucket-filed caller passes.
- Item 1's unit tests were written in the same edit as the decoder rather than seen red first; items 2 to 8 were each
  seen red (compile failure or failing assertions) before their implementation.
- Item 6's `remember` refuses an unknown argument through `deny_unknown_fields`, so `lean_surface`'s unknown-argument
  probe for `remember` moved from `level` to `scope`, as planned.
