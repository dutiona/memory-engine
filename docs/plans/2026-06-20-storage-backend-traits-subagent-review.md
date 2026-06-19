# Subagent Review — #629 `StorageBackend` trait family plan

**Reviewed:** `docs/plans/2026-06-20-storage-backend-traits.md`
**Reviewer stance:** fresh eyes, validated against the actual codebase at `/home/mroynard/dev/memory-engine/.worktrees/feat-629-storage-traits`.
**Verdict:** **Strong plan, NOT yet LGTM.** One [HIGH] correctness error in the D1 rationale (the `for_each` streaming claim is factually false), plus a small cluster of [MEDIUM]/[LOW] items. The object-safety strategy is sound. The error/relocation/feature-gate risk analysis is accurate. Fix D1's framing (the decision can stay, the justification cannot) and the rest is polish.

---

## 1. Object-safety strategy — SOUND ✓

The core technical bet is correct. I verified each load-bearing claim:

- **`async_trait` desugaring is object-safe.** `#[async_trait]` rewrites `async fn` into `fn(...) -> Pin<Box<dyn Future + Send + '_>>`. That return type is concrete and sized → object-safe. No GAT, no `impl Trait` in return position, no AFIT. `Arc<dyn StorageBackend>` will coerce. Correct.
- **Sync `capabilities()` under `#[async_trait]` (D7).** Confirmed: `#[async_trait]` passes non-`async` `fn`s through untouched, and a sync `fn capabilities(&self) -> BackendCapabilities` is object-safe on its own. The mixed sync/async vtable is fine. Correct.
- **No generic methods, no `Self`-by-value, no associated consts in the contract.** The whole surface in §"The contract" is `&self`, monomorphic args, `Result<concrete>`. Object-safe. The `&[f32]` / `&str` / `&FactFilter` args are all object-safe. Correct.
- **The blanket `impl<T> StorageBackend for T where T: <six parts>` does NOT cause coherence/overlap problems.** `StorageBackend` is a new local trait with zero other impls; the blanket impl is the _only_ impl. No overlap, no orphan issue (all traits local). The existing `traits.rs` already proves the `&dyn`/`Arc<dyn>` pattern works in this crate. Correct.
- **The non-vacuous negative control (P1 step 6)** is genuinely the highest-signal cheap check and is specified correctly (`async fn _g<F: FnMut()>(&self);` does break object-safety → the `&dyn` assert must then fail to compile). Keep it.

The "object safety fails → epic dead" risk is the right thing to retire first, and the P1 spike-before-surface sequencing retires it correctly. **No object-safety blocker found.**

One precision note (not a defect): edition is **2024 / rust-version 1.85** (verified `Cargo.toml:8-9`), not 2021. `async_trait` 0.1 supports edition 2024 fine. With 1.85, native RPITIT/AFIT _exists_ but is **not** `dyn`-compatible — so `async_trait` remains the correct choice for a `dyn`-dispatched port. The plan's reasoning holds; it just never states the edition, and a reader might wonder "why not native async fn in trait?" A one-line note ("native AFIT is not `dyn`-safe → `async_trait` required") would close that.

---

## 2. [HIGH] D1 — dropping `for_each` DOES lose a streaming caller; the rationale is factually wrong

> Plan D1: "Drop `for_each`; keep `list_all() -> Vec<T>`. … every `for_each` caller is a full-table walk already paired with a `list_all`. Zero capability loss."

**This is false.** The actual `for_each` callers (verified `src/inspect/dump.rs:51-66`) are the **constant-memory JSON dump** path. `for_each` exists _specifically_ to avoid materializing a `Vec`:

- `src/store/facts.rs:677` docstring: "deserialized, passed to the callback, and dropped before the next row is read. Suitable for streaming serialization of large databases."
- `dump.rs:26-31` docstring: "never holds more than one entity in memory per collection. Peak memory drops from O(total entities) to O(1), making this suitable for databases with 100K+ facts."
- `stream_for_each` (`dump.rs:83`) consumes `for_each` to write a JSON array one row at a time to a `BufWriter`.

`list_all() -> Vec<T>` is the **opposite** of this: it holds O(total) facts × embedding-dim × content in RAM at once. So D1 does not have "zero capability loss" — it trades the documented O(1)-peak dump for O(n)-peak. For a 100K-fact store with 768-dim embeddings, that is the difference the code was explicitly written to avoid.

**Why it is HIGH, not BLOCKER:** `dump.rs` is `pub(crate)` inspect code that today calls the _concrete_ stores directly (`FactStore::new(conn, …).for_each(cb)`), not through any trait — confirmed no `dyn FactGraph`/`dyn StorageBackend` references exist yet (grep: zero hits). So **#629 itself does not break the dump** — dropping `for_each` from the _trait_ leaves the concrete `for_each` methods in place, and `dump.rs` keeps compiling. The harm is deferred: the moment #630/#631 routes inspection through `Arc<dyn StorageBackend>` (which is the epic's whole point), the dump must either (a) call the concrete backend type directly, breaking the abstraction the epic exists to build, or (b) re-add a streaming primitive to the trait, reshaping the contract this PR is meant to freeze.

**Recommendation (the call is the user's):** Keep `list_all_*` (it's needed), but **do not drop the streaming capability from the trait — express it object-safely.** The current `for_each<F: FnMut>` is generic and _is_ object-unsafe, so it genuinely cannot go on the trait verbatim — D1 is right about that. The object-safe replacement is a boxed callback, which the codebase _already uses_: `stream_for_each`'s bound is `F: FnOnce(Box<dyn FnMut(T) -> Result<()> + '_>)`. So add, per streaming store:

```rust
async fn for_each_fact(&self, f: &mut dyn FnMut(Fact) -> Result<()>) -> Result<()>;
```

`&mut dyn FnMut` is object-safe (no generic on the method). This preserves O(1)-peak dump through the port and costs ~6 methods. If the user prefers to defer streaming to a follow-up issue, that is a legitimate scope call — but it must be a _stated, filed_ deferral (a `type:enhancement` / `area:storage` issue "expose object-safe streaming reads on the port"), **not** the current silent "zero capability loss" claim, which would let a reviewer approve a real regression. Per CLAUDE.md: surface it, recommend the fix, leave the call to the user.

At minimum: **rewrite the D1 rationale** to say what is true — "`for_each` is object-unsafe (generic `F`); its sole consumer is the O(1) streaming dump; A1 ships `list_all` only and [defers | replaces-with-boxed-callback] the streaming primitive" — so the trade is visible.

---

## 3. Risk analysis (error ripple / relocation / feature gates / blanket impl)

### 3a. [LOW] `MemoryError::Storage` ripple — accurate, one nuance

Verified `memory-engine-mcp/src/error.rs:69` has the `other => ErrorData::internal_error(...)` wildcard. So the new variant **compiles** across the workspace without the typed arm — correct. The plan's P2 step 3 (add the typed arm above `other =>` + `storage_maps_to_internal` test) is the right "no silent degradation" move and matches the existing `Migration`/`Archive` arm style.

Nuance the plan should note: the **CLI** (`memory-engine-cli`) and **embed** (`memory-engine-embed`) crates also consume `MemoryError`. The plan's gate runs `--workspace`, so a non-wildcard `match` there would be caught — but the plan only calls out the MCP arm explicitly. Quick check worth adding to P2: `grep -rn "match.*MemoryError\|MemoryError::" memory-engine-cli/ memory-engine-embed/` to confirm neither does an exhaustive non-wildcard match. (Given `#[non_exhaustive]` on `MemoryError`, an exhaustive match is impossible downstream anyway — the compiler forces a wildcard — so this is belt-and-suspenders, hence LOW.)

### 3b. [LOW] Type relocation — safe; verified no impls block the move

Verified all three are plain data structs with **no inherent or trait impls** at their definition sites:

- `EventFilter` (`store/events.rs:8-20`): `#[derive(Debug, Clone, Default)]`, fields only.
- `FactScoringRow` (`store/facts.rs:42`): `#[derive(Debug, Clone)]`, scalar fields only.
- `SessionFact` (`store/facts.rs:~1152`): `#[derive(Debug, Clone)]`, single `id` field.

The `pub use` shim approach is behavior-neutral and will work; the documented in-place fallback is unnecessary for these three (good to keep as a guard, but it won't fire). **One caveat:** `EventFilter` is `pub` and re-exported? Check whether it currently reaches the crate root. If `dump.rs`/inspect or the CLI names `EventFilter` via a `store::` path, the shim covers it; if anything names it via a _flat_ root re-export, confirm `types::*` re-export keeps the same root path. Low risk — the shim + `pub use types::*` should preserve both paths — but worth a `grep -rn "EventFilter" --include=*.rs` to enumerate call sites before moving (the plan asserts "1178 tests green proves it" which is true _post-hoc_ but a pre-move grep is cheaper than a failed gate).

### 3c. [LOW] Feature-gate (archive/ann/async) — correctly handled

- **D4 (ColdStorage separate, not a supertrait)** is the right call and the stated reason is correct: a `#[cfg(feature="archive")]` supertrait bound would make `dyn StorageBackend`'s vtable shape feature-dependent → two different types under `--features archive`. Keeping it a separate `Option<Arc<dyn ColdStorage>>` keeps the umbrella feature-invariant. Verified the manifest types (`ArchiveManifestEntry` in `crate::archive::types`) are themselves `archive`-gated, so `ColdStorage`'s signature referencing them is consistent — no leak of a gated type into the always-present umbrella.
- **`ann`** is correctly kept impl-internal (`SearchIndex` returns `Vec<i64>`; brute-force-vs-HNSW never surfaces). Correct.
- **`async`** — the plan's claim "nothing `.await`s; `async` feature not required to build/test A1" is correct: `async_trait` is a _proc-macro_ dep, independent of the crate's `async` feature flag (which gates `tokio`, verified `Cargo.toml:51 async = ["dep:tokio"]`). The trait methods are `async fn` at the _type_ level but A1 never executes them, so no runtime is needed. Sound.

### 3d. [LOW] insert_archive_manifest 10-arg — verified

`store/archive_manifest.rs:27-39`: exactly 10 args, already carries `#[allow(clippy::too_many_arguments)]`. Plan's "10-arg" + "file a collateral `type:refactor` for a `NewArchiveManifest` struct" is accurate and correctly scoped out of #629.

---

## 4. Full-surface method names — VERIFIED grounded (initial false alarm cleared)

I spot-checked the plan's method list against the real store and initially flagged ~8 names as missing (`list_active_facts_scoring`, `mark_facts_dream_cycled`, `stamp_facts_surfaced`, …). **They are not missing** — they are the plan's documented "entity-suffixed" transform of the real names:

| Plan name                                  | Real store method (`facts.rs`)       |
| ------------------------------------------ | ------------------------------------ |
| `list_active_facts_scoring`                | `list_active_scoring`                |
| `list_active_facts_at`                     | `list_active_at`                     |
| `mark_facts_dream_cycled`                  | `mark_dream_cycled`                  |
| `stamp_facts_surfaced`                     | `stamp_surfaced`                     |
| `list_active_facts_by_metadata_key_recent` | `list_active_by_metadata_key_recent` |
| `list_undreamt_facts_in_period`            | `list_undreamt_in_period`            |
| `list_facts_by_scopes_recent`              | `list_by_scopes_recent`              |

The full `FactStore` surface (31 pub fns) maps cleanly. The surface is real and the 1:1 transcription claim holds. **[LOW] suggestion:** the P5 coverage checklist should include the _real_ (un-suffixed) name alongside the trait name in each row, so the grep-able audit checks `mark_dream_cycled → mark_facts_dream_cycled` rather than asserting against a name that doesn't exist in `store/`. Otherwise a reviewer doing `grep "fn mark_facts_dream_cycled" src/store/` gets zero hits and a false "missing" finding — exactly the trap I fell into.

---

## 5. Over-engineering assessment

### 5a. [LOW] Full 90-method surface in a "traits only" PR — JUSTIFIED, with a guard

This is the plan's most debatable scope call, and it gets it right. The alternative (skeleton now, widen later) would force a **trait reshape mid-epic**, which is exactly the "we cannot get this wrong" failure the user named. A trait contract is a published surface; widening it after #630/#631 build against it is a breaking churn. Transcribing the full surveyed surface up front, _after_ the 2-method spike proves dyn-safety (P1), is the correct risk ordering. The mechanical P5 transcription is low-risk _because_ P1 already retired the hard part.

The one guard I'd add: the surface is "every `pub`/`pub(crate)` store method." Some of those may be **internal plumbing the engine drives, not port operations** (the plan already excludes `open_connection*`, `init_schema`, `get/set/list_config` per D5 — good). Worth a second pass asking "does the _engine_ call this, or only the store's own internals?" for borderline cases like `insert_lineage_raw` (vs `insert_lineage`) — if `_raw` is a within-store helper, it doesn't belong on the port. Cheap to check during P5; flag any that are store-private in the coverage checklist as "stays backend-private."

### 5b. [LOW] One-file-per-trait — fine, mild over-structuring

Ten files for ten traits + filter/capabilities/backend is more granular than `traits.rs` (one file, 7 consumer traits). For a port this size (~90 methods across 7 bounded contexts) the split is defensible — each bounded context is genuinely separable and the files map to the survey's store groupings. Not over-engineering, but note the asymmetry with the existing `traits.rs` convention; if the user prefers consistency, `graph.rs`+`event_log.rs`+`search_index.rs`+`consolidation.rs`+`session.rs`+`schema.rs`+`cold_storage.rs` could collapse to fewer files. Low-stakes; the plan's choice is fine.

### 5c. Not over-engineered: the blanket impl, the closed `FactFilter` (D6), driver-opaque `StorageError`. These are all minimal-mechanism choices. Good.

---

## 6. Structural completeness — Documentation / Testing / Verification

All three sections **present and adequate**:

- **Documentation** — good. Rustdoc density matching `traits.rs`, the consolidated `# Errors` paragraph (correctly noting `missing_errors_doc` is pedantic=warn, avoiding ~90 copy-paste stanzas), `crate-layout.md` row, ADR correctly deferred to #640. One gap: it should add a line documenting the **streaming decision** (per finding #2) — readers of `event_log.rs` will wonder where `for_each` went.
- **Testing** — strong. Two-tier (compile-tests + concrete-type unit tests) is exactly right for a traits-only PR. Feature matrix (default / `archive` / `--all-features`) covers the `ColdStorage` cfg'd assert. The explicit "NOT here (deferred)" list is good discipline. The non-vacuous control is the standout.
- **Verification** — thorough and matches CLAUDE.md's workspace gate (touches `error.rs` + public API → full `--workspace` + `--all-features` build/test/clippy + `fmt --check` + `doc`). The in-lane diff-stat scope check is a nice MVP-discipline guard. The `fmt` note ("trust cargo fmt not bare rustfmt") correctly reflects the worktree edition-2024 gotcha.

**[LOW] one verification gap:** the gate asserts "test count strictly > 1178" but doesn't pin where 1178 comes from or run a baseline capture first. If `main` moved (the plan itself says rebase on `#641/#642`), 1178 may be stale. Recommend: capture the _actual_ baseline count on the rebased `main` as step 0, then assert `> baseline`, rather than hardcoding 1178.

---

## 7. Feasibility / sequencing

**P1 → P5 ordering is correct and the strongest part of the plan.** Spike (2-method dyn-safety + dep lock) → error wiring → value types → relocation → full surface → re-exports. Each phase ends green; the load-bearing risk (object-safety) is retired on a skeleton before the ~90-method transcription. The TDD framing ("the compiler _is_ the test; `_assert_obj_safe` red→green is the loop") is accurate for a traits PR. Achievable in the stated order.

One sequencing nit **[LOW]:** P4 (relocation) is independent of P1–P3 and P5 — it could run first (it's pure refactor, lowest risk, and de-risks the `types.rs` import paths the traits will use). Moving it earlier means the trait files in P5 can `use crate::types::EventFilter` from the start instead of a `store::` path that later changes. Minor; the current order works.

---

## Summary table

| #   | Severity | Finding                                                                                                                                                                                                                                                                                                                                                                                         |
| --- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2   | **HIGH** | D1's "zero capability loss" is false — `for_each` is the O(1)-peak streaming dump primitive (`dump.rs`/`facts.rs:677`), not a `list_all` duplicate. Dropping it from the trait silently regresses the 100K-fact dump path the moment #630/#631 route through `dyn`. Rewrite the rationale; either add an object-safe `for_each_*(&mut dyn FnMut)` or file an explicit streaming-deferral issue. |
| 1   | SOUND    | Object-safety strategy (`async_trait` + `&dyn`/`Arc<dyn>` asserts, sync `capabilities`, blanket impl) is correct for every proposed trait shape. No blocker.                                                                                                                                                                                                                                    |
| 3a  | LOW      | MCP ripple analysis correct (wildcard at `error.rs:69` verified); add a CLI/embed exhaustive-match grep to P2.                                                                                                                                                                                                                                                                                  |
| 3b  | LOW      | Relocation safe — all 3 types are impl-free plain structs; pre-move `grep EventFilter` is cheaper than the post-hoc 1178 assert.                                                                                                                                                                                                                                                                |
| 4   | LOW      | Full surface is grounded; method names are the entity-suffixed transform of real store fns. Put the real name in the P5 coverage checklist to avoid false "missing" findings.                                                                                                                                                                                                                   |
| 5a  | LOW      | Full 90-method scope is justified (avoids mid-epic reshape); add a "does the engine call this, or store-internal?" pass for borderline methods like `insert_lineage_raw`.                                                                                                                                                                                                                       |
| 6   | LOW      | Doc/Test/Verify sections adequate; document the `for_each` decision; capture the real test baseline instead of hardcoding 1178.                                                                                                                                                                                                                                                                 |
| 7   | LOW      | Consider running P4 (relocation) first; it's the lowest-risk, dependency-free phase.                                                                                                                                                                                                                                                                                                            |

**Bottom line:** the architecture, error model, feature-gating, and object-safety reasoning are correct and well-grounded in the actual code — this is a careful plan. The single substantive issue is the D1 `for_each` rationale, which states a falsehood ("zero capability loss") about a primitive that exists precisely to bound dump memory to O(1). The _decision_ (don't put generic `for_each` on the trait) is defensible; the _justification_ is not, and as written it would let a reviewer wave through a real, deferred regression. Fix that framing — and the call on whether to add an object-safe streaming method or file a follow-up — and the plan is ready.

## Resolution

- **[HIGH] D1 "drop for_each → list_all" loses O(1) streaming (factually false rationale).** ACCEPTED & FIXED. Reversed the decision: D1 now keeps an object-safe streaming method `async fn for_each_X(&self, f: &mut (dyn FnMut(X) -> Result<()> + Send)) -> Result<()>` for every store that has `for_each` today (facts, edges, scopes, events, summaries, lineage). `&mut dyn FnMut` is object-safe (trait object, not generic); `+ Send` keeps the `#[async_trait]` boxed future Send. `list_all_X` kept only where it exists today (facts/edges/scopes/summaries) — NOT invented for events/lineage. Updated: D1 row, uniform-transforms note, FactGraph/EventLog/ConsolidationStore method lists, coverage-checklist wording, streaming doc-note requirement. Flagged as a Codex review focus (the Send-bound-under-async_trait nuance).
- **[LOW] Baseline test count hardcoded at 1178.** ACCEPTED & FIXED. Verification + green-criteria now say "re-capture baseline AFTER rebase (was 1178)" rather than asserting 1178.
- **[LOW] Method names grounded once entity-suffix transform accounted for.** NOTED — no change needed; the transform is documented in the uniform-transforms note.
- **Positive confirmations (no action, recorded as de-risking):** object-safety strategy sound (`async_trait` → `Pin<Box<dyn Future>>`, sync `capabilities()` passes through); blanket `impl<T> StorageBackend for T` has zero coherence risk (only impl of a new local trait); `Arc<dyn StorageBackend>` will coerce; MCP wildcard arm at `error.rs:69` confirmed; the 3 relocation types are impl-free plain structs (clean move); ColdStorage-not-supertrait keeps the umbrella feature-invariant.

Advisor review (Step 3a): `advisor()` tool unavailable in this environment — substituted by this clean-slate subagent review + the Codex/agy multi-model loop (Step 4), per the skill's "skip to a one-shot review workflow" guidance. No advisor artifact fabricated.
