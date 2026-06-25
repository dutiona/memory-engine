# Plan — #623: background reconstruction (shadow space → resumable backfill → atomic promote)

**Issue:** #623 `feat(core): background reconstruction — shadow space → resumable backfill → atomic promote`
**Type/Area:** `type:feature` / `area:core` · **Epic:** #610 (TEI/Qwen migration), Wave 2
**Schema:** v13 → v14 · **Worktree:** `.worktrees/feat-623-reconstruction` (off `main` `0e55cd2`)
**Engagement:** Thorough · **Severity mode:** Default (address all)
**Scope this PR:** SAME-dim reconstruction. Different-dim (engine effective-dim transition) is a forward-compatible follow-up (PR2) — see Deferred.

---

## BLUF

Re-embed stored fact **content** (the lossless source of truth) under a new **same-dimension** embedding
identity, in the background, then **atomically** swap the new vectors in as active — a model swap (e.g.
re-quantization or a better same-dim model) with no downtime and an instant rollback.

Schema **v13→v14** adds a generalized **`fact_vectors(fact_id, space_id, embedding)`** table that holds the
**non-active** spaces' vectors (the `populating` space during backfill; the old space, retained after a
promote, for rollback). **`facts.embedding` stays the authoritative active-space vector** — so every
existing read path (the 17 `FACT_COLUMNS` query sites, brute-force + HNSW search, dump) is **unchanged**.
Reconstruction = open a `populating` space → resumable backfill of its `fact_vectors` rows → **atomic
promote** = one transaction that retains the old active vectors into `fact_vectors[old]`, copy-swaps the
populating vectors into `facts.embedding`, and flips the registry status. **Rollback** is the inverse
copy-swap (old vectors retained).

> **Design pivot (driven by the 5-lens plan review):** an earlier draft made `fact_vectors` the read-source
> and promote an O(1) status-flip — but the review proved that "repoint all reads" is **17 hot-path
> `FACT_COLUMNS` JOIN sites** (`row_to_fact` has no DB handle), permanent complexity + the #622-D9 blast
> radius, for a benefit (O(1) vs O(N)) that only matters on the _rare_ promote. So we keep `facts.embedding`
> as the active store (zero read-path change) and pay an **O(N) copy-swap on promote** (a few seconds, once
> per reconstruction). `fact_vectors` is still the multi-space table #689 (coexistence) reads. Lower risk,
> smaller PR, same future-proofing.

Scope (locked with the user): **internal engine + `StorageBackend` port methods only** (CLI/MCP UX → #689);
**live backfill** (no quiescence) + atomic promote with a straggler catch-up; residual race → **#625**;
HNSW rebuild → **#624**; **same-dim only** (different-dim → PR2 follow-up).

---

## Scope (LOCKED — user decision)

**IN**

- New `fact_vectors(fact_id, space_id, embedding)` table; **additive** v13→v14 migration (just
  `CREATE fact_vectors` — NO data move, NO `facts` rebuild). `facts.embedding` is untouched and remains the
  active serving store.
- `embedding_spaces` registry seams `insert_populating` / `promote` / `deprecate` (the #622 reserved seams);
  a `Reconstruction` port trait (`begin_populating` / `backfill_batch` / `count_unbackfilled` / `promote` /
  `deprecate`) + `SqliteBackend` impl; an engine `reconstruct(new_fingerprint, embedder)` orchestration.
- Resumable, crash-safe, idempotent backfill (cursorless anti-join, `ON CONFLICT DO NOTHING`,
  `spawn_blocking` embed) into `fact_vectors[populating]`.
- **Atomic promote** (one transaction): retain old active vectors → `fact_vectors[old]`; copy
  `fact_vectors[populating]` → `facts.embedding`; flip registry status (demote-then-activate). The
  completeness gate runs **inside** the tx; straggler catch-up (off-lock embed) runs **before** the tx.
- **Same-dim guard:** promote asserts `new_space.dim == active.dim` and errors otherwise (different-dim is
  PR2). The new fingerprint (carrying its dim) is surfaced in `PromoteOutcome` so PR2 relaxes the guard
  without an API change.

**OUT — separate issues; leave clean seams**

- **Different-dim reconstruction** (engine effective-`embed_dim` transition: rebuild pool / dim-validation /
  HNSW at D′) — **new follow-up issue (PR2, this session)**. The storage layer (`fact_vectors`,
  dim-on-space-row, backfill, promote) is already dim-agnostic; PR2 relaxes the same-dim guard + adds the
  engine-lifecycle transition. Entangled with #624 (HNSW reconfig at a new dim).
- HNSW rebuild + similarity-edge invalidation on promote — **#624** (promote fires a rebuild hook; until
  #624, a full `build_from_db` over the new active vectors — needed even same-dim, since the vectors change).
- Live-write race-freedom (a write landing inside the promote window) — **#625** (`PromoteOutcome` + the
  catch-up loop are shaped so #625 adds quiesce/retry without an API break).
- Operator UX (CLI/MCP) + querying a non-active space (coexistence/rollback UX) — **#689** (reads
  `fact_vectors[space]`, pure addition).
- Summary/lineage vector re-embedding — facts-only this wave (ADR 0015 §4 summary obligation noted, not done).

---

## Synthesis rationale (three-lens panel + 5-lens review)

Drafts at `docs/plans/2026-06-25-623-background-reconstruction-drafts/`. The 5-lens adversarial review
(`-advisor.md` / `-subagent-review.md`) drove the final shape:

- **Keep `facts.embedding` active; `fact_vectors` = non-active spaces; O(N) copy-swap promote ← risk-first,
  confirmed by the review's HIGH finding** that "repoint all reads → O(1) promote" is 17 hot-path JOIN sites
  (the synthesis had over-sized that as one edit). Zero read-path change; trivially additive migration.
- **Module boundary + `Reconstruction` port trait + the registry seams ← architecture-first**, trimmed
  (orchestration folds into `engine/reconstruct.rs`; no separate `src/reconstruct/` module — the review
  flagged the 3-module split as over-structured for #623's now-smaller scope).
- **Risk-sequencing + retirement-gate tests + cursorless anti-join backfill ← risk-first.**
- **Completeness gate INSIDE the promote tx** (not pre-tx — the review's HIGH TOCTOU finding); **same-dim
  scope** (the review's HIGH: engine `embed_dim` is frozen at open).
- Option A (mvp-first) and the O(1)-flip/DROP variant (architecture-first) are documented as the rejected
  alternatives with the evidence.

---

## Design decisions

### D1 — `fact_vectors` table (additive; holds NON-active spaces)

```sql
CREATE TABLE IF NOT EXISTS fact_vectors (
    fact_id   INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    space_id  TEXT    NOT NULL REFERENCES embedding_spaces(name) ON DELETE CASCADE,
    embedding BLOB    NOT NULL,
    PRIMARY KEY (fact_id, space_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_fact_vectors_space ON fact_vectors(space_id);
```

- Holds vectors for **non-active** spaces only: the `populating` space during backfill, and the previous
  active space (now `deprecated`) retained after a promote for rollback. The **active** space's vectors stay
  in `facts.embedding` — so there is exactly one source of truth for the served vector (**no drift**).
- `space_id TEXT → embedding_spaces(name)` (the registry PK) — no surrogate, no `embedding_spaces` rebuild
  → v14 stays additive. (Numeric-surrogate join-key perf is a later optimization if profiling warrants.)
- Composite PK `(fact_id, space_id)` `WITHOUT ROWID` = the `ON CONFLICT(fact_id, space_id)` idempotency key
  - the point-read key. `ON DELETE CASCADE` from both parents (hard-delete a fact → its non-active vectors
    go; drop a deprecated space → its vectors go). `idx_fact_vectors_space` serves the backfill/promote scans.

### D2 — `facts.embedding` stays the active serving store (no read-path change, no drop)

`facts.embedding` remains exactly what it is today: the active space's vector, read by all 17 `FACT_COLUMNS`
sites, `vector.rs` brute-force, `ann.rs` HNSW, and dump — **all unchanged**. Reconstruction never makes these
read `fact_vectors`. No vestigial column, no v15 drop, no JOIN surgery. (This reverses the earlier
DROP/repoint design after the review priced its cost.)

### D3 — `migrate_v13_to_v14` (purely additive)

`CREATE fact_vectors` + its index. **No data move** (active vectors stay in `facts.embedding`; `fact_vectors`
starts empty). Registered `(migrate_v13_to_v14, false)` (no FK disable). The migration is one `execute_batch`
of frozen DDL — the lowest-risk migration shape (cf. the additive part of `migrate_v12_to_v13`). DDL-oracle
updates only (no fidelity-of-moved-data concern): index count + golden + insta snapshots. Three branches are
moot here (nothing is copied) — a fresh or never-embedded store simply gets an empty `fact_vectors`.

### D4 — NO read-path repoint (the review's HIGH finding, resolved by D2)

Because `facts.embedding` stays the active store, **none** of the readers change: `FACT_COLUMNS` /
`row_to_fact` (`facts.rs:22,1250`), `vector.rs:106`, `ann.rs:107/157/388`, `dump.rs`. Only **new**
reconstruction code touches `fact_vectors` (backfill writes it; promote reads/copies it). The grep audit
(the #622-D9 discipline) is still run to _prove_ no existing reader was accidentally changed and that
`fact_vectors` is only touched by new code.

- **Dump/restore (additive):** dump gains a `fact_vectors` section (so non-active spaces survive a backup,
  for #689 rollback). `EngineSnapshot` gains `#[serde(default)] fact_vectors: Vec<FactVectorSnapshot>`
  (pre-#623 snapshots default empty). Restore writes them after `embedding_spaces` + `facts` (FK order:
  **embedding_spaces → facts → fact_vectors**); `assert_empty_db` adds `fact_vectors`. `facts[].embedding`
  is unchanged in the snapshot (still the active vector). No legacy fallback needed (active vector still
  lives where it always did).

### D5 — Resumable backfill (cursorless anti-join + idempotent) into the populating space

`insert_populating(EmbeddingSpace{ status: Populating, .. })` (registry seam; never trips the single-active
index). Each batch: `SELECT f.id, f.content FROM facts f LEFT JOIN fact_vectors v ON v.fact_id=f.id AND
v.space_id=:pop WHERE v.fact_id IS NULL AND f.id > :last_id ORDER BY f.id LIMIT :batch` (the populating
space's missing rows — **no persisted cursor**; the absent row IS the work signal; self-corrects after a
concurrent insert). Embed off the write lock under **`spawn_blocking`** (sync consumer trait; `reqwest::blocking`
safe there — the #631 pattern; `Arc<dyn EmbeddingProvider>` captured by a `'static` closure; no pool guard
across `.await`). Write the batch in one `block_write` tx with `INSERT … ON CONFLICT(fact_id, space_id) DO
NOTHING` (idempotent on replay). Each batch commits independently → a crash loses ≤1 in-flight batch,
re-derived by the anti-join on restart.

### D6 — Atomic promote (O(N) copy-swap, same-dim) — completeness gate INSIDE the tx

Pre-tx (off-lock): a **catch-up** pass = run the D5 backfill loop once more to embed any stragglers (facts
ingested after their cursor passed — live backfill allows this). Then the atomic step, one `block_write` /
`unchecked_transaction`:

```sql
-- (0) same-dim guard already checked from the space rows (new.dim == active.dim), else error.
-- (1) completeness gate INSIDE the tx (no TOCTOU): any active fact lacking a populating vector → abort.
SELECT COUNT(*) FROM facts f LEFT JOIN fact_vectors v
  ON v.fact_id=f.id AND v.space_id=:pop WHERE f.t_expired IS NULL AND v.fact_id IS NULL;  -- must be 0
-- (2) retain the OLD active vectors for rollback:
INSERT INTO fact_vectors (fact_id, space_id, embedding) SELECT id, :old_active, embedding FROM facts;
-- (3) copy the populating vectors into the active store (the O(N) swap):
UPDATE facts SET embedding = (SELECT embedding FROM fact_vectors WHERE fact_id=facts.id AND space_id=:pop);
-- (4) flip status (demote-then-activate so the partial-unique index never sees two actives):
UPDATE embedding_spaces SET status='deprecated' WHERE status='active';
UPDATE embedding_spaces SET status='active'     WHERE name=:pop;
-- (5) the populating space's rows are now redundant with facts.embedding → DELETE fact_vectors WHERE space_id=:pop;
```

All-or-nothing in one tx (the #631-incident lesson: a single `block_write` closure, never decomposed
per-call). The active-flip IS the identity flip (the single active row's tuple is what `embedding_meta::load`
reads — #622 collapsed KB's parallel `config` writes) and, because `facts.embedding` now holds the new
vectors, the served-vector flip too — **all atomic** (retires R4). `PromoteOutcome { promoted, deprecated,
stragglers_caught, rebuild_index, new_fingerprint }`. Post-commit (engine-side, off-lock): HNSW rebuild hook
(#624; full `build_from_db` until then — required because the vectors changed); re-stamp the engine
fingerprint (dim unchanged this PR). **Rollback (#689 mechanism seam):** the inverse copy-swap (retain
current → `fact_vectors[current]`, load `fact_vectors[old]` → `facts.embedding`, flip status) — the retained
old vectors from step (2) make it possible.

### D7 — Module boundary (trimmed)

`src/store/fact_vectors.rs` (NEW — row CRUD + the backfill/promote SQL); extend
`src/store/embedding_spaces.rs` (the reserved `insert_populating`/`promote`/`deprecate` seams — reconcile the
#622 comment: #623 owns the mechanism, #689 the UX); `src/storage/reconstruction.rs` (NEW — `Reconstruction`
port trait) + `src/storage/sqlite/reconstruction.rs` (NEW — impl, delegating to the store free-fns via
`block_read`/`block_write`); `src/engine/reconstruct.rs` (NEW — the `reconstruct()` orchestration; **no**
separate `src/reconstruct/` module — folded here per the review). `StorageBackend` gains `Reconstruction` as
a supertrait (core lifecycle). `#632` conformance: add the `Reconstruction` methods to the conformance stub
so `PgBackend` (#634) must honor them (noted, not implemented).

---

## Files to change

| File                                                                                                                                      | Change                                                                                                                                                                                                                                                                        |
| ----------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/store/schema/mod.rs`                                                                                                                 | `CURRENT_SCHEMA_VERSION` 13→14; `fact_vectors` + index in `TABLES_DDL`/`INDEXES_DDL`; append `(migrate_v13_to_v14, false)`; DDL-oracle: `all_nine_indexes_created` 28→29 + tally, golden `schema_ddl_snapshot_is_stable`, the migration-chain insta `.snap`s; migration test. |
| `src/store/schema/migrations.rs`                                                                                                          | `migrate_v13_to_v14` (frozen `CREATE fact_vectors`).                                                                                                                                                                                                                          |
| `src/store/fact_vectors.rs` (NEW)                                                                                                         | row CRUD; `insert_populating_batch` (anti-join window + `ON CONFLICT`); `count_unbackfilled`; the promote copy-swap SQL helpers; unit tests.                                                                                                                                  |
| `src/store/embedding_spaces.rs`                                                                                                           | `insert_populating` / `promote` / `deprecate`; update the seam comment.                                                                                                                                                                                                       |
| `src/storage/reconstruction.rs` (NEW) + `src/storage/sqlite/reconstruction.rs` (NEW) + `src/storage/mod.rs` + `src/storage/sqlite/mod.rs` | `Reconstruction` port trait + SqliteBackend impl + supertrait wiring.                                                                                                                                                                                                         |
| `src/engine/reconstruct.rs` (NEW) + `src/engine/mod.rs`                                                                                   | `reconstruct(new_fp, embedder)` orchestration.                                                                                                                                                                                                                                |
| `src/inspect/dump.rs`, `restore.rs`, `types.rs`                                                                                           | additive `fact_vectors` snapshot section + restore FK order + `assert_empty_db`.                                                                                                                                                                                              |
| docs                                                                                                                                      | see Documentation.                                                                                                                                                                                                                                                            |

No change to the vector read paths (`vector.rs`, `ann.rs`, `facts.rs` `row_to_fact`/`FACT_COLUMNS`), to
`error.rs` variants (reuse `Internal`/`Migration`), or to consumer crates beyond recompile.

---

## Tasks (phased; risk-retirement order; TDD — failing test first)

- [ ] **T0 — Plan issue** (at finish-pr): archive as `type:plan`+`area:core` under #610; remove `docs/plans/2026-06-25-623-*` from the PR; close after merge. **Also file the different-dim follow-up issue (PR2).**
- [ ] **T1 — Phase 1: v14 migration + `fact_vectors` table + registry seams (retire R1).** `CURRENT_SCHEMA_VERSION`→14; `migrate_v13_to_v14`; `fact_vectors.rs` CRUD; `insert_populating`/`promote`/`deprecate`. Gates: `migrate_v13_to_v14_creates_empty_fact_vectors` + `fresh_v14_matches_migrated_v14` (convergence) + `migrate_v13_to_v14_idempotent`; DDL-oracle (index 28→29, golden, insta); `insert_populating_coexists_with_active`; **grep audit = no existing reader of `facts.embedding` changed, `fact_vectors` only touched by new code.**
- [ ] **T2 — Phase 2: resumable backfill (retire R3 + R6).** `Reconstruction` port + SqliteBackend impl `backfill_batch` (cursorless anti-join, `ON CONFLICT DO NOTHING`, `spawn_blocking` embed, `block_write`); engine backfill loop. Gates: `backfill_writes_one_vector_per_fact`, `backfill_resumes_after_crash_without_duplicates`, `_idempotent_on_replay`, `_picks_up_fact_inserted_mid_reconstruction`, async-off-lock evidence.
- [ ] **T3 — Phase 3: atomic copy-swap promote (retire R2 + R4 + R5).** `promote` = same-dim guard + (pre-tx, off-lock) catch-up + (one tx) completeness-gate-INSIDE + retain-old + copy-swap + status-flip + cleanup; `PromoteOutcome`; post-commit HNSW-rebuild hook (#624 stub) + identity re-stamp. Gates: `promote_is_atomic_exactly_one_active_throughout`, `promote_rollback_on_mid_tx_error_leaves_old_active_intact` (crash injection via the `raw_exec` #727 seam), `query_mid_reconstruction_sees_only_active` (search returns old vectors until promote commits), `promote_refuses_incomplete_populating`, `promote_catches_up_stragglers_before_tx` (renamed — catch-up is pre-tx), `promote_rejects_different_dim` (same-dim guard), `promote_flips_served_identity`, `query_under_old_identity_after_promote_is_rejected` (#614).
- [ ] **T4 — Phase 4: engine `reconstruct()` end-to-end (same-dim).** Compose begin→backfill→promote. Gate: `reconstruct_full_cycle_swaps_same_dim_model` (`#[tokio::test]`, distinguishable test embedders) + a mid-cycle crash→resume variant. (Different-dim gate → PR2.)
- [ ] **T5 — Dump/restore (additive `fact_vectors` section).** Snapshot section (all non-active spaces), restore FK order, `assert_empty_db`. Gates: v14 round-trip (a deprecated space's vectors survive), pre-#623 snapshot back-compat (empty `fact_vectors`, active vector still under `facts[].embedding`), empty-db rejection.
- [ ] **T6 — Docs** (below) + the different-dim follow-up issue + #624/#625/#689 seam notes.
- [ ] **T7 — Verification gate** (workspace + `--all-features` + clippy `-D warnings` + fmt + cargo-deny + insta).
- [ ] **T8 — PR** (worktree-absolute paths only; `feat(core): background reconstruction — same-dim (#623)`; `type:feature`+`area:core`; under #610; no `closes #624/#625/#689`).
- [ ] **T9 — Review** (2–3 adversarial subagent lenses → adversarial-verify, per [[feedback_review_under_budget]]: migration additivity + no-reader-changed, promote single-`block_write` atomicity + completeness-gate-inside-tx, backfill resumability, async/!Send, same-dim guard; verify worktree bytes after any review agent). Triage gemini-bot.
- [ ] **T10 — finish-pr (BEFORE merge) → rebase + re-gate → squash-merge.** `closingIssuesReferences` = #623 only; `git status --porcelain` before merge; verify `HEAD==origin/main` after.

---

## Documentation

Not N/A. `docs/design/embedding-identity-and-tei-qwen-migration.md` §Design.7 + Out-of-scope: mark same-dim
reconstruction implemented (#623); describe the `facts.embedding`-stays-active + `fact_vectors`-holds-non-active
design + the O(N) copy-swap promote + the rollback retention; note different-dim → follow-up.
`docs/design/adr/0015-cross-layer-embedding-identity-policy.md` §4: mark ME reconstruction implemented (the
copy-swap mechanism; KB uses per-space vec0 tables — same semantics) — **flag the KB-copy parity divergence,
do NOT edit the KB copy here** (shared-doc lockstep). `docs/design/schema-evolution-policy.md`: v13→v14 entry
(purely additive table-add). `docs/reference/crate-layout.md`: register `store::fact_vectors`,
`storage::reconstruction`, `engine::reconstruct`. New `docs/advanced/reconstruction.md` (sibling to
`consolidation.md`): the lifecycle, single-active invariant, the same-dim scope + different-dim/#624/#625/#689
dependencies. Module rustdoc on the new modules + the updated `embedding_spaces.rs` seam comment.

## Testing

Unit + integration per phase (TDD), all retirement gates T1–T5. **Levels:** unit (store free-fns, migration,
registry seams), integration (`Reconstruction` port via SqliteBackend — the consolidation port-test pattern),
engine `#[tokio::test]` (end-to-end + crash-resume). e2e/CLI excluded — **N/A: internal API only (#689 owns
the operator surface).** Key gates: additive migration + convergence; **promote atomicity** (single
`block_write`, completeness-gate-inside-tx, rollback-on-mid-tx-error via crash injection); resume-after-crash

- idempotency; query-mid-reconstruction sees only active (the served vector doesn't change until promote
  commits); same-dim guard rejects different-dim; #614 mismatch tracks the identity flip; dump/restore
  multi-space round-trip + pre-#623 back-compat. The `--all-features` run exercises the HNSW path (still reads
  `facts.embedding`, but the post-promote rebuild hook touches it) — run it.

## Verification

```bash
cd .worktrees/feat-623-reconstruction
cargo build --workspace
cargo test  --workspace
cargo test  --workspace --all-features          # HNSW + archive + compress + test-util
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --check                               # run BARE (a `| tail` pipe masks the exit code)
cargo deny check
cargo insta review                              # v14 DDL snapshots — diff = ONLY fact_vectors + its index
cargo test -p memory-engine migrate_v13_to_v14 fact_vectors reconstruct  # targeted
```

Acceptance: additive migration + convergence green; **grep audit shows no existing `facts.embedding` reader
changed**; promote single-`block_write` atomicity proven by the crash-injection test + completeness gate
inside the tx; same-dim guard test green; dump/restore multi-space round-trip + back-compat green;
clippy/fmt/deny clean. CI runs `@stable` — re-run clippy on fresh `main` after the batch; verify
`HEAD==origin/main`.

---

## Deferred (with the reason each can wait)

| Deferred                                                                      | Issue                                 | Why safe to defer                                                                                                                                                                                                                                       |
| ----------------------------------------------------------------------------- | ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Different-dim reconstruction** (engine effective-`embed_dim` transition)    | **new follow-up (PR2, this session)** | Storage layer is already dim-agnostic; PR2 relaxes the same-dim guard + adds the engine pool/HNSW/validation rebuild at D′. PR1's `PromoteOutcome` already carries the new fingerprint/dim → no rework. Entangled with #624 (HNSW reconfig at new dim). |
| HNSW rebuild + similarity-edge invalidation on promote                        | #624                                  | Promote fires the rebuild hook (full `build_from_db` until #624 — needed even same-dim since vectors change).                                                                                                                                           |
| Live-write race-freedom (write inside the promote window)                     | #625                                  | `PromoteOutcome` + catch-up loop shaped so #625 adds quiesce/retry without an API break; #625 is a research issue.                                                                                                                                      |
| Operator UX (CLI/MCP) + querying a non-active space (coexistence/rollback UX) | #689                                  | #623 ships the mechanism + `fact_vectors`; #689's reads/commands are pure additions.                                                                                                                                                                    |
| Numeric `space_id` surrogate (join-key perf)                                  | later                                 | TEXT FK keeps v14 additive; profile first.                                                                                                                                                                                                              |
| Summary/lineage vector re-embedding                                           | #689/#624                             | Facts-only this wave; ADR 0015 §4 obligation noted.                                                                                                                                                                                                     |

## Rejected alternatives (the panel + review evidence)

- **DROP `facts.embedding` / repoint all reads / O(1) status-flip promote** (architecture-first) — rejected:
  17 hot-path `FACT_COLUMNS` JOIN sites + a heavy `facts` rebuild; O(1) benefit only on the rare promote.
- **Separate `fact_shadow_vectors` single-shadow table** (mvp-first Option A) — subsumed: `fact_vectors`
  keyed by `space_id` is the multi-space generalization that also serves #689 rollback/coexistence at no
  extra cost over A.
- **Different-dim in this PR** — rejected: needs the engine-lifecycle rebuild + #624 HNSW reconfig; split to
  PR2 with no rework.
