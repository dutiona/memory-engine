# Implementation audit: 2026-11-17-lean-mcp-surface (pre-review)

- Approved plan: `docs/plans/2026-11-17-lean-mcp-surface.md`, SHA-256
  `63615f8813ae874586fba9ae0be975f7d517ea2e5e6a61c78d4feaa4677b4dc1`, approved on its plan issue #1066.
- Implementation head audited: `c88d91d` (`chore(build): drop the facade's unused libc dependency`).
- Scope: pre-review.

| Item | Status | Note |
|---|---|---|
| 1. The plan on the branch | Implemented | `d9a8858`; drafts deleted, never committed |
| 2. `ScopeQuery::ExactSet` | Modified | `0716434`. The HNSW variant of the filter-before-top-k test was dropped: it reproduces a pre-existing panic in the `ann` path (hnsw 0.11 `copy_from_slice` when widening asks for more neighbours than the graph walk reaches), outside this ticket and absent from the deployed image (default features). Recorded as an open collateral bug with its reproduction; the FTS, vector and hybrid channels are tested |
| 3. Read-only scope lookup | Implemented | `b1056d3` |
| 4. `StoreLock` | Implemented | `44e36b1` |
| 5. A read-only engine exports SQLite | Implemented | `8edaaf1` |
| 6. Every writable CLI command takes the lock | Implemented | `93c9e26`; the `--create` refusal is a `db.rs` unit test, the rest integration |
| 7. `prune` | Implemented | `2b3b766` |
| 8. The server holds the lock | Implemented | `8df8e7a` |
| 9. `recall` and `remember` | Modified | `bfbed99`. The query cap is 1 KiB, not the plan's 4 KiB: Michaël asked (approval relay) to take 1 KiB if it brings the crafted worst case well under 1 s. Measured on SQLite 3.46.1 over 20,000 rows: at 4 KiB, a 2,048-segment `a_a_…` phrase 0.995 s and 287 diacritic spellings of one word 1.907 s; at 1 KiB, 0.186 s and 0.167 s (a U+0345-joined phrase 0.290 s → 0.062 s). Intent unchanged |
| 10. Call records in the event log | Implemented | `6f28787` |
| 11. Natural-language full text | Implemented | `0f6bb1f`; under the 1 KiB cap |
| 12. Serve only `recall` and `remember` | Implemented | `02e2777`; the clippy guard was proven red on the four job handlers before they were deleted |
| 13. The docs agree | Implemented | `79d8039` |
| 14. Boy scout: unused `libc` | Implemented | `c88d91d` |
| 15. Ship | In progress | gates, PR, `/super-review`, `/finish-pr` |

Counts: Implemented 12, Modified 2, Dropped 0, Added 0, In progress 1 (operational). Divergence
(Modified + Dropped + Added) = 2 of 15 = 13%, under the 30% threshold. Neither modification changes approved intent:
item 9's cap value is Michaël's explicit choice, and item 2 loses one test variant to a pre-existing bug that is
surfaced, not hidden.
