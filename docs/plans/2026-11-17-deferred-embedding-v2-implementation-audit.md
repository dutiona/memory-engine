# Implementation audit (pre-review): 2026-11-17-deferred-embedding-v2

- Scope: pre-review
- Approved plan: `docs/plans/2026-11-17-deferred-embedding-v2.md`, SHA-256
  `29441a37ea5919d78a66fd5bb3710bee52de2d0a4a7d143a010cfad9aaacbdde` (approved on #1083 by the owner,
  2026-11-17T20:31:28Z; D1 = B, option A deferred to #1084)
- Implementation head audited: `53c6c30`

| Item | Status | Evidence |
|---|---|---|
| 0 Worktree and plan | Implemented | locked worktree; plan issues #1079 (predecessor) and #1083; `26c8a49` |
| 1 Schema v16 | Implemented | `47a8f3e` |
| 2 Search-path guards | Implemented | `10c23bb` |
| 3 Maintenance-path guards | Implemented | `74fe5ef`; the watermark flag is `count_active > count_active_with_vector`, the apply-time check compares the same counts |
| 4 Embedder error classification | Implemented | `0ad8f05`; a connect error that is also a timeout is `Failed`, as the plan says |
| 5 Chunking and pooling | Implemented | `95e517a`; a one-piece text is sent as before and not re-normalised; `embed_batch` pools a splitting text directly (same requests); a 12th over-length test from the round-2 Resolution |
| 6 Recall's bounded query embed | Implemented | `25a396b`; one `warn` per fallback added |
| 7 Deferred insert | Implemented | `608c3f5` |
| 8 Drain steps and retry rules | Implemented | `9247789`; the identity is recorded only when the vector is written (a gone or already-embedded fact only loses its queue row, as the plan states); `NextEmbedding`/`PendingEmbedding` re-exported; an `ann`+`test-util` seam to trip `IndexInconsistent` |
| 9 `status` | Implemented | `126a73c`; `rusqlite::MAIN_DB` for the read-only check |
| 10 `remember` defers, the drain | Implemented | `60df04f`; the drain is held in a `JoinSet` and stopped (abort plus await) after the fence, which also aborts it if `serve` is dropped; #1022's request, levels and receipt kept |
| 11 Never wait for a hung embed | Implemented | `31d40f4` |
| 12 Documentation | Implemented | `216c9b8`; D1 recorded in `design-choices.md` with `Refs #1084` |
| 13 Boy-scout cli-inspector notes | Implemented | `ef195c6` |
| 14 #634 and the parity proof | Implemented | #634 comment 6964257979; `cargo test -p me-backend-postgres --features test-util -- --ignored`: 18 passed |
| 15 Rebase and reconcile | Implemented | rebased onto `9b9d676` (#1022); conflicts in `recall.rs` and two MCP test files resolved; #1023 has not merged yet (re-check before merge) |
| 16 PR | pending | next step |
| 17 `/super-review`, `/finish-pr` | pending | |
| 18 Merge | owner | |
| — `style(mcp)` import order | Added | `34376cc`: rustfmt fallout of the rebase resolution |

Counts: Implemented 16, Modified 0, Dropped 0, Incomplete 0, Added 1, pending 2 (16, 17), owner 1 (18).
Divergence (Modified + Dropped + Added) / 19 = 1 / 19 = 5 %, under 30 %: no successor.

Collateral boy-scout commits outside the plan's items, each its own commit: `2adc613` and `98b13c6` (the
proposer's broken rustdoc links), `4ccd8ef` (api.md's stale `add_fact` signature, duplicate Bootstrap section and
error table; the "MCP Server {Planned}" heading), `53c6c30` (the "Unforgettable Facts {Planned}" heading).
