# Plan Review — #622 (advisor-slot: adversarial migration/SQLite reviewer)

> **Note on provenance.** The `advisor()` built-in tool is **not available** in this environment.
> Per the super-plan skill's fallback guidance and the `feedback_review_under_budget` preference,
> the mandatory advisor slot was satisfied by an **independent clean-slate subagent** with an
> adversarial SQLite-migration/rusqlite-correctness lens (no shared context; read the plan + code
> from disk). This file records its verdict verbatim. The heavyweight external cross-model loop
> (Codex + agy) is reserved for the PR stage (T9 super-review).

## Verdict: NOT solid — ≥1 BLOCKER (corroborated the gap-review reviewer independently)

### [BLOCKER] Dump/restore (JSON import/export) silently loses the embedding identity

`src/inspect/dump.rs:75` serializes identity only because it is a `config` row (`list_config`);
`src/inspect/restore.rs:367-370` re-inserts it via the generic config-copy loop. Once identity
moves to the `embedding_spaces` table, dump no longer captures it and restore rebuilds an empty
table → dump→restore drops the identity (real data loss). Test `embedding_meta dim survives
restore` (`restore.rs:643-652`) will fail. `engine/restore.rs::restore_sqlite` (whole-file
`std::fs::copy`) is unaffected.

### [BLOCKER] Two test seeders stamp identity via `set_config("embedding_meta", …)` (bypass facade)

`engine/equivalence.rs:183-191` (dim-mismatch test) and `engine/cycle/apply.rs:508-516` (the
`engine()` helper feeding the apply-delta tests) write the raw config row; under the facade
`load`/`require_present` read the empty table → tests fail and the dim-mismatch guard is defeated
for any consumer that stamped via `set_config`. Switch seeders to `embedding_meta::store`.

### [MEDIUM] Convergence test as specified is whitespace-fragile

`sqlite_master.sql` is stored verbatim; the migration's frozen DDL literal and the fresh-init
literal will differ on whitespace/`IF NOT EXISTS`. Normalize via `deterministic_schema_dump`
(`mod.rs:1932`) or compare `pragma_table_info`/`pragma_index_info` (`mod.rs:2636`).

### [LOW] `ON CONFLICT(name) DO NOTHING` is dead-defensive (table created empty same step; version

gate makes it run-once) — harmless, simplified to a plain INSERT.

### Verified correct (no defect)

`(migrate_v12_to_v13, false)` lands at v13 (`target = i+2`, `mod.rs:245`); corrupt-JSON returns
`Err` → step rolls back → version stays 12; no read-only test pins `target: 12`
(`validate_schema_version` emits it dynamically; `error.rs` `target:11` literals are Display unit
tests); `store`→upsert preserves `store_overwrites`; the DDL-oracle list (golden + 2 insta snaps +
`all_nine_indexes_created` 27→28) is correct; `migration_chain_ddl_differs_from_init_known_artifact`
still passes. `src/storage/` #628 `SchemaManager` is trait-only (no impl) — compiles unchanged.

## Resolution

- [BLOCKER] dump/restore identity loss → **Addressed.** Added D9 + Files rows 10/11 + Task T5b:
  dump serializes `embedding_spaces`; restore re-inserts via `insert_active` and translates a legacy
  `config['embedding_meta']` from old dumps; round-trip test extended.
- [BLOCKER] `set_config` seeders → **Addressed.** Files row 14 + T5b: retarget `equivalence.rs` +
  `apply.rs` seeders to `embedding_meta::store`. Behavior-compat audit notes them.
- [MEDIUM] convergence whitespace-fragility → **Addressed.** D8 + Testing now mandate normalized
  comparison (`deterministic_schema_dump` / pragma), not raw `sqlite_master.sql`.
- [LOW] dead `ON CONFLICT` → **Addressed.** Migration sketch simplified to a plain INSERT with a
  comment on why no conflict is possible.
- Verified-correct items → no change required; folded the confirmations into D6/D8/Testing so the
  implementer doesn't re-investigate.
