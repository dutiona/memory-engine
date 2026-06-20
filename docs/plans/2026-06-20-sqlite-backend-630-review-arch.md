# Architecture / Scope / API-Seam Review — Plan #630 `SqliteBackend`

> Reviewer: clean-slate, architecture/scope/seam lens, adversarial.
> Plan under review: `docs/plans/2026-06-20-sqlite-backend-630.md`.
> Verdict: **APPROVE with fixes.** The seam is drawn in the right place, delegation is the correct call, and #630 stays in its lane. No BLOCKERs. A handful of HIGH/MEDIUM points sharpen claims that are currently overstated or under-specified, plus one genuine seam-leakage gap in the verification gate.

---

## Summary table

| #   | Severity | Area                    | One line                                                                                                                                                                                                                                                                                                                              |
| --- | -------- | ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F1  | HIGH     | #631 set-up (oversell)  | "Four-fields-to-one swap" understates #631 — the engine reads `pool`/`graph`/`hnsw` directly in dozens of non-storage call sites; #630 cannot make #631 mechanical and should stop claiming it.                                                                                                                                       |
| F2  | HIGH     | Seam leakage (gate gap) | The grep gate (`§7`) forbids `rusqlite`/`Connection` in `pub` signatures, but every `block_*`/`for_each_streamed`/`scan` bound names `rusqlite::Connection`. Those are private, but the gate as written has no allowlist mechanism beyond prose — specify the exact grep so it does not false-positive and get silently weakened.     |
| F3  | MEDIUM   | Delegation two-layer    | Plan does not acknowledge the real downside of delegation: the parity oracle and the SUT share the same SQL, so H1/H2 parity tests are partly tautological (identity by construction). Name it and state what they actually catch (the _wrapper_, not the SQL).                                                                       |
| F4  | MEDIUM   | D4 error mapping        | `map_seam_err` only remaps `MemoryError::Database`. Verified correct against `error.rs:393-395`, but the FTS path never yields `Database` (it swallows to `Ok(empty)`), so the "forced raw SQL fail ⇒ `Storage(Backend)`" D4 witness (T1) must come from a _non-FTS_ method — call that out or the witness is vacuous.                |
| F5  | MEDIUM   | Scope creep (T9)        | T9 (`Arc<dyn StorageBackend>` realization) is correctly in #630's lane, not gold-plating — but its framing as "the value-level upgrade of #629's vtable test" is right only if it does NOT pull forward the #632 conformance battery. The plan says this; keep T9 to one call-per-trait smoke, no semantic assertions.                |
| F6  | MEDIUM   | ColdStorage gating      | `#[cfg(all(async, archive))]` on the impl is correct, but the plan never states the load-bearing reason it is _safe_: `ColdStorage` is not a `StorageBackend` supertrait bound, so gating the impl out cannot change the umbrella's vtable. Make the invariant explicit so a future contributor does not "promote" it into the bound. |
| F7  | LOW      | D3 HNSW ownership       | Owning HNSW in #630 is the right call (defensible), but the decision rationale conflates "where it lives" with "when it is built"; D3 should note that `notify_*` hooks are _new_ call sites with no existing oracle — H9 is the only net-new-behavior test and deserves that label.                                                  |
| F8  | LOW      | Collateral §9           | §9 item 3 ("note on #631 issue") is the one collateral that is NOT a separate issue — it is a comment on a sibling issue. That is fine per the one-concern rule, but the plan should not list it alongside "separate issues" without distinguishing it.                                                                               |

---

## What checks out (confirmed against source)

These claims I verified against the tree and they hold — calling them out so the author knows they are load-bearing-and-correct, not unexamined:

- **Engine holds the pool by value** (`engine/mod.rs:157` → `pub(crate) pool: ConnectionPool`), and the four sibling fields the struct absorbs all exist: `pool`, `embed_dim` (`:158`), `upcaster_registry` (`:167`), `hnsw_strategy` (`:163-164`, `#[cfg(feature="ann")]`). The plan's `SqliteBackend` field list (plan §2, lines 57-63) is an accurate mirror. The `Arc<ConnectionPool>` change (owned handle for `'static` closures) is justified — `spawn_blocking` needs `'static`, and the pool is not `Clone`.
- **`try_write` returns `MemoryError::ReadOnly` on a read-only pool** (`connection_pool.rs:271-273`) — so D-decision "`try_write` not `write` preserves DD#6 for free" (plan §3, line 119) is real. `write()` (`:264`) bypasses the guard; the plan correctly mandates `try_write` for every write method.
- **`pool.read()` returns `Result<ReadConn>` yielding `MemoryError::Pool` on timeout** (`:225,250`), never `Database`. So `map_seam_err`'s pass-through of `Pool` (plan §3, line 84-88) is correct: a pool-acquire failure stays `Pool`, only raw driver `Database` becomes `Storage(Backend)`.
- **FTS5 malformed-query swallow** is real and per-call: `fts_search` catches at `query_map` and returns `Ok(vec![])` (`fts.rs:65-71`); `fts_count_expired` mirrors it (`fts.rs:90` doc + body). So "malformed ⇒ empty / Ok(0)" (plan H1) is preserved purely by delegation. **But see F4** — this means the FTS path cannot be the D4 `Database→Storage(Backend)` witness.
- **`Some(&[]) = matches-nothing` scope quirk** is preserved by passing the slice straight to `serialize_scope_ids` (`search/mod.rs:32-39`): `Some(&[])` → `Some("[]")` → `scope_id IN (json_each('[]'))` matches nothing; `None` → `None` → filter skipped. The plan's convert.rs claim (plan §3, line 142) is exact. ✅
- **`lexical_count_expired` already drops `FactFilter`** in the #629 trait (`search_index.rs:59-64` takes `(query, fact_type, scope_ids)`), matching `fts_count_expired`'s free-fn shape (`fts.rs:95-100`). So H1's "count_expired not taking a filter" is a non-issue at the trait level — the trait was already shaped right in #629. The plan inherits this correctly.
- **`StorageError::Backend(String)` is the designed opaque sink** (`error.rs:361-368`), and `MemoryError::Database(#[from] rusqlite::Error)` is explicitly reserved for "SQLite-internal use" (`error.rs:355`). D4's remap target is exactly right.
- **`vector_search` returns `EmbeddingDimension` on wrong-length input** (`vector.rs:76-81`), matching the #629 `SearchIndex::vector_search` doc contract (`search_index.rs:40-44`). H1's "wrong-dim ⇒ EmbeddingDimension" is preserved by delegation. ✅
- **`ColdStorage` is a separate `Option<Arc<dyn ColdStorage>>`, NOT a supertrait bound** (`cold_storage.rs:4-9`, `backend.rs:49-51`). The umbrella (`backend.rs:52-55`) is genuinely feature-invariant. The plan's `#[cfg(all(async,archive))]` treatment is structurally sound — **see F6** for the missing rationale.
- **#629 explicitly defers the full-value realization to #630** (`backend.rs:86-90`: "constructing a full `Arc<dyn StorageBackend>` value … is #630's `SqliteBackend`"). So T9 is in-scope by #629's own design, not gold-plating. ✅

---

## Findings (detail)

### F1 — [HIGH] "Four-fields-to-one swap" oversells #631; #630 cannot make #631 mechanical

**Plan:** §2 line 54 ("so #631 is a four-fields-to-one swap"); §1 D-table and H5 reinforce a "mechanical #631" framing.

**Problem.** The engine does not access persistence _only_ through the four named fields as opaque handles. The `MemoryEngine` struct (`engine/mod.rs:156-168`) also holds `graph: RwLock<MemoryGraph>`, `scope_tree: RwLock<ScopeTree>`, `vector_strategy: Box<dyn VectorSearchStrategy>`, and `hnsw_strategy` — and the engine's sub-modules (`ingest`, `query`, `consolidation`, `forgetting`, `inspect`, …, 20+ modules listed at `engine/mod.rs:21-40`) call `self.pool.read()` / `self.pool.try_write()` directly and compose multi-store transactions over a single `&Connection` (the H5 hazard, which the plan _does_ acknowledge separately). Swapping four fields to one `Arc<dyn StorageBackend>` does not rewire those call sites — every `self.pool.write()` + inline `FactStore::new(&conn).insert(...)` becomes `self.backend.insert_fact(...).await`, across the whole engine, and the transaction-composing ones (`add_fact`, `consolidate`) cannot be swapped at all without a seam-level transaction primitive (correctly flagged as collateral §9 item 3).

This is not a #630 defect — #630 is right to _not_ touch the engine. The defect is the **claim**: #630 sets up the _field_ for #631 but does not make #631 mechanical, and asserting it does will mislead the #631 planner into under-scoping.

**Fix.** Soften §2 line 54 to: "#631 replaces the four fields with one `Arc<dyn SqliteBackend-as-StorageBackend>` **handle**; the call-site rewrite (every `self.pool.*` + inline-store call → an `await` on the handle) and the multi-store-transaction reshaping (§9 item 3) are #631's substantive work — #630 only makes the handle constructible." Keep H5's `git diff --stat src/engine/` empty-gate; it is the right guard.

---

### F2 — [HIGH] The seam-leakage grep gate has no teeth as written

**Plan:** §7 pass-criteria ("no `rusqlite`/`Connection` symbol in any `pub` signature under `storage/` … except inside the private `block_*` helper bounds (grep check)").

**Problem.** The design _does_ confine `rusqlite::Connection` correctly: it appears only in the `where F: FnOnce(&rusqlite::Connection) -> Result<T>` bounds of the private `block_read`/`block_write`/`for_each_streamed`/`scan` items (plan §3 lines 94, 105, 129). None of those are `pub`. **But** the verification is specified as prose ("grep check") with a prose exception ("except inside the private `block_*` helper bounds"). A literal `grep -r 'rusqlite\|Connection' src/storage/sqlite/` will hit those private bounds and either (a) trip a naive reviewer into thinking the seam leaks, or (b) get "fixed" by loosening the grep to the point it stops catching a _real_ `pub fn foo(&self, c: &Connection)` regression. The gate's value is entirely in its precision, and the plan does not pin the precise command.

**Fix.** Replace the prose with the exact gate, e.g.:

```bash
# A pub trait-method or pub fn must never name a driver type.
# Allowed: rusqlite in private `fn block_*` / `scan` closure bounds only.
! grep -rnE '^\s*(pub(\([^)]*\))?\s+)?async\s+fn .*\b(rusqlite|Connection)\b' src/storage/sqlite/
grep -rn 'rusqlite::Connection' src/storage/sqlite/ | grep -vE 'fn (block_read|block_write|for_each_streamed)|where|FnOnce'  # expect: empty
```

State that the second grep returning non-empty is a hard fail. Without a pinned command the "confinement to private helpers" claim is unverifiable and will rot exactly the way `storage/mod.rs:8` warns seams rot.

---

### F3 — [MEDIUM] Delegation's real downside (tautological parity) is unacknowledged

**Plan:** §1 "Why delegation, not absorption" (lines 15); §4 hazard register; §10 testing ("identity holds because the SQL is reused").

**Assessment of the architectural call:** delegation is **correct** for the epic. #634's `PgBackend` reuses zero SQLite SQL (spec §7 line 339-341: "shared-SQL approach would have been wrong"), so inlining the SQL into `impl FactGraph for SqliteBackend` would only have to be undone, and would fork the SQL away from the existing fast in-process unit tests that exercise `src/store/*` directly. The two-layer structure (`storage/sqlite/graph.rs` adapter → `store/facts.rs` SQL) is the right shape and #634 mirrors it cleanly as `storage/postgres/graph.rs`. No change to the decision.

**The unacknowledged downside:** because the parity oracle (`FactStore::m`) and the system-under-test (`SqliteBackend::m` → `FactStore::m`) call the **same** SQL, the H1/H2 parity tests assert `f(x) == f(x)` — they are identity-by-construction for everything _below_ the wrapper. They genuinely catch wrapper-layer drift (a `convert.rs` mis-projection, a wrong conn selection, a `block_read` vs `block_write` swap, a forgotten `.await`, a `map_seam_err` mistake), which is the real risk surface. But the plan's framing ("per-dimension parity vs `fts_search`", H1) reads as if it proves the SQL correct, which it cannot — the SQL is the same object. A green parity suite where the wrapper is a perfect pass-through is _expected_, not _informative_, for the SQL.

**Fix.** Add one sentence to §4 / §10: "Parity tests are identity-by-construction below the wrapper (same SQL on both sides); their job is to catch **wrapper** defects — conn-selection, projection, async-bridge, error-remap — not to (re)validate the delegated SQL, which the existing `store/*` unit tests own." This reframes what 'green' means and stops H1 from over-claiming.

---

### F4 — [MEDIUM] The D4 witness (T1) cannot come from the FTS path

**Plan:** T1 ("forced raw SQL fail ⇒ `Storage(Backend)` (D4 witness)"); H4 ("forced raw SQL fail ⇒ `Storage(Backend)`").

**Problem.** `map_seam_err` remaps only `MemoryError::Database` (plan §3 line 85). Verified: that variant is `Database(#[from] rusqlite::Error)` (`error.rs:393-395`). But the two FTS methods **never produce `Database`** — they catch the rusqlite error and return `Ok(vec![])` / `Ok(0)` (`fts.rs:65-71`). So you cannot witness D4 by forcing an FTS failure; it will silently succeed-empty. The witness must force a `Database` from a method that _propagates_ it — e.g. `vector_search` (propagates via `?` at `vector.rs:91,97`), or a `FactStore` write against a corrupted/locked DB, or `get_fact` on a malformed row. T1 implements only `EventLog::{insert_event, get_event, for_each_event}` — so the T1 D4 witness must be an `EventLog` method whose body can surface a real `rusqlite::Error` (a `get_event` on a row that fails column extraction, or an `insert_event` against a constraint violation), not a query-swallow path.

**Fix.** In T1, name the concrete D4 witness method and the forced failure (e.g. "force `insert_event` to hit a UNIQUE/constraint `rusqlite::Error` ⇒ assert `MemoryError::Storage(StorageError::Backend(_))`"). Add a note that FTS-swallow methods are _not_ valid D4 witnesses (they prove the empty-result contract instead).

---

### F5 — [MEDIUM] T9 is in-scope, but guard its blast radius

**Plan:** T9 ("`Arc<dyn StorageBackend>` realization proof … one method per bounded trait through it").

**Assessment.** Correctly in #630's lane — #629 explicitly deferred the full-value vtable realization here (`backend.rs:86-90`). It is **not** gold-plating: it is the minimal closure of #629's deliberately-partial callability test (which only proved a 3-method `Dummy SearchIndex` through `&dyn`, `backend.rs:97-139`). Building a real `Arc<dyn StorageBackend>` from `SqliteBackend` and dispatching one method per bounded trait through it is the value-level proof that the blanket impl (`backend.rs:60-63`) actually closes over a real type.

**The risk:** the line between "one smoke call per trait through the `dyn`" (T9, in scope) and "assert the semantics of each call" (#632 conformance, out of scope) is thin, and the testing section's appetite for parity tables could leak the #632 battery into T9. The plan says the right thing (T9 = "drive one method per bounded trait", #632 = the conformance suite) — but it is one editing pass away from over-building.

**Fix.** Add an explicit non-goal to T9: "T9 asserts _dispatchability_ (the call returns `Ok`/the right variant through `&dyn`), NOT semantic parity — semantic parity per method already lives in T1-T8's per-trait tests; the _cross-backend_ battery is #632. T9 is ~7 calls, no fixtures beyond an empty DB."

---

### F6 — [MEDIUM] ColdStorage gating is correct but the safety invariant is unstated

**Plan:** §2 layout (`cold_storage.rs #[cfg(all(feature="async", feature="archive"))]`); T7; §1 D-table omits a ColdStorage decision.

**Assessment.** The gating is structurally right. `ColdStorage` is held by the engine as a separate `Option<Arc<dyn ColdStorage>>` and is explicitly **not** a `StorageBackend` supertrait bound (`cold_storage.rs:4-9`; `backend.rs:49-51`). Therefore cfg-gating the `impl ColdStorage for SqliteBackend` in or out **cannot** change the `StorageBackend` vtable — the umbrella stays feature-invariant (`backend.rs:52-55` has no `ColdStorage` in the bound list). The `all(async, archive)` conjunction is correct because the impl needs _both_ `spawn_blocking` (async) _and_ the archive types.

**The gap.** The plan treats this as obvious, but it is exactly the kind of invariant a future contributor breaks by "tidying up" — adding `+ ColdStorage` to the umbrella bound under `#[cfg(archive)]`, which would make `Arc<dyn StorageBackend>`'s type feature-dependent and silently break #631/#634's single-handle assumption. The plan's own module doc (`storage/mod.rs:18-20`, `backend.rs:49-51`) calls this out for the _trait_; the #630 plan should restate it for the _impl_.

**Fix.** Add to T7 / §8 module-rustdoc bullet: "`impl ColdStorage` is gated `all(async, archive)` and lives off the `StorageBackend` supertrait set _by design_ — the umbrella vtable MUST stay feature-invariant (`backend.rs:49-51`). Never fold `ColdStorage` into the umbrella bound." This is a one-line invariant that prevents a whole class of #634 breakage.

---

### F7 — [LOW] D3 conflates HNSW _placement_ with HNSW _maintenance_, hiding the one net-new-behavior surface

**Plan:** §1 D3; T8; H9.

**Assessment.** Owning HNSW in `SqliteBackend` (vs deferring to #631) is **defensible and probably right**: the index-maintenance hooks (`notify_insert`/`notify_expire`) fire from the _write_ methods, which live in the backend after #630; deferring would split write-and-index across the seam and reintroduce an O(N) brute-force regression #631 could not fix without reopening the seam (plan's D3 rationale). #629 already declared these hooks impl-private (`backend.rs:21-23`: "HNSW index-maintenance hooks … stay impl-private — SQLite-ann internals, not a port contract"). So #630 owning them is consistent with the established seam.

**The subtlety the plan buries.** Everything else in #630 is a _pure pass-through_ with an identity oracle (F3). The HNSW `notify_*` wiring is the **one place #630 introduces genuinely new call sequencing** — the write methods must call `notify_insert`/`notify_expire` in the right order, and there is no existing engine call site doing exactly this (today the engine maintains `hnsw_strategy` itself, `engine/mod.rs:163-164`). So H9 ("brute≡HNSW recall; insert/expire reflected") is not a parity-with-an-oracle test like the others — it is the genuine _new-behavior_ test, and the highest-value one in the whole plan. The plan ranks it H9 (near-bottom) and labels it "perf cliff", underselling it.

**Fix.** Re-label H9 as "HNSW maintenance — the one net-new write-path behavior in #630 (no pre-existing oracle)" and note T8 carries the only correctness risk not covered by identity-parity. Consider moving T8 earlier or at least flagging it as not-mechanical. (Severity LOW because the plan _does_ test it; the issue is mis-prioritization, not omission.)

---

### F8 — [LOW] §9 collateral list mixes a sibling-issue comment in with separate issues

**Plan:** §9 ("Collateral / follow-ups (separate issues …)"); item 3 is "note on the #631 issue"; item 4 is "already tracked, #652-style".

**Assessment.** The repo convention (CLAUDE.md "Collateral issues": one logical concern per issue, separate `type:*`+`area:*`, linked via `addSubIssue`) is satisfied by items 1 and 2 (genuine new separate issues: `type:docs`/`area:storage` and `type:enhancement`/`area:retrieval` — both correctly labeled). Item 3 is NOT a new issue — it is a comment on the existing #631 — and item 4 is a pointer to existing #652. Listing them under a header that says "separate issues" is mildly inconsistent: a reader checking the one-concern rule will expect four `gh issue create` calls and find two.

**Fix.** Re-title §9 "Collateral / follow-ups" and tag each: items 1-2 `[NEW ISSUE]`, item 3 `[COMMENT on #631]`, item 4 `[EXISTING #652 — link only]`. No behavior change; just truth-in-labeling so T12 ("Collateral issues filed") does not over- or under-create.

---

## Scope verdict (explicit answers to the brief)

- **Seam leakage (Q1):** No `pub` signature leaks `rusqlite`/`Connection`. Confinement to private `block_*`/`scan` bounds is achievable as designed and verified against the #629 trait surface (no driver types in `graph.rs`/`search_index.rs`/`cold_storage.rs`). `SqliteBackend` itself being `pub` is **correct** — #631 and #632 must name the type to construct it; the _trait methods_ are the seam, not the struct. The only weakness is the **gate's precision (F2)**, not the design.
- **Delegation vs absorption (Q2):** Delegation is the right architectural call for the epic (#634 reuses zero SQLite SQL). The two-layer structure is justified and #634-symmetric. The one unacknowledged downside is **tautological parity (F3)** — a framing fix, not a design fix.
- **Scope creep / under-scope (Q3):** No task reaches into #631/#632/#634 territory. T9 is in-scope (not gold-plating) per #629's explicit deferral; guard its blast radius (F5). D3 HNSW-ownership is correctly #630's job (F7).
- **#631 set-up (Q4):** The plan **oversells** "#631 mechanical" (F1). #630 makes the handle _constructible_; the call-site rewrite and transaction reshaping are #631's real work. The H5 empty-diff gate is the correct #630-side guard.
- **ColdStorage gating (Q5):** Correct; `all(async, archive)` and off-the-umbrella-bound is right. Make the feature-invariance invariant explicit (F6).
- **Structural completeness + feasibility (Q6):** Documentation (§8), Verification (§7), Testing (§10) all present. Task ordering is feasible — no task depends on a later one (seam core T1 before all impls; HNSW T8 after the write methods it hooks; T9 after the traits it realizes; docs/PR last). Collateral handling is convention-consistent modulo the labeling nit (F8).

**Bottom line:** the seam is in the right place and #630 stays in its lane. Land it after fixing F1 (claim), F2 (gate), F4 (witness) — the three that would otherwise let a real regression or a misleading hand-off through. F3/F5/F6 are framing/invariant hardening; F7/F8 are polish.
