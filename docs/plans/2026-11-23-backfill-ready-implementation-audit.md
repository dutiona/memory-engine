# Implementation audit (pre-review): 2026-11-23-backfill-ready

- Scope: pre-review
- Approved plan: `docs/plans/2026-11-23-backfill-ready.md`, SHA-256 `578afdc576bb0c1cb6f2b730141f58d639de6cf1f44bd34ad93b16173c157aec`,
  approved on #1157 (comment 7112682409)
- Implementation head audited: `44272ed51511df7494cfcaa7e3c7925fe8aed37f` (the commits below were rewritten once by an
  autosquash folding three lint fixes into their own member commits; the hashes cited are the pre-squash ones)

## Items

| # | Item | Status | Evidence |
|---|---|---|---|
| 1 | #1153: one preview from dry run to write | Implemented | 5794246. Engine red at the helper's `assert!(write.committed)`; route red: the `transcript-future` seed's replay answered 412 with `t_valid` hashed, passed without it; `backfill_import_test` 19/19 |
| 2 | #1154: one interrupted-call rule for both readers | Implemented | edb8dfa. Red on shapes (b), (c), (d), (f) exactly as planned; green: `adapters::claude_code` 31/31, `bootstrap::` 89/89, doctest 1/1 |
| 3 | #1092: api.md signatures checked by a parity test | Implemented | cdd2a93. Red listed 32 async-only rows plus execute_query, consolidate, three restore parameters, `StoreLock::acquire`, the untyped bootstrap rows and the builder rows; green 6/6 |
| 4 | Boy-scout doc fixes | Implemented | d6b577f (README link), b962b32 (MemoryQuery fields) |
| 5 | #1093: committed, checked fuzz lock | Implemented | e2d1b6e. Red: `fetch --locked` rc 101 with no lock; `gate_matrix_parity` red with one doc missing the row; green: fetch rc 0, fuzz build and the bounded run pass, lock untouched |
| 6 | PR, review, finish | Pending | this phase: PR, `/super-review` Full, `/finish-pr` |

Counts: Implemented 5, Modified 0, Dropped 0, Incomplete 0, Added 0, Pending 1 (the lifecycle item). Divergence 0%.

## Intent-preserving variations (not Modified)

- Item 1: `transcript-future` was generated with the seed script's own encoding (checked byte-identical on
  `transcript-main`), without regenerating the other seeds, which needs the plugin-export binary; the script in the
  seed README carries the new entry.
- Item 3: two guards beyond the plan's three (the page parser on a small table; a rustfmt-wrapped signature), added
  after the first red run showed the page parser was untested; the normaliser also drops a trailing comma, as the
  plan's earlier text form did. The page rewrite was scripted from the test's own drift report.
- Item 4: the MemoryQuery list also missed `include_expired_probe`; both fields went into the one commit.
- Item 5: each gate matrix's prose paragraph gained one sentence on the new row (Documentation).
