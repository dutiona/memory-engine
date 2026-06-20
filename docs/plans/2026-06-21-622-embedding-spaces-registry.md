# Plan — #622: multi-embedding-space registry (schema + API only)

**Issue:** #622 `feat(storage): multi-embedding space registry (adopt KB embed_spaces schema + status enum)`
**Type/Area:** `type:feature` / `area:storage` · **Epic:** #610 (TEI/Qwen embedding migration), Wave 2
**Schema:** v12 → v13 · **Worktree:** `.worktrees/feat-622-embedding-spaces` (branch `feat-622-embedding-spaces`)
**Engagement:** Thorough · **Severity mode:** Default (address all)

---

## BLUF

Generalize the single-value `store::embedding_meta` identity (one JSON `config` row) into a
first-class **`embedding_spaces` table** modeling the Knowledge layer's `embed_spaces` registry
shape: a `name` PK, the five `EmbeddingFingerprint` columns, a `status` enum
(`active`/`populating`/`deprecated`), and a **partial unique index** enforcing
**exactly-one-active**. A **lossless v12→v13 migration** folds a present `embedding_meta` value
into one `active` row named `default` and retires the legacy key.

The structural lever: introduce a new **`store::embedding_spaces`** module that owns the table,
and turn **`store::embedding_meta`** into a thin **single-active facade** over it. The facade's
five public functions keep byte-identical signatures and semantics, so the **facade call sites**
and the public type surface (`EmbeddingFingerprint`) do not change. The single `active` row is the
degenerate case; nothing in this PR writes a second live space.

> **Identity relocation has a blast radius beyond the facade (D9).** Today the identity lives in
> the `config` table, and several consumers read/write it via **raw SQL outside the facade**
> (dump/restore, the CLI/MCP dim probes, two test seeders). Moving it into `embedding_spaces`
> **requires** updating those too — otherwise dump/restore silently loses the identity and the
> CLI/MCP probes break at runtime. This is in-scope: it completes the identity's relocation across
> _all_ its access paths (not new multi-space behavior). See **D9**.

This delivers the **foundation** that #623 (reconstruction), #624 (HNSW rebuild), and #689
(end-to-end coexistence / promote-rollback) extend as pure additions — new rows + new methods on
`embedding_spaces`, never edits to the facade or its callers.

---

## Scope (LOCKED — user decision)

**IN**

- New `embedding_spaces` table (in the fresh-init DDL **and** a frozen snapshot inside the
  migration; convergence guarded by a test).
- `name TEXT PRIMARY KEY` (degenerate value `'default'`), the five identity-tuple columns, a
  `status` TEXT CHECK enum, `created_at`, and the partial unique index for single-active.
- New `store::embedding_spaces` module owning the table + row CRUD; `store::embedding_meta`
  becomes a behavior-compatible facade delegating to it.
- `migrate_v12_to_v13` (config value → one `active` row; drop legacy key; fresh-store no-op;
  corrupt value = hard error).
- `CURRENT_SCHEMA_VERSION` 12 → 13; append `(migrate_v12_to_v13, false)` to `MIGRATIONS`.

**OUT — separate issues; do NOT pull forward (but DO leave clean seams)**

- Background reconstruction / backfill — **#623**.
- HNSW rebuild + similarity-edge invalidation — **#624**.
- Live-write concurrency safety for a second space — **#625**.
- End-to-end multi-space coexistence, query-across-spaces, promote/rollback UX, and **promoting
  `SpaceStatus`/`EmbeddingSpace` to public `types.rs` types** — **#689**.

The table MAY _model_ multiple rows/statuses, but no code path in this PR writes a non-`active`
row (except a test). No `EngineConfig`/MCP/CLI surface change.

---

## Synthesis rationale (three-lens judge panel)

Drafts at `docs/plans/2026-06-21-622-embedding-spaces-registry-drafts/` (mvp-first, risk-first,
architecture-first). Cherry-picked:

- **Sequencing + test ledger ← risk-first.** Migration fidelity is the one silent-corruption risk
  (`dim` drives vector deserialization), so it is validated by a round-trip test _before_ the API
  reads the table. Plus its read-cardinality contract (zero-active → `None`, >1 → hard error) and
  the read-only-path proof.
- **Module boundary ← architecture-first.** `embedding_spaces` (table owner) + `embedding_meta`
  (single-active facade) is the firewall keeping #623/#624/#689 additive.
- **`name` PK + 5 typed columns ← risk-first + architecture-first.** The forward-compatible row
  identity and queryability that make #622 a _foundation_, not just a status column. Without
  `name`, #623 can't address a second row without another migration — defeating the issue's point.
- **DDL-oracle discipline ← mvp-first.** A real table breaks the insta snapshots, the in-source
  golden `schema_ddl_snapshot_is_stable`, and the `all_*_indexes_created` count (27→28). Update
  content; defer the `v11`→`v13` snapshot _rename_ to a separate chore.
- **Scope pullback (mine).** `SpaceStatus`/`EmbeddingSpace` stay `pub(crate)` store-internal, NOT
  public `types.rs` types — the locked scope is API-behavior-compatible. `types.rs` is untouched;
  consumer crates recompile-only. #689 promotes them when it exposes a multi-space API.

---

## Design decisions

### D1 — Table shape (5 identity columns + `name` PK + `status` + `created_at`)

```sql
CREATE TABLE IF NOT EXISTS embedding_spaces (
    name                TEXT    PRIMARY KEY,                         -- 'default' for the single space
    model               TEXT    NOT NULL,
    provider            TEXT    NOT NULL,
    dim                 INTEGER NOT NULL,
    matryoshka_base_dim INTEGER,                                     -- NULL = untruncated (Option<usize>)
    element_type        TEXT    NOT NULL DEFAULT 'float32',
    status              TEXT    NOT NULL DEFAULT 'active'
                        CHECK(status IN ('active', 'populating', 'deprecated')),
    created_at          TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Exactly-one-active (mirrors KB idx_embed_spaces_one_active). Unbypassable by any writer.
CREATE UNIQUE INDEX IF NOT EXISTS idx_embedding_spaces_one_active
    ON embedding_spaces(status) WHERE status = 'active';
```

KB-internal columns `table_name`/`chunk_count`/`chunk_strategy` are **omitted** — ME does not
chunk and keeps vectors inline in `facts.embedding` (no per-space backing table this wave). Adding
a column later is a cheap `ALTER TABLE ADD COLUMN`; shipping a misleading one is not. `usize↔i64`
mapping uses `i64::try_from`/`usize::try_from` (never `as` — keeps clippy-nursery + `forbid(unsafe)`
clean), erroring on the impossible overflow rather than truncating.

### D2 — Module boundary: `embedding_spaces` owner + `embedding_meta` facade

New `src/store/embedding_spaces.rs` owns: the `TABLE_DDL`, the `SpaceStatus` ⇄ TEXT mapping, an
`EmbeddingSpace` row struct (`pub(crate)`), and the row CRUD. `embedding_meta`'s five public
functions delegate, keeping exact signatures, error variants, and semantics. The single-active
_policy_ (write-once, #614 mismatch, dim guard) stays in the facade; the table module just
reads/writes rows.

### D3 — Domain types stay store-internal (`pub(crate)`)

```rust
// src/store/embedding_spaces.rs — NOT types.rs (no public-API expansion this wave)
pub(crate) enum SpaceStatus { Active, Populating, Deprecated }   // as_sql()/from_sql() owned here
pub(crate) struct EmbeddingSpace {                               // wraps the fingerprint by composition
    pub name: String,
    pub fingerprint: EmbeddingFingerprint,
    pub status: SpaceStatus,
}
impl EmbeddingSpace { pub(crate) const DEFAULT_NAME: &str = "default"; }
```

`from_sql` rejects an out-of-CHECK status as `MigrationError::Incompatible` (never a silent
default — consistent with `load`'s existing corrupt-JSON contract). Composition (wrapping, not
flattening, `EmbeddingFingerprint`) preserves the #614 full-tuple `Eq` contract and the normative
serde key-set test on `EmbeddingFingerprint`.

### D4 — Single-active = DB partial unique index; violation → diagnosable `Internal`

Structural enforcement (unbypassable by future #623/#689 writers) beats an app-level
`COUNT(*)` guard (TOCTOU-racy once #625's concurrency lands). A unique-violation is remapped at
the `insert_active` seam to `MemoryError::Internal("…single-active invariant violated…")` so a
future promote bug is _diagnosable_ rather than a bare `Database` error. A dedicated typed
`EmbeddingSpaceConflict` variant is **deferred to #689** (per #560 doctrine: don't split a variant
no caller matches on; this wave has no caller that can recover differently).

### D5 — Migration is lossless (map, not drop)

Unlike v11→v12 (which _dropped_ `embed_dim` for lack of a full tuple), the v12 `embedding_meta`
value is a complete, correct `EmbeddingFingerprint`, so v12→v13 parses it (via the **same serde
path** `load` used — so a corrupt value fails identically, never becomes a fabricated row) and
inserts it as the single `active`/`default` row, then deletes the legacy key. Fresh/never-embedded
store → empty table, identity still established lazily on first write (preserves the #613 lazy-stamp
invariant). Corrupt value → migration returns `Err` → framework rolls the step back → version
stays 12. Idempotent (IF NOT EXISTS + version gate). Runs inside the framework's per-step
transaction; the existing `VACUUM INTO` pre-migration backup covers it — no new backup machinery.

### D6 — Read contract & read-only path

`find_active` = `SELECT … WHERE status='active'`: zero rows → `Ok(None)` (a fully-deprecated store
reads like fresh for the single-active API — the only coherent degenerate reading); >1 row →
`MemoryError::Internal` (impossible by the index, but fail loud, never pick arbitrarily). `load`
must be a **pure SELECT** (no side-effecting `CREATE TABLE`) so it works under
`SQLITE_OPEN_READ_ONLY`. The RW open runs `migrate`; the read-only open runs
`validate_schema_version` only — so a read-only open of a pre-v13 DB correctly errors
`SchemaVersionNeedsMigration { found: 12, target: 13 }`. **Audit any read-only test that pins
`target: 12`** and update to 13.

### D7 — Seams for #623/#624/#689: expose read surface, document mutators

Expose now (facade-needed or trivially useful, write the row-mapping once): `find_active`,
`list_spaces`, `insert_active`, `upsert_active_fingerprint`. **Do NOT stub**
`insert_populating`/`promote`/`deprecate` — dead `unimplemented!()` lies about capability; specify
them as prose in the module docstring's "Future seams" section so #623/#689 add them with their
tests. (YAGNI-correct: enough structure to be additive, no speculative dead code.)

### D8 — DDL-oracle bookkeeping (a real table is added)

Update the in-source golden in `schema_ddl_snapshot_is_stable`; re-accept the two insta snapshots
(`schema_v11`, `schema_v11_migration_chain`) after confirming the diff is _only_ the new table +
index; bump the index-count assertion in **`all_nine_indexes_created` (`schema/mod.rs:1168`, asserts
`27`)** to `28` **and update its hand-maintained tally comment (`:1167`)**. The new partial unique
index matches `name LIKE 'idx_%'`, so 28 is correct. **Keep the `v11` snapshot names** (frozen
historical labels) — the rename to `v13` is a cosmetic follow-up filed as a separate `chore`, not
folded in. The **fresh-vs-migrated convergence test must normalize** (reuse
`deterministic_schema_dump` — `split_whitespace().join(" ")`, `mod.rs:1932` — or compare via
`pragma_table_info`/`pragma_index_info` like `fresh_and_migrated_dedup_index_have_identical_columns`,
`mod.rs:2636`), **NOT** a raw `SELECT sql FROM sqlite_master` string-compare: `sqlite_master.sql` is
stored verbatim, so the two hand-copied DDL literals would false-red on any whitespace/`IF NOT
EXISTS` difference.

### D9 — Identity-relocation blast radius (raw config readers/writers outside the facade)

Both plan reviewers independently flagged this as the plan's one real blind spot: moving the
identity out of the `config` table breaks every consumer that touches `config['embedding_meta']`
**without** going through the facade. All of these are **IN scope** for #622 (they complete the
relocation; none add multi-space behavior):

| Path              | Site                                                      | Today                                                                                    | Fix                                                                                                                                                                                                                                |
| ----------------- | --------------------------------------------------------- | ---------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Dump**          | `src/inspect/dump.rs:75`                                  | serializes only enumerated tables + the `config` map → never captures `embedding_spaces` | dump the `embedding_spaces` row(s) as a new section                                                                                                                                                                                |
| **Restore**       | `src/inspect/restore.rs:367` (+ test `:643`)              | restores identity via the generic config-copy loop                                       | re-insert dumped `embedding_spaces` rows via `insert_active`; **and** translate a legacy `config['embedding_meta']` value from an OLD (pre-#622) dump → `insert_active` (back-compat for old exports). Extend the round-trip test. |
| **CLI dim probe** | `memory-engine-cli/src/db.rs:35` `peek_embed_dim_from_db` | raw `SELECT value FROM config WHERE key='embedding_meta'`                                | read `dim` from the `embedding_spaces` active row, with a legacy `config['embedding_meta']` JSON fallback for a not-yet-migrated v12 DB                                                                                            |
| **MCP dim probe** | `memory-engine-mcp/src/config.rs:136` `probe_embed_dim`   | identical raw read                                                                       | same as the CLI probe                                                                                                                                                                                                              |
| **Test seeders**  | `engine/equivalence.rs:183`, `engine/cycle/apply.rs:508`  | stamp identity via `set_config("embedding_meta", …)` (bypasses the facade)               | switch to `embedding_meta::store(conn, &fp)` so the facade-routed write lands in the table                                                                                                                                         |

`engine/restore.rs::restore_sqlite` is a whole-file `std::fs::copy` — the table travels in the
bytes, unaffected. The `src/storage/` #628 `SchemaManager` trait is **trait-only (no impl)** — it
compiles unchanged; the T10 rebase caveat applies if a #628 backend lands first.

---

## Files to change

| #   | File                                                       | Change                                                                                                                                                                                                                                                                                                                                                    |
| --- | ---------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | `src/store/embedding_spaces.rs` (NEW)                      | `TABLE_DDL` const, `SpaceStatus` (+`as_sql`/`from_sql`), `EmbeddingSpace`, `find_active`/`list_spaces`/`insert_active`/`upsert_active_fingerprint`, `map_single_active_violation`, module docstring + Future-seams section, unit tests.                                                                                                                   |
| 2   | `src/store/embedding_meta.rs`                              | Re-implement the five functions as a facade delegating to `embedding_spaces`. `ensure_match` unchanged. Demote `EMBEDDING_META_KEY` to `pub(crate)` (grep-confirmed no external user; the migration reads the literal). Update module docstring (future→done). Keep behavior-compat tests verbatim; retarget `load_errors_on_corrupt_json` (see Testing). |
| 3   | `src/store/schema/migrations.rs`                           | Add `migrate_v12_to_v13` (frozen DDL snapshot + lossless map + legacy-key delete).                                                                                                                                                                                                                                                                        |
| 4   | `src/store/schema/mod.rs`                                  | `CURRENT_SCHEMA_VERSION` 12→13; append `(migrations::migrate_v12_to_v13, false)` to `MIGRATIONS`; add `embedding_spaces` to fresh-init DDL; declare `pub mod embedding_spaces` (or under `store/mod.rs`); update golden + index count; add migration/convergence tests.                                                                                   |
| 5   | `src/store/mod.rs`                                         | `pub(crate) mod embedding_spaces;` (module registration — confirm location vs `schema`).                                                                                                                                                                                                                                                                  |
| 6   | `docs/design/embedding-identity-and-tei-qwen-migration.md` | §1/§Out-of-scope: note the registry schema + single-active API (#622) landed; #623/#624/#625/#689 remain.                                                                                                                                                                                                                                                 |
| 7   | `docs/design/schema-evolution-policy.md`                   | Add the v12→v13 entry (DDL migration; lossless map; single-active invariant; fresh-vs-migrated convergence).                                                                                                                                                                                                                                              |
| 8   | `docs/reference/crate-layout.md`                           | Register `store::embedding_spaces` in the module map.                                                                                                                                                                                                                                                                                                     |
| 9   | snapshots (`*.snap`)                                       | Re-accept after review (only the new table + index).                                                                                                                                                                                                                                                                                                      |
| 10  | `src/inspect/dump.rs`                                      | **(D9 BLOCKER)** Serialize the `embedding_spaces` row(s) — dump no longer captures identity via the `config` map.                                                                                                                                                                                                                                         |
| 11  | `src/inspect/restore.rs`                                   | **(D9 BLOCKER)** Re-insert dumped `embedding_spaces` rows via `insert_active`; translate a legacy `config['embedding_meta']` value from an OLD dump → `insert_active` (back-compat). Update the round-trip test `:643`.                                                                                                                                   |
| 12  | `memory-engine-cli/src/db.rs`                              | **(D9 BLOCKER)** Repoint `peek_embed_dim_from_db` at the `embedding_spaces` active row + legacy `config` fallback; migration-aware test.                                                                                                                                                                                                                  |
| 13  | `memory-engine-mcp/src/config.rs`                          | **(D9 BLOCKER)** Repoint `probe_embed_dim` identically; migration-aware test.                                                                                                                                                                                                                                                                             |
| 14  | `src/engine/equivalence.rs`, `src/engine/cycle/apply.rs`   | **(D9 BLOCKER)** Switch the `set_config("embedding_meta", …)` test seeders to `embedding_meta::store(conn, &fp)` so the facade-routed write lands in the table.                                                                                                                                                                                           |

No change to `error.rs`, `types.rs`, `traits.rs`, or `lib.rs`. The CLI and MCP crates **do** change
(D9 — their raw dim probes read the relocated identity); the embed crate is recompile-only.

---

## Migration & facade sketches

`migrate_v12_to_v13` (frozen DDL snapshot — never reference the live `TABLE_DDL` const):

```rust
// H1: decode into a MIGRATION-LOCAL struct, NOT crate::types::EmbeddingFingerprint, so a future
// change to the live type's serde shape cannot silently alter what this v12-era migration means
// (honors the frozen-snapshot doctrine in migrations.rs).
#[derive(serde::Deserialize)]
struct V12Fingerprint {
    model: String,
    provider: String,
    dim: u64,
    #[serde(default)]
    matryoshka_base_dim: Option<u64>,
    element_type: String,
}

pub(super) fn migrate_v12_to_v13(conn: &Connection) -> Result<()> {
    conn.execute_batch("/* frozen copy of embedding_spaces DDL + partial index (D1) */")?;
    if let Some(raw) = get_config(conn, "embedding_meta")? {
        // Same serde decode path load() used → a corrupt value fails identically (hard error,
        // never a fabricated row). The table was just created empty in this step, so a plain
        // INSERT cannot conflict (the version gate makes the step run-once) — no ON CONFLICT.
        let fp: V12Fingerprint = serde_json::from_str(&raw).map_err(|e| {
            MigrationError::Incompatible(format!("corrupt embedding_meta during v12->v13: {e}"))
        })?;
        conn.execute(
            "INSERT INTO embedding_spaces
                 (name, model, provider, dim, matryoshka_base_dim, element_type, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active')",
            rusqlite::params![
                "default",
                fp.model,
                fp.provider,
                i64::try_from(fp.dim).map_err(|_| MigrationError::Incompatible("dim exceeds i64".into()))?,
                fp.matryoshka_base_dim
                    .map(|d| i64::try_from(d).map_err(|_| MigrationError::Incompatible("mrl exceeds i64".into())))
                    .transpose()?,
                fp.element_type,
            ],
        )?;
        conn.execute_batch("DELETE FROM config WHERE key = 'embedding_meta';")?;
    }
    Ok(())
}
```

Facade (signatures byte-identical; bodies delegate):

```rust
pub fn load(conn) -> Result<Option<EmbeddingFingerprint>>
    = embedding_spaces::find_active(conn)?.map(|s| s.fingerprint)
pub fn store(conn, fp) = embedding_spaces::upsert_active_fingerprint(conn, fp)   // upsert 'default' active row
pub fn record_if_absent(conn, candidate, expected_dim) -> Result<EmbeddingFingerprint>
    // if find_active → ensure_match (#614) + return stored; else dim-guard + insert_active(default/active)
pub fn check_compatible(conn, candidate)  // find_active.map_or(Ok(()), ensure_match)
pub fn require_present(conn)              // find_active.is_none() → Internal (unchanged message)
// ensure_match — UNCHANGED (#614 full-tuple Eq)
```

---

## Documentation

Not N/A. Touched: `embedding_spaces.rs` module docstring (role, degenerate-case framing, "Future
seams (#623/#624/#689)"); `embedding_meta.rs` docstring (future→done — now a facade); the design
doc (#622 landed); `schema-evolution-policy.md` (v12→v13 entry); `crate-layout.md` (new module).
No Sphinx narrative page — no public API/type change (the CLI/MCP changes in D9 are internal raw-SQL
read repoints, not user-facing surface). ADR 0015 shared text untouched (status/table_name already
named layer-internal there); the landing note lives only in the ME-local design doc.

## Testing

TDD, risk-ordered (write failing test → implement). The pre-existing `embedding_meta` unit tests
are the **behavior-compat contract** — they pass **unchanged** through the facade.

- **Migration fidelity (retire FIRST):** `migrate_v12_to_v13_roundtrips_fingerprint` (seed v12 by
  `set_config("embedding_meta", json)` + roll version to 12, then `migrate`; assert version 13,
  one `default`/`active` row, reconstructed tuple `== fp` incl. MRL `Some` + non-default
  `element_type`, legacy key gone, then re-run → no-op); `_fresh_no_rows`;
  `_corrupt_json_rolls_back` (version stays 12); `_idempotent`.
- **Behavior-compat:** all existing `embedding_meta::tests` unchanged. **Retarget**
  `load_errors_on_corrupt_json` → the corrupt-config path no longer exists (value is columns); move
  that assertion into the migration tests, and replace it with
  `load_none_when_only_deprecated_row` + a single-active-invariant assertion. **Add**
  `store_then_load_preserves_matryoshka_and_element_type`.
- **Registry mechanics (`embedding_spaces::tests`):** `find_active_none_on_fresh`;
  `insert_then_find_active_roundtrip` (`Some` + `None` base-dim); `list_spaces_returns_active`;
  `space_status_sql_roundtrip` + `from_sql("bogus")` is `Err`;
  `single_active_index_rejects_second_active` (direct 2nd active insert → mapped `Internal`, first
  row intact).
- **Behavior-compat audit (H3 — enumerate explicitly, must pass unchanged):** the schema-level
  cases that call `embedding_meta::load` and assert `None` on fresh/migrated stores —
  `config_no_default_embedding_meta` (`mod.rs:1087`) and `migrate_v11_to_v12_drops_embed_dim`
  (`mod.rs:1100`) — confirm green against the table-backed facade.
- **Convergence (normalized — NOT raw `sqlite_master.sql`):**
  `fresh_vs_migrated_embedding_spaces_converge` compares the table + index via
  `deterministic_schema_dump` normalization or `pragma_table_info`/`pragma_index_info` (D8), so
  whitespace between the two hand-copied DDL literals can't false-red; `fresh_db_has_embedding_spaces_table`.
- **Read-only:** `read_only_open_reads_registry`; `read_only_open_pre_v13_errors` (asserts
  needs-migration, no migrate on read-only). (Audit confirmed: no existing test pins `target: 12` —
  `validate_schema_version` emits `target` dynamically; the `error.rs` `target: 11` literals are
  Display unit tests independent of `CURRENT_SCHEMA_VERSION`. Nothing to change, but assert the new
  cases.)
- **Chain + bookkeeping:** extend the full-chain migration test to v13; update
  `all_nine_indexes_created` 27→28 + its tally comment (`mod.rs:1167-1168`); confirm
  `migration_chain_ddl_differs_from_init_known_artifact` still passes (the new table is
  `CREATE … IF NOT EXISTS` on both paths → identical → does not perturb the pre-existing
  v3-ALTER divergence); update the in-source golden + re-accept insta snapshots.
- **D9 identity relocation:** extend the dump/restore round-trip test (`inspect/restore.rs:643`) so
  the identity survives a dump→restore through `embedding_spaces` (new-format dump) **and** a
  legacy `config['embedding_meta']` old-format dump translates to one active row; migration-aware
  tests for the CLI/MCP dim probes (`peek_embed_dim_from_db` / `probe_embed_dim` read dim from a
  migrated v13 DB); confirm the retargeted `equivalence.rs`/`apply.rs` seeders (now via
  `embedding_meta::store`) keep their tests green.
- **Cross-crate:** the workspace gate (`cargo test --workspace`) runs the CLI/MCP probe + dump/restore
  tests — runtime SQL reads compile fine but fail at execution if D9 is incomplete, so the gate is
  the backstop, not signature drift.

## Verification

```bash
cd .worktrees/feat-622-embedding-spaces
cargo build --workspace
cargo test  --workspace
cargo test  --all-features        # async + ann + archive + compress touch the schema
cargo clippy --workspace --all-targets
cargo fmt --check                 # run BARE (a `| tail` pipe masks the exit code)
cargo deny check
cargo insta test --review         # confirm DDL diff is ONLY embedding_spaces + its index, then accept
```

Acceptance gates: every kept `embedding_meta` unit test passes unedited; the round-trip migration
test proves bit-for-bit fidelity + legacy-key drop; the single-active 2nd-insert test maps to a
typed error; snapshots/golden reflect only the new table+index; clippy/fmt/deny clean. CI runs
`@stable` (not MSRV) — a newer clippy lint (e.g. `collapsible_if` on the `if let … && …` in
`map_single_active_violation`) can redden the PR; prefer the idiomatic fix over `#[allow]`.

---

## Tasks

- [ ] **T0 — Plan issue.** Post this plan as a `type:plan` issue (or a comment on #622) linked to
      #622 / epic #610; confirm #622 carries `type:feature` + `area:storage` and is a sub-issue of #610.
- [ ] **T1 — Migration fidelity harness (DO FIRST).** Bump `CURRENT_SCHEMA_VERSION` 12→13; add
      `migrate_v12_to_v13` + `MIGRATIONS` entry; add `embedding_spaces` to fresh-init DDL; write &
      pass the four migration tests (round-trip / fresh / corrupt-rolls-back / idempotent),
      asserting row columns directly (API not yet repointed).
- [ ] **T2 — Domain types + registry module.** `SpaceStatus` (+`as_sql`/`from_sql`),
      `EmbeddingSpace`, `find_active`/`list_spaces`/`insert_active`/`upsert_active_fingerprint`,
      `map_single_active_violation`; register the module; registry unit tests.
- [ ] **T3 — Facade.** Re-implement the five `embedding_meta` functions over `embedding_spaces`;
      keep behavior-compat tests verbatim; retarget `load_errors_on_corrupt_json`. Demote the key const.
- [ ] **T4 — Single-active + read contract.** Partial-unique violation → typed error test;
      `find_active` zero/>1 contract test.
- [ ] **T5 — Convergence + read-only + bookkeeping.** Fresh-vs-migrated convergence test; read-only
      registry-read + needs-migration tests (audit pinned `target:`); index count 27→28; extend the
      full-chain migration test; update golden + re-accept snapshots.
- [ ] **T5b — Identity relocation (D9 BLOCKERs).** Dump: serialize `embedding_spaces` rows
      (`inspect/dump.rs`). Restore: re-insert via `insert_active` + translate a legacy
      `config['embedding_meta']` from old dumps (`inspect/restore.rs`); extend the round-trip test.
      Repoint the CLI/MCP dim probes (`cli/src/db.rs`, `mcp/src/config.rs`) at the active row +
      legacy fallback, with migration-aware tests. Retarget the `equivalence.rs`/`apply.rs` seeders to
      `embedding_meta::store`. This is mandatory correctness, not optional.
- [ ] **T6 — Docs.** The four doc updates (design doc, schema-evolution-policy, crate-layout,
      module docstrings).
- [ ] **T7 — Verification gate.** Run the full Verification block; all green.
- [ ] **T8 — PR.** Atomic Conventional-Commits commits; open PR vs `main`; body leads with the
      migration-fidelity risk + the facade boundary; links #622 + #610; states the OUT list; **no
      `closes #623/#624/#625/#689`** anywhere (auto-close trap) — only #622 closes.
- [ ] **T9 — super-review.** `/super-review` (or 2–3 adversarial subagent reviewers per
      `feedback_review_under_budget`): migration bit-fidelity, un-bypassable single-active, no
      `record_if_absent` assertion drift, `i64`/`usize` read guards, read-only `target:` pin.
      Triage the gemini-code-assist auto-review.
- [ ] **T10 — finish-pr + merge.** Rebase on latest `main` and **re-gate** (CLEAN ≠ semantically
      safe — a sibling schema PR (#630/#631 line) could bump the version; reconcile
      `CURRENT_SCHEMA_VERSION` + `MIGRATIONS` index and re-run the round-trip). Squash-merge; confirm
      #622 closes and the four OUT issues stay open.

---

## Deferred (with the reason each can wait)

| Deferred                                                      | Issue       | Why safe to defer                                                                                                     |
| ------------------------------------------------------------- | ----------- | --------------------------------------------------------------------------------------------------------------------- |
| Second live space / coexistence / query-across-spaces         | #689        | Table _models_ it; the API only ever touches the `active` row. Nothing writes `populating`/`deprecated`.              |
| Promoting `SpaceStatus`/`EmbeddingSpace` to public `types.rs` | #689        | Locked scope = API behavior-compatible; public surface stays `EmbeddingFingerprint`. Promote with the multi-space UX. |
| Background reconstruction (shadow→backfill→promote)           | #623        | Orthogonal machinery; the registry is its precondition, delivered here.                                               |
| HNSW rebuild + similarity-edge invalidation                   | #624        | Only triggered when an identity _changes_ (reconstruction); identity is write-once here.                              |
| Live-write concurrency for a 2nd space                        | #625        | This PR creates no second writable space; single-active write path unchanged from v12.                                |
| Typed `EmbeddingSpaceConflict` error variant                  | #689        | No caller branches on it this wave (#560 doctrine); structural enforcement + diagnosable `Internal` suffices.         |
| Snapshot rename `v11` → `v13`                                 | new `chore` | Cosmetic name-vs-version honesty; content update is required and done, file rename is churn.                          |

If any deferred item turns out load-bearing for #622's acceptance, surface it and let the user
decide — never silently pull forward or drop.

---

## Post-Implementation Audit

Plan tasks classified against the as-built implementation (audit is mandatory for ≥5
top-level tasks). Divergence is well under 30%; no advisor arbitration triggered.

| Task | Status | Notes |
|------|--------|-------|
| T0 — Plan issue | **Modified** | Plan published as the in-PR file (`docs/plans/2026-06-21-622-embedding-spaces-registry.md`) and referenced from the PR body, rather than a standalone `type:plan` issue. Same intent (the plan is discoverable + linked); lighter ceremony. |
| T1 — Migration fidelity harness | **Implemented** | `CURRENT_SCHEMA_VERSION` 12→13, `migrate_v12_to_v13` (frozen DDL + lossless map via a migration-local `V12Fingerprint`), 3 migration tests (roundtrip/fresh/corrupt-rolls-back; idempotency asserted inside the roundtrip test). |
| T2 — Registry module + types | **Implemented** | `store::embedding_spaces` with `SpaceStatus`/`EmbeddingSpace`, `find_active`/`list_spaces`/`insert_active`/`upsert_active_fingerprint`, `map_single_active_violation`; 7 unit tests. |
| T3 — Facade | **Implemented** | `embedding_meta` `load`/`store` delegate; `record_if_absent`/`check_compatible`/`require_present`/`ensure_match` unchanged (compose `load`/`store`). 11 behavior-compat tests verbatim + 2 new; `load_errors_on_corrupt_json` retargeted. Removed the now-dead `EMBEDDING_META_KEY` const. |
| T4 — Single-active + read contract | **Implemented** | Covered by T2's `single_active_index_rejects_second_active` + `find_active` zero/>1 contract + the facade `load_none_when_only_deprecated_row`. |
| T5 — Convergence + read-only + bookkeeping | **Implemented** | Normalized `fresh_vs_migrated_embedding_spaces_converge` + `read_only_open_reads_registry`; golden + 2 insta snapshots updated, `all_nine_indexes_created` 27→28, `migrate_v11_to_v12` test version assertions → CURRENT. |
| T5b — D9 identity relocation | **Implemented** | (Folded in pre-approval by the review.) Dump serializes `embedding_spaces` (+ `EmbeddingSpaceSnapshot` DTO); restore reconstructs via `restore_embedding_spaces` with a legacy-config fallback; CLI `peek_embed_dim_from_db` + MCP `probe_embed_dim` read the registry table-first with legacy fallback (+1 migration-aware test each); the `equivalence.rs`/`apply.rs` seeders now stamp via the facade. |
| T6 — Docs | **Implemented** | `crate-layout.md` (new module), `schema-evolution-policy.md` (data-folding-DDL section + relocation-blast-radius note), the design doc (#622 landed), module docstrings (facade + registry). |
| T7 — Verification gate | **Implemented** | `cargo fmt --check` clean; `cargo test --workspace` 1356 pass / 9 ignored; `cargo test --all-features` 1074 pass; `cargo clippy --workspace --all-targets` clean; `cargo deny check` ok. |
| T8–T10 — PR / super-review / finish-pr / merge | **Pending** | Next. |

**Minor implementation choices (same intent, not divergences):** the facade's `record_if_absent`
absent-branch writes via `store` (upsert) rather than the plan-sketched `insert_active` — both
record the identity; upsert keeps `record_if_absent` edit-free. `default_active` is `#[cfg(test)]`
(test-only convenience). Registry items are `pub` (not `pub(crate)`) to match the `pub(crate) mod
store` convention and satisfy `redundant_pub_crate`.
