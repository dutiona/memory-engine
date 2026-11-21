# Implementation audit (pre-review): 2026-11-21-hnsw-short-walk

- Scope: `pre-review`.
- Approved plan: `docs/plans/2026-11-21-hnsw-short-walk.md`, SHA-256
  `d3fe837fdd7e6ae9b6bd8461dd718571b4a8436aa0ef3dab79c85bb49410ae94`. It was approved on #1109 on 2026-11-21 with
  D1 = (b) and D2 = follow-up #1111.
- Audited head: `634917d2cc9f215c273ad48783b0cf80b43cceb2`.

| Item | Status | Evidence |
|---|---|---|
| 0. Worktree, baseline, plan issue | Implemented | The worktree is locked. Baseline: `me-backend-sqlite` 561 passed; the facade's eval, recall and store tests passed; `cargo deny check` ok (13 warnings, the same set afterwards). Plan issue #1109 |
| 1. Drop the facade's unused deps | Implemented | `a995400`. The all-features build, `check --features ann`, clippy and the recall test passed. The lock diff touches only the facade's dependency list |
| 2. Vendor hnsw 0.11.0 unmodified | Implemented, with variations | `ea28872`. The `.crate` sha256 matched the lock's checksum, and `src/` and `LICENSE` showed no diff. Still 18 members; `cargo tree` shows one `vendor/hnsw`. Locked build, all-features build, fmt, clippy, deny, fuzz build and image build all exit 0. Variations, all intent-preserving: (1) the lint remedy was needed (`dead_code`, `deprecated`); (2) the fmt remedy was not, because the unmodified source passes `cargo fmt --all --check`, which does check `vendor/` (probed with an injected misformat); (3) the manifest also drops `readme` (README not vendored); (4) the image tag is `memory-engine:verify-1074`, not `memory-engine:verify`, so it cannot collide with a parallel worker's build |
| 3. Reproduce both triggers, restore the #1020 variant (red) | Implemented | `7c1e0cf`. T1 fails at source 100 / destination 102. T2 fails at 33 / 36, so it panics on the first attempt (the duplicate cluster stops the walk at 33 = M0 + 1). E1 fails as `query failed: Pool("task join error: … (33) … (67)")`. The brute-force variant passes. Also adds a `seed_facts` test helper |
| 4. Copy only the filled prefix | Implemented | `39cf287`. The diff against upstream is exactly the two hunks plus their comments; the CRLF line endings are kept. E1 and T2 pass. T1 fails only at limit 120 with 2 batched queries. The other 562 tests pass |
| 5. `ef >= dest_len` | Implemented | `480ee8a`. 563 passed; the recall and eval tests pass with no threshold edited; clippy is clean |
| 6. Stale line references | Implemented | `7dd20b3`. `grep 'ann\.rs:[0-9]'` finds nothing |
| 7. Documentation | Implemented, with an addition | `634917d`. Sphinx `-W` exit 0, and `gate_matrix_parity` 7 passed. Added at the orchestrator's relay of the approval: design-choices.md names #1111 for the deferred port |
| 8. PR | Pending at audit time | Next step |
| 9. Review | Pending | — |
| 10. Merge | Michaël's | — |

Counts (items 0–7):

| Status | Count |
|---|---|
| Implemented | 8 (2 with intent-preserving variations) |
| Modified | 0 |
| Dropped | 0 |
| Added | 0 |
| Incomplete | 0 |

Items 8–10 are lifecycle steps, still to come. Divergence (Modified + Dropped + Added) is 0%, so no successor plan.

Full gate at the audited head: every AGENTS.md Verify command, both MSRV 1.89 builds, the image build
and both `--version` runs exit 0. `cargo test --workspace --all-features` reports 2273 passed, 0 failed and 109
ignored, with E1, T1 and T2 listed `ok`.
