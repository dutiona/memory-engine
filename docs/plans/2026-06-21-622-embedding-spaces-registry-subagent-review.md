# Plan Review — #622 (clean-slate subagent, gap/risk/feasibility lens)

Independent general-purpose subagent, no shared context; read the plan + `src/store/*`,
`src/inspect/*`, CLI/MCP from disk.

## Verdict: NOT LGTM — two BLOCKERs (load-bearing blind spot: identity leaving the `config` table)

### [BLOCKER] B1 — Dump/restore loses the embedding identity

`inspect/dump.rs:75-77` dumps `list_config` + enumerated tables only (not `embedding_spaces`);
`inspect/restore.rs:367-370` restores identity via config-copy; test `restore.rs:643-652` asserts
`meta.dim == Some(4)` survives restore. Plan's Files/Tasks omit both files. Fix: dump+restore the
table (or reconstruct from a dumped fingerprint).

### [BLOCKER] B2 — CLI + MCP read `config['embedding_meta']` via raw SQL; migration deletes the key

`memory-engine-cli/src/db.rs:35-51` `peek_embed_dim_from_db` and `memory-engine-mcp/src/config.rs:136-152`
`probe_embed_dim` run raw `SELECT value FROM config WHERE key='embedding_meta'` — not facade calls.
After the migration deletes the key → `None` → "no recorded dimension" runtime error on a migrated
store. The "no consumer-crate change" claim (plan lines 54/197/285) is false; strike it and repoint
both at the facade/table with migration-aware tests.

### [HIGH] H1 — Freeze the migration's fingerprint decode into a migration-local struct

The sketch couples to live `crate::types::EmbeddingFingerprint`; a future serde-shape change would
silently alter this v12-era migration's meaning. Deserialize into a local struct (the v11→v12
migration deliberately avoided parsing the live type).

### [HIGH] H2 — Index-count test is `all_nine_indexes_created` (`mod.rs:1157`, asserts 27 at `:1168`)

with a hand-maintained tally comment at `:1167` that must also change; 27→28 is correct (the partial
unique index matches `idx_%`).

### [HIGH] H3 — Enumerate `config_no_default_embedding_meta` (`mod.rs:1087`) and

`migrate_v11_to_v12_drops_embed_dim` (`mod.rs:1100`) in the behavior-compat audit — they call
`embedding_meta::load` and assert `None`; should pass unchanged but must be named.

### [MEDIUM] M2 — Read-only path: no existing test pins `target: 12` (the audit comes up empty —

`validate_schema_version` is dynamic). Flagging was right; the new `read_only_open_pre_v13_errors`
test is correctly specified.

### Over-engineering / structural completeness / feasibility — LGTM

Scope discipline strong (D3 `pub(crate)` types, D7 no stubs, D4 deferred typed variant). Doc/Testing/
Verification sections present and concrete. Migration-fidelity-first sequencing sound and achievable.
Module registration is `src/store/mod.rs:10` (confirmed). Fresh-init DDL in `init_schema:108-112`.

## Resolution

- [BLOCKER] B1 dump/restore → **Addressed.** D9 + Files 10/11 + T5b; round-trip test extended.
- [BLOCKER] B2 CLI/MCP raw reads → **Addressed.** D9 + Files 12/13 + T5b; the false "no consumer
  change" claim struck in BLUF/Files/Documentation; migration-aware probe tests added.
- [HIGH] H1 migration-local struct → **Addressed.** Migration sketch decodes into `V12Fingerprint`.
- [HIGH] H2 test name/count → **Addressed.** D8 + Testing name `all_nine_indexes_created`, `:1167-1168`,
  27→28.
- [HIGH] H3 audit enumeration → **Addressed.** Testing now names both schema-level cases.
- [MEDIUM] M2 read-only audit → **Addressed.** Testing records the audit came up empty (dynamic
  `target`), keeps the new-case assertions.
