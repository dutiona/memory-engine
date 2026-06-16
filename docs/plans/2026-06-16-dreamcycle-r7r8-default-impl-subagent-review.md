# Subagent Review — DreamCycle R7/R8 + Default DBSCAN Impl (#49)

**Reviewer:** fresh-eyes subagent, no prior context.
**Plan under review:** `docs/plans/2026-06-16-dreamcycle-r7r8-default-impl.md`
**Method:** read the plan, then validated load-bearing claims against the worktree source
(`types.rs`, `traits.rs`, `engine/cognitive.rs`, `engine/consolidation.rs`, `engine/outcome.rs`,
`store/facts.rs`, `store/schema.rs`, `store/lineage.rs`, `pool/connection_pool.rs`, `async_engine.rs`).

**Verdict: strong plan, conditionally approve.** No BLOCKERs. The grounding section is unusually
accurate — every load-bearing claim I spot-checked is correct. The findings below are mostly HIGH/MEDIUM
gaps in the *apply* mechanics (the load-bearing Task 5) and a couple of structural items the plan glosses.

---

## Claim verification (the load-bearing grounded claims)

All of the following were checked against the actual code and are **CORRECT**:

| Claim | Status | Evidence |
|---|---|---|
| Old `CycleReport` = `{facts_evaluated, facts_promoted, facts_rescored, facts_expired, promotions: Vec<PromotionProvenance>}` | ✅ | `types.rs:419-430` exact match |
| Serde round-trip test for old `CycleReport` | ✅ | `types.rs:844-845` (`facts_promoted: 5, facts_rescored: 20` in a test) |
| **Blast radius is core-internal** — no `CycleReport`/`DreamCycle`/`DreamContext` in CLI/MCP/embed/examples/tests | ✅ | Workspace grep: matches only in core `src/`, docs, drafts, and stale `qa/` reports. No `memory-engine-{cli,mcp,embed}` or `tests/`/`examples/` hits. |
| `facts_promoted`/`facts_rescored` outside `types.rs` only in `cognitive.rs` (NoopCycle + delegate test) | ✅ | grep returns only `cognitive.rs:278-279,355` and `types.rs` |
| `CURRENT_SCHEMA_VERSION = 11` | ✅ | `schema.rs:8` |
| `DreamCycle::run(&self, ctx: &DreamContext) -> Result<CycleReport>` | ✅ | `traits.rs:199-202` |
| `consolidate()` wraps 3 passes in `conn.unchecked_transaction()` | ✅ | `consolidation/mod.rs:85`, commits at `:104`; watermark `last_consolidated_at` via `get_config`/`set_config` at `:76`/`:101` |
| `promote_with_lineage` acquires its own write lock | ✅ | `cognitive.rs:186` (`self.write_conn()?`), then `conn.savepoint()` at `:212` |
| Pool is `parking_lot::Mutex`, non-reentrant; `try_write()` returns `write_conn.lock()` | ✅ | `connection_pool.rs:19,213-225`. **Self-deadlock-on-reentry is real** → `promote_in_conn` extraction is mandatory, as claimed. |
| `cosine_similarity` returns 0.0 on zero vector (NaN-safe) | ✅ | `vector.rs:28` (`if denom == 0.0 { 0.0 }`) |
| `expire` is soft-delete with `t_expired IS NULL` guard, `NotFound` if 0 rows | ✅ | `facts.rs:330-340` |
| `DreamCycleConfig` defaults 0.2/0.8/0.8 + `promotion_percentile=0.75` + `validate()` | ✅ | `types.rs:449-511` |
| FactStore primitives present (`insert`, `get`, `list_active`, `list_active_in_period`, `set_pinned`, `update_importance_score`) | ✅ | `facts.rs:66,203,224,557,610,757` |
| **No** `merge_metadata`/supersede/quarantine/`json_set` primitive exists yet | ✅ | grep returns zero matches in `src/` |
| `record_outcome`/`get_outcome_counts` present, append `OutcomeSignal` events | ✅ | `outcome.rs:29,67` |
| `async_engine.rs` `run_dream_cycle` wrapper returns `CycleReport` (delegates) | ✅ | `async_engine.rs:~585` (`spawn_blocking` → `engine.run_dream_cycle`) |

Minor: a few line numbers drift slightly (e.g. plan says `promote_with_lineage` "uses `conn.savepoint()` (`cognitive.rs:212`)" — correct; "promote_with_lineage acquires its own write lock (`cognitive.rs:186`)" — correct; the method *definition* is at `:153`, the lock acquisition at `:186`). The plan cites the load-bearing line (the lock site), which is the right one. No load-bearing claim is wrong.

---

## Findings

### [HIGH] H1 — `EventStore::new` requires an `upcaster_registry`; the apply tx must thread it, and `record_outcome` must NOT be reused

Task 5 says `TagOutcome → EventStore::insert OutcomeSignal`. Verified this is the *right* call (not
`record_outcome`, which acquires its own `write_conn()` at `outcome.rs:53` → would self-deadlock inside
the held apply transaction — same trap as `promote_with_lineage`). **Good.** But two under-specified
details will bite the implementer:

1. `EventStore::new(&conn, &self.upcaster_registry)` takes a registry ref (`outcome.rs:54`). The
   `apply_cycle_report` body must thread `self.upcaster_registry` into the event insert. The plan's
   "`EventStore::insert OutcomeSignal`" hand-waves this — call it out so the implementer doesn't reach
   for the non-existent zero-arg constructor.
2. `record_outcome` also validates fact existence via a **read-pool** checkout *before* the write
   (`outcome.rs:32`). The apply path validates existence in `validate_report` already, so skipping that
   re-check inside the tx is correct — but the plan should state that the `OutcomeSignal` payload shape
   (`{"fact_id", "outcome"}`, `source:"outcome_tracking"`, `scope_id: ROOT_SCOPE_ID`) must be replicated
   verbatim, or `get_outcome_counts`'s SQL (`json_extract(payload,'$.fact_id')`, `event_type='OutcomeSignal'`)
   won't see the rows. This is a silent-data-loss hazard if the apply path emits a different payload shape.

**Fix:** Task 5 should add a `pub(crate)` store-level helper `EventStore::record_outcome_in_conn` (or have
`apply` build the exact `NewEvent` from `outcome.rs:38-51`) and reference the canonical payload contract.

### [HIGH] H2 — `validate_report` uses the read pool, then `apply` re-reads under the write lock: TOCTOU window

Task 5 splits validation (`validate_report` — "single read connection") from application (`write_conn()`
once). Between the read-pool validation and acquiring the write lock, another writer (a concurrent
`forget`, `consolidate`, `add_fact`, or a second `apply_cycle_report`) can expire/mutate a fact that
validation just approved. The plan's rollback test (5c) covers *invalid-delta* rollback but not the
**TOCTOU** case where a delta validated-OK then fails mid-apply because the world changed.

Two acceptable resolutions — the plan must pick one:
- **(preferred)** Re-validate inside the write transaction (validation becomes a pure function over the
  *held* connection; the read-pool pre-flight is then just an optimization / early-out, and any
  divergence at apply time surfaces as a `CycleError` that rolls the whole tx back). Since
  `expire`/`update_importance_score` already `NotFound`-guard on 0 rows, most divergences self-detect —
  but `update_importance_score` does **NOT** guard (`facts.rs:610-616` ignores the changed-row count),
  so an `AdjustScore` on a concurrently-expired fact would silently succeed on a soft-deleted row.
- Accept the window and document it as a known limitation alongside the two honest limitations (single
  writer in practice; the engine serializes writers via the pool mutex anyway, so the only real racer is
  another thread holding an `Arc<MemoryEngine>`).

Given the pool serializes all writers through one mutex, the practical risk is low, but the plan asserts
"store byte-identical" rollback as a *test* — that guarantee is only true if validation re-runs under the
lock. **Flag and decide explicitly.**

### [HIGH] H3 — `update_importance_score` does not exist as an importance *adjustment*; it overwrites a *materialized* column, and there are TWO importance fields

`AdjustScore → update_importance_score(clamp(cur + adj*STEP))` (Task 5) writes the
`importance_score` column (`facts.rs:610`, `UPDATE facts SET importance_score = ?1`). But `update_importance`
(`facts.rs:347`) writes a *different* column, `importance`. The codebase has **two** importance fields —
`importance` (raw) and `importance_score` (materialized, used by `list_active_in_period`/`list_by_importance_score`
for ordering). The prior super-qa run even flagged this ambiguity (`core-root/design-importance-field-ambiguity`,
`types.rs:133-139`).

The plan picks `update_importance_score` (the materialized one) without justifying *which* field the
±2×STEP delta should move, and reads "cur" without saying which column `cur` is. If `AdjustScore` reads
`fact.importance` but writes `importance_score`, the two diverge and the next cycle's "cur" is wrong.
Worse, the forgetting/decay math may recompute `importance_score` from `importance`, silently reverting
the adjustment. **Decision D1 is incomplete:** it specifies the i16→f64 mapping but not the target column
or its interaction with the decay recomputation. **This needs a grounded answer before Task 5/Task 9.**

### [MEDIUM] M1 — "produce" phase runs `consolidate()` (a committed write) before the human review gate (D3)

D3's whole point is produce/apply separation so patterns are reviewed *before any mutation*. But Task 9
step (b) has `DefaultDreamCycle::run` call `ctx.consolidate()` — which `engine/consolidation.rs:22-58`
**commits its own transaction** (dedup may expire facts, rebuild the graph, notify HNSW). So
`run_dream_cycle` is *not* side-effect-free: it mutates the store (dedup/cluster/global summaries) during
the "produce" phase, before the report is reviewed or `apply_cycle_report` is called.

The plan acknowledges this obliquely ("A full `run_dream_cycle` is not globally atomic… `consolidate()`
commits in its own transaction before `apply_cycle_report`"), but it undersells the **semantic** problem:
the review gate (D3) is advertised as "present patterns for human review before any promotion," yet
consolidation has *already* irreversibly fused/expired facts by the time the human sees the report. That's
a meaningful caveat for the issue's pipeline-step-4 requirement. Either:
- move `consolidate()` into the *apply* phase (so produce is truly read-only), or
- explicitly document that "review gate" covers only deltas (Promote/Quarantine/Supersede/AdjustScore),
  NOT the consolidation pass, which is unconditional.

The MVP-first sequencing in the plan leans on `consolidate()` for the clustering substrate, so moving it
is non-trivial — but the gate's honesty depends on saying so plainly. **Tighten D3's scope statement.**

### [MEDIUM] M2 — No "undreamt" selection primitive exists; Task 4's `list_undreamt_in_period` is hand-waved between two strategies

Verified: `list_active_in_period` (`facts.rs:757`) filters by `t_expired`/`t_valid`/`t_invalid`/scope/type
and orders by `importance_score DESC` — there is **no** metadata predicate. Task 4 offers a fork —
"a `list_undreamt_in_period(...)` selection helper **(or Rust-side filter on `Fact.metadata`)**." These
are very different in cost and correctness:
- SQL `json_extract(metadata, '$.dream_cycled')` predicate: efficient, but `metadata` is unindexed → full
  scan over the period window anyway (acceptable, matches existing patterns).
- Rust-side filter: loads the whole window into memory, then filters — fine for bounded windows but
  interacts badly with `MAX_CLUSTER_FACTS`/`DBSCAN O(N²)` if the window is large and mostly-dreamt.

Pick one in the plan. The SQL predicate is the better default (selection happens before the O(N²) DBSCAN
sees the points). Also: the "dream-cycled" marker is written by `mark_dream_cycled` *inside the apply tx*
(Task 5), but selection happens in the *produce* phase (Task 9). On a re-run before apply, the same facts
are re-selected and re-emitted — idempotency (Task 10 "re-run selects ~0") only holds **after** a prior
`apply_cycle_report`, not after a bare `run_dream_cycle`. State this precondition in Task 10.

### [MEDIUM] M3 — `EmptyReport` variant indecision left in the plan (Task 3)

Task 3 literally contains an unresolved decision: "`EmptyReport` (decide: empty report = `Ok(empty)` …
**recommend empty = Ok no-op**, drop this variant)." A plan should not ship an open enum-variant question
into implementation. The recommendation (empty → `Ok(ApplyResult::default())`, no variant) is correct and
consistent with Task 5 test (d). **Resolve it in the plan text** — delete the variant, don't leave the
TODO.

### [MEDIUM] M4 — Supersede ↔ lineage semantics: `LineageStore::insert` validates source facts exist and is promotion-shaped

Task 5 maps `Supersede → expire(old) + LineageStore::insert(new ← [old])`. But `LineageStore::insert`
(`lineage.rs:31`) is the *promotion* lineage primitive — it takes a `&PromotionProvenance` and
`insert_rejects_nonexistent_source_facts` (`lineage.rs:363`) proves it validates source-fact existence.
Two issues:
1. A `Supersede` is not a *promotion to wisdom*; reusing the wisdom-lineage table conflates "this fact
   superseded that one" with "this wisdom was distilled from those facts." D9 says Supersede reuses
   "conflict machinery," but Task 5 wires it to **lineage**, not conflict resolution. Pick the right
   table/semantics — the plan is internally inconsistent (D9 ADR bullet says "Supersede-reuses-conflict-
   machinery"; Task 5 says LineageStore).
2. `Supersede` requires a `PromotionProvenance` to call `LineageStore::insert` — but the `Supersede{old_id,
   new_id}` delta carries none. Either synthesize a provenance (smell) or use `insert_raw`
   (`lineage.rs:76`, takes a `LineageSnapshotEntry`) or a genuinely different supersede primitive.
   **Unresolved — Task 5 will not compile as written.**

### [MEDIUM] M5 — Forward-reference resolution in `Supersede` requires intra-batch ID mapping that the plan only half-specifies

Test (f) and `validate_report` mention `Supersede` `new_id` "exists-or-introduced-earlier-in-vec." But
`AddFact` IDs are assigned by `FactStore::insert` *at apply time* (autoincrement PK) — they are **not**
known at validation time (which runs on the read pool, before any insert). So a `Supersede{old_id,
new_id}` that references an `AddFact`'d fact cannot use a real DB id in the delta; it must use a
*placeholder/positional* reference resolved during replay. The plan never defines the placeholder scheme
(negative ids? vec-index tokens? a `NewFactRef` enum?). `validate_report`'s "new_id exists-or-introduced-
earlier-in-vec" check is **unimplementable** without that scheme. **Specify the intra-batch reference
representation** (recommend: `CycleDelta::AddFact` returns a positional handle and `Supersede`/`Promote`
reference it by index, resolved into real ids in the `new_fact_ids` map during replay).

### [LOW] L1 — `IMPORTANCE_STEP` clamp domain vs. importance field range

D1 clamps the store delta to `[0,1]`. Confirmed `importance` is documented as `[0.0, 1.0]` (`types.rs:409`
on `Insight`). But `importance_score` (the materialized column H3 targets) may carry decay-scaled values
outside a naive `[0,1]` read of "cur." Confirm the clamp domain matches whichever column H3 settles on.

### [LOW] L2 — `cargo test --all-features` exercises HNSW notify, but the plan's post-commit notify list omits `AdjustScore`/`TagOutcome`

Task 5: "post-commit HNSW notify (insert for AddFact/Promote, expire for Quarantine/Supersede)." Correct —
`AdjustScore`/`TagOutcome` don't change vectors. Just confirm `Quarantine` (which `expire`s) emits
`notify_expire` and a re-add never happens (it doesn't). Non-blocking; listed for completeness.

### [LOW] L3 — ADR number placeholder `00NN`

Documentation section references `docs/design/adr/00NN-delta-based-cycle-report.md`. Resolve the next ADR
number (CLAUDE.md says 9 ADRs exist) before writing — trivial, but a literal `00NN` will ship if not.

---

## Structural completeness (requirement #4)

| Section | Present? | Note |
|---|---|---|
| Documentation | ✅ | Thorough — rustdoc, new `dream-cycle.md`, ADR, crate-layout, CLAUDE.md table rows, ROADMAP-frozen guard. Resolve L3. |
| Testing | ✅ | Per-task unit + integration; transactionality + lock-safety regression + determinism + word-boundary negatives. e2e explicitly N/A with reason (MCP surface is #225). Good. |
| Verification | ✅ | Full workspace gate + `--all-features` + doc-tests + schema snapshot + `insta pending` + grep for stale fields. Strong. |

All three sections present and substantive. **No structural gap.**

---

## Over-engineering check (requirement #3)

Mostly disciplined. Two mild flags:
- **`CycleAnomaly` (Task 2) is admitted-dead** ("reserved for future soft-fail; empty in v1"). Shipping an
  empty public `#[non_exhaustive]` type now is premature; `#[non_exhaustive]` already lets it be added
  later non-breaking. **Recommend dropping it from v1** — it adds a serde round-trip test and a re-export
  for zero behavior.
- The six-variant `CycleDelta` enum is justified by the issue scope, but `TagOutcome` and `Supersede` are
  the two thinnest (M4/M5 show Supersede isn't even fully wired). Consider whether v1 needs all six or
  whether `Supersede` defers to #578 alongside the synthesized-`AddFact` deferral — Task 9 step (d)
  *already* says "no synthesized `AddFact` in v1," and `Supersede` "only on exact `content_hash` collision
  with differing `t_created`," which is a narrow path. Dropping `Supersede` from v1 would eliminate M4+M5
  entirely. **Worth weighing against the wire-format-lock argument** (adding an enum variant later is
  non-breaking under `#[non_exhaustive]`, so deferring costs nothing).

---

## Feasibility / ordering (requirement #5)

The "tree compiles between tasks" claim holds **except Task 6**, by the plan's own admission: Task 2
creates the new `CycleReport` while the old one still exists in `types.rs`. Two same-named `CycleReport`
types cannot coexist unless the new ones live in `engine::cycle` (they do, per D4) and are NOT yet
re-exported at the name `CycleReport` until Task 6. Task 2 says "Do NOT delete the old `CycleReport` yet" —
fine — but Tasks 3/4/5 reference the **new** `CycleReport`/`CycleDelta` (e.g. Task 5's `apply_cycle_report`
returns `ApplyResult` and consumes the new `CycleReport`). As long as Tasks 3-5 import
`engine::cycle::CycleReport` by path (not the `lib.rs` re-export, which still points at the old one until
Task 6), they compile. **This is feasible but fragile** — the plan should state that pre-Task-6 code refers
to the new types by their `engine::cycle::` path, and only Task 6 flips the `lib.rs` re-export + deletes
the old type + updates `traits.rs`/`NoopCycle`/`async_engine`/the delegate test in one compile-unit. Task 6
is correctly scoped as the single breaking atom; just make the import-by-path discipline explicit for
Tasks 3-5.

Sequencing is otherwise sound: the MVP core (Task 5) ships before the DBSCAN producer (Tasks 8-9), and the
day-0 spikes (Task 1) retire the two under-specified semantics (units, bi-temporal quarantine) first.

---

## D2 deviation (already flagged by the plan)

D2 (`prior_reports: Vec<CycleMetadata>` instead of the issue's `Vec<CycleReport>`) is a reasonable
persistence-weight argument and the plan flags it for maintainer sign-off. I concur with the deviation —
persisting full delta logs as history would bloat the config-backed store, and retrieve-before-reflect
only needs metadata. **No objection; correctly surfaced.**

---

## Summary of required-before-implementation items

1. **H3 + D1** — settle which importance column `AdjustScore` reads/writes and its interaction with decay
   recomputation. (Highest-impact correctness gap.)
2. **M4 + M5** — Supersede is not wired coherently: lineage-vs-conflict table inconsistency *and* missing
   intra-batch forward-reference scheme. Either fully specify or defer Supersede to #578.
3. **H1** — thread `upcaster_registry` into the apply-tx event insert and pin the `OutcomeSignal` payload
   contract.
4. **H2** — decide TOCTOU: re-validate under the write lock (preferred) or document the window.
5. **M3** — resolve the `EmptyReport` variant question in the plan text.

Everything else is MEDIUM/LOW polish. The grounding is accurate, the breaking-change blast radius is
genuinely core-internal, and the lock-safety analysis (`promote_in_conn`, non-reentrant mutex, store-level
event insert) is correct and load-bearing-right. **Approve once H3/M4/M5/H1/H2/M3 are addressed.**

## Resolution

- [HIGH H3/D1] importance vs importance_score ambiguity → **Fixed**: D1 + Task 5 now specify `AdjustScore` adjusts the durable base `importance` (not the decayed materialized `importance_score`), with an active-fact guard and row-count guard.
- [BLOCKER/M4/M5] Supersede incoherently wired to `LineageStore` + unimplementable validation-time forward-ref → **Fixed**: D10 re-wires Supersede to a `"supersedes"` graph edge (`EdgeStore`); forward-ref `new_id` resolution moved to apply-time (after `AddFact` inserts).
- Structural completeness (Doc/Test/Verify present) → acknowledged, no change.
