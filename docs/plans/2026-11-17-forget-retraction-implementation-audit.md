# Implementation audit (pre-review): forget as a reversible retraction (#1023)

Scope: `pre-review`.
Approved plan: `docs/plans/2026-11-17-forget-retraction.md`, SHA-256
`756ffce9c47c3256a32cf8403d72323667c1e773f0661028eea794b233ff52cc` (approved on plan issue #1087).
Implementation head audited: `7775ebb` (17 commits on `origin/main` 9b9d676, which carries #1022).

## Items

| # | Item | Status | Commit | Note |
|---|---|---|---|---|
| 0 | Worktree | Implemented | — | Rebased onto main once #1022 merged (9b9d676), before the first commit. |
| 1 | `hard_delete_ids` one JSON array | Implemented | 2fc26bf | |
| 2 | Drop uncalled port methods | Implemented | 028c1a7 | Also drops `EdgeStore::hard_delete_by_facts`, whose only caller was the dropped `hard_delete_edges_by_facts` (same intent: no hard delete that skips erase's cleanup). |
| 3 | Archival unlinks a promoting activity | Implemented | 38eae0e | |
| 4 | Schema v16 | Implemented | 0c64545 | `all_nine_indexes_created` = 33; the v15→v16 policy paragraph lands here, with the schema. |
| 5 | `Retraction` port: forget, unforget, tombstone, `is_forgotten` | Implemented | f7007f7 | Variation: `forget_fact` takes the call record and appends it **in its own transaction** (`record_at`, `record: &NewEvent`, returns `ScopedWrite`), following #1022's `move_fact_atomic`, instead of a record id written beforehand. D7's intent (record first, tombstones name it, a failed record changes nothing) holds more strongly. The `unforget` record lists `fact_ids` (one forget can restore several). |
| 6 | Facade `forget_fact`, `unforget_fact`, `explain` | Implemented | be4e24d | Signature follows item 5: `forget_fact(id, homes, reason, record_at, record) -> (ForgetOutcome, i64)`. |
| 7 | Import and batch gate | Implemented | e9ecd83 | `batch_ingest_skips_a_forgotten_line_and_lands_its_chunk` is an in-process CLI unit test (`ingest_from_reader`), not a binary test: `batch-ingest` needs an HTTP embedder, and the unit seam is the one the existing tests use. |
| 8 | Promotion gate | Implemented | c9becda | |
| 9 | Archival and export | Implemented | 1db4b42 | |
| 10 | Erase in one transaction | Implemented | 9606c9e | Variation: the transaction body is `crates/me-backend-sqlite/src/store/erase.rs` (SQL over a connection, like the other `store/` modules); `sqlite/retraction.rs` wraps it with the purge. That is the function #1025/#1026/#1029 extend. `EraseReport` is not `#[non_exhaustive]`: the backend crate constructs it, and siblings extend `deleted`, not fields. |
| 11 | Facade `erase` | Implemented | 2cc87bd | Variation: the HNSW sidecar is removed by the backend (it owns the path), before the transaction. |
| 12 | `forget` model tool | Implemented | 386a658 | Variations from #1022 having landed first, as the sibling notes foresaw: the Homes are `[ctx.own_home(), Home::User]` (no separate `own_homes`); the record detail key is `fact_ids` like `set_level`'s and `recall`'s. **Owner point carried:** per approved D8, `not found` is a success result, while `set_level` (#1022) answers it as `error: not found`; the two tools now differ. |
| 13 | CLI `unforget` | Implemented | 6223a2d | `explain` already prints the state with `{:?}`, which now carries the forget's time, reason and record: no explain code change needed. |
| 14 | CLI `erase` | Implemented | 87258e2 | |
| 15 | Documentation | Implemented | 79a613c | `schema-evolution-policy.md` and `crate-layout.md` landed with items 4 and 5. |
| 16 | ADR-0021 §3 amendment | Implemented | c64abe1 | |
| 17 | Stale CLI reference | Implemented | 7775ebb | Also removes the "#80" export/import limitation (#80 is closed and unrelated; the round trip is tested). |
| 18 | Operational steps | Incomplete (pre-review) | — | Parity note on #634, PR, `/super-review`, `/finish-pr` follow this audit; the prune rename is filed as #1089 by the orchestrator. |

Counts: 18 Implemented, 0 Modified, 0 Dropped, 0 Added, 1 Incomplete (the operational item, by sequence).
Divergence (Modified + Dropped + Added) / 19 = **0%**. The variations above preserve the approved intent; none
changes a decision.
