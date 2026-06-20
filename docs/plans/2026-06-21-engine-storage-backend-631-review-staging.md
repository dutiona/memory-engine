# Review — #631 engine→`Arc<dyn StorageBackend>` plan (STAGING / FEASIBILITY lens)

Reviewer: clean-slate adversarial, no prior context. Verified every load-bearing claim against
source in `.worktrees/refactor-631-engine-storage-backend`. Scope of this lens: is the big-bang
claim true, are Stages A–D additive-green, is the cutover PR safely landable, are the A–F boundaries
right, is the plan structurally complete.

Plan under review: `docs/plans/2026-06-21-engine-storage-backend-631.md`
Design it builds on: `docs/plans/2026-06-21-engine-storage-backend-design-synthesis.md`

---

## Verdict

**APPROVE WITH ONE REQUIRED ADDITION.** The plan's central thesis — the engine cutover is an
irreducible big-bang, de-risked by front-loading additive prep — is **sound and independently
verified**. The big-bang claim survives three concrete incremental-path attacks (§Finding 1). The
A–D prep stages are genuinely additive and CI-green under this repo's `-D warnings` posture
(§Finding 2), with one Stage-C caveat. The A–F boundaries are clean (§Finding 4). Structure is
complete (§Finding 5).

**The one gap that must be closed before Stage E is authored:** the plan resolves the `Drop`→async
hazard and the `apply_cycle_deltas` deadlock, but **misses the symmetric hazard on the read side of
the cognitive pipeline** — the `DreamCycle::run` trait boundary is **sync by contract**
(`traits.rs:344`) yet runs _inside_ what becomes an `async fn run_dream_cycle_guarded`, and its body
reaches the now-async backend through `DreamContext`. This is the same class of finding the plan
_did_ catch for `Drop`; it just wasn't carried to the cycle read path. It is a [HIGH], not a
blocker, because all `DreamCycle` impls are in-crate (no external consumer breaks), but it is a real
design decision the cutover cannot avoid and the plan does not make. Detail in §Finding 6.

Three figures in the plan are inaccurate and should be corrected so the cutover PR is sized honestly
(§Finding 3) — none change the strategy, but "~14 methods" and "~50–80 helpers" understate and
overstate respectively, and a reviewer sizing the PR off them will be surprised.

---

## Finding 1 — Is the big-bang claim true? [CONFIRMED SOUND]

> Plan §1: "The cutover is one irreducible big-bang commit … no half-async engine compiles."

**The claim is correct.** I pressure-tested three incremental paths the plan did not explicitly rule
out; all three are non-viable, each for a concrete, cited reason.

**(a) Dual-field coexistence** (engine holds _both_ `pool` and `storage` during migration, converted
methods use `storage.*().await`, unconverted keep `self.with_read`). **NOT VIABLE.** `SqliteBackend`
wraps an existing `Arc<ConnectionPool>` (`src/storage/sqlite/mod.rs` `from_pool`), so the two fields
_could_ share one pool — but the pool hands out RAII `ReadGuard`/`WriteGuard`s that mutate a single
`Mutex<Vec<Connection>>` on drop (`src/pool/connection_pool.rs`). Two abstractions
(`with_read` directly + `storage` via its own `spawn_blocking` borrow) concurrently checking
connections out of and back into the same pool is a connection-leak / guard-race hazard, not a clean
seam. It trades one big-bang for a subtle concurrency bug surface — strictly worse.

**(b) Per-method sync-over-async bridge** (method stays `pub fn`, calls `block_on(storage.*())`
internally, flip signatures last). **NOT VIABLE.** The backend's `block_read`/`block_write` are
`async` and themselves `spawn_blocking` onto the pool. Calling `block_on` from a thread already owned
by the tokio runtime (which CLI-with-`#[tokio::main]` and MCP's handler thread both are) panics or
deadlocks. The existing `AsyncMemoryEngine` exists precisely to invert this (it `spawn_blocking`s the
_sync_ engine); reversing the inversion reintroduces the hazard it was built to avoid.

**(c) Sync facade over storage** (reimplement `with_read`/`write_conn` as private `block_on` shims so
the ~67 internal sites never change). **NOT VIABLE.** There is no sync backend API to call — the
`StorageBackend` family is async-only (`#[async_trait]`, `src/storage/*.rs`), and a `block_on` shim
inherits (b)'s deadlock. Worse, `with_read(|conn| FactStore::new(conn, …).get(id))` passes an opaque
closure a _live `&Connection`_; the backend cannot abstract that without re-exposing a sync pool —
the exact thing #630 moved below the seam.

**Verdict: big-bang is genuinely forced.** The struct field swap deletes `with_read`/`write_conn`
(mod.rs:573, 586) whose only data source is `pool`; every site that used them breaks at once; a
half-async public surface does not compile against sync callers; the atomic workspace build drags
CLI+MCP+`AsyncMemoryEngine`-deletion into the same PR. The plan correctly identified the
load-bearing constraint and the right mitigation (front-load design → cutover is pure translation).
This is the plan's strongest call.

---

## Finding 2 — Are Stages A–D independently green + additive? [CONFIRMED, one caveat]

The decisive fact: CI runs `cargo clippy --workspace --all-targets --all-features -- -D warnings`
(`.github/workflows/ci.yml:31`), and `[lints.rust]` (root `Cargo.toml`) forbids only `unsafe_code` —
so `dead_code`/`unused_*` are warn-by-default rustc lints **promoted to hard errors by `-D
warnings`**. Additive-green therefore requires every new symbol to be _reachable_, not merely
compile.

- **Stage A (atomic methods + config on the bounded traits).** **GREEN.** The 5 atomic methods land
  as **`pub` trait methods** (`FactGraph`/`ConsolidationStore`/`ColdStorage`) with `SqliteBackend`
  impls. A `pub` trait method is API surface → never `dead_code`; its impl satisfies the trait →
  never dead. The plan's parity tests "drive `SqliteBackend` directly," giving call sites regardless.
  `get_config`/`set_config` already exist as `pub fn` (`src/store/schema/mod.rs:123,159`); promoting
  them onto `SchemaManager` is additive. No unused-warning risk. **Sound.**

- **Stage B (HNSW into `SqliteBackend`).** **GREEN with care.** The trait surface (`vector_search`)
  already exists (#629/#630) and is exercised. The risk is any **backend-private** helper introduced
  for snapshot relocation that nothing calls until Stage E — that _would_ trip `dead_code -D
warnings`. The plan's "snapshot round-trip" parity test must call the new snapshot path, or the
  helper needs `#[cfg(test)]`-only construction wired immediately. Flag to the implementer; not a
  plan defect, but the additive-green guarantee for Stage B is conditional on the parity test
  actually consuming the relocated code.

- **Stage C (port-driven `hybrid_search` _alongside_ the old).** **GREEN only if the parity oracle
  consumes the new fn in the same PR.** `hybrid_search` is a free `pub fn` (`src/search/hybrid.rs:144`).
  A second decomposed version that the engine does not call until Stage E is **dead code under `-D
warnings`** unless its bit-identical parity test (which the plan _does_ specify, §5) calls it. So
  the additive-green claim holds _because_ the parity test is in-PR — the plan should state this
  dependency explicitly ("the parity oracle is what keeps the second `hybrid_search` from tripping
  `-D warnings`"), since it is load-bearing, not incidental. This is the one place where "additive +
  green" is true but only by construction.

- **Stage D (`async` default-on).** **GREEN, mechanical.** Flipping `default = ["async"]` with tokio
  already a CLI dependency (`memory-engine-cli/Cargo.toml:29`, `rt-multi-thread`+`macros`) and an MCP
  dependency (`memory-engine-mcp/Cargo.toml:25`). The "tokio in CLI ~30 crates" cost the design doc
  frets over is **already partly paid** — the dep is declared today; Stage D only makes it
  unconditional.

Net: A–D are additive and CI-green. The only nuance is that "nothing consumes these yet" (plan §7)
is slightly misleading under `-D warnings` — the **parity tests** consume them, and that is exactly
what keeps them green. State it.

---

## Finding 3 — Cutover PR size: figures are off; strategy unaffected [MEDIUM]

The cutover is large but the plan's own numbers misrepresent _which_ parts are large. Corrected
against source:

| Plan claim                                | Measured                                                                                                                                             | Note                                                                                                                                                                                                                                                                                                        |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "all 14 `pub fn`→`async fn`"              | **85** `pub fn`/`pub async fn` across `src/engine/**` (excl. test mods); the DB-touching subset that must go async is **larger than 14**             | "14" appears to count a hand-picked core; it is not the count of methods that flip. `cognitive.rs` alone re-exposes ~12 (`query`/`get_fact`/`consolidate`/`forget`/`promote`/…). Whatever "14" denotes, the PR touches far more `async fn` signatures than the number implies. **Correct it or define it.** |
| "~150–170 internal sites"                 | **67** direct `self.with_read`/`self.write_conn`/`self.pool` in `src/engine/` (`grep`), heaviest in mod.rs (15), inspect (8), archive (6), query (5) | The 150–170 presumably counts resulting `.await` sites + transient-store constructions. The _direct_ surface is 67. Honest framing: "67 helper call sites expand to ~150 await points."                                                                                                                     |
| "~250 engine tests"                       | **~245** `#[test]`, **0** `#[tokio::test]` today                                                                                                     | Accurate within rounding. **Sound.**                                                                                                                                                                                                                                                                        |
| "~50–80 test helpers reach `engine.pool`" | **~16–18 distinct** helpers / **~19** call sites (`insert_raw_fact` tests.rs:53 is the one reusable utility; the rest are inline test bodies)        | **Overstated ~3×.** Good news for the PR (the re-point is smaller), but the `#[cfg(test)] storage()` accessor still needs to cover them.                                                                                                                                                                    |

None of these change the staging strategy. They matter because a reviewer approving "one PR, ~14
methods" will open a diff touching dozens of signatures + 67 sites + ~245 test conversions + CLI +
MCP and reasonably balk. **Fix the figures so the PR's true mass is declared up front.** The
mechanical/bisectable argument is the right defense for that mass — but only if the mass is stated.

**Can CLI/MCP be staged behind a temporary sync shim to shrink the cutover?** No — verified in
Finding 1(b/c): a sync shim over the async backend deadlocks on the runtime thread. The
workspace-atomic-build constraint is real; CLI (24 engine call sites across 12 commands, sync
`fn main() -> ExitCode` at `main.rs:77`) and MCP (single `spawn_blocking` at `server.rs:102`) must
flip in the cutover. MCP is genuinely a one-site change (drop the `spawn_blocking`, `.await`
`dispatch`); CLI is a `#[tokio::main]` + 24 `.await`s. Both are mechanical. The constraint is not
artificial.

---

## Finding 4 — Sub-issue split sanity [SOUND]

The A–F boundaries are the right cut. Each prep stage is a distinct bounded-context change with its
own parity oracle (A=atomicity/rollback, B=recall/ordering, C=bit-identical fusion, D=feature flip),
which is exactly how to keep the cutover "pure translation." Specific judgments:

- **A and C should NOT merge** despite both touching retrieval-adjacent code: A is `area:storage`
  (transaction primitives), C is `area:retrieval` (fusion decomposition), and C's parity oracle is
  conceptually independent of A's. Keep them split. **Agree with plan.**
- **B and C ordering is correct**: C's port-driven `hybrid_search` calls `storage.vector_search()`,
  whose HNSW-vs-brute dispatch B moves into the backend. C depends on B. The plan lists B before C.
  **Correct.**
- **F (cfg-cleanup) correctly deferred** — it is post-cutover, mechanical, and bundling it would
  bloat the already-large E with no correctness benefit. **Agree.**
- **One refinement:** the plan folds the `Drop`/`close()` split _and_ (per this review) the
  `DreamCycle::run` boundary resolution into Stage E. The `close()`/`Drop` split is small. But if the
  `DreamCycle` boundary resolution chosen is "make `DreamCycle::run` async" (§Finding 6), that is a
  **trait-signature change with in-crate ripple** (`DefaultDreamCycle`, `LlmDreamCycle`) that is
  _additive-able ahead of E_ — it could be its own tiny prep stage (call it **A′**) landing the async
  trait + `#[async_trait]` with the impls still sync-bodied behind a `block_on`, so E only removes the
  bridge. Whether to pre-stage it depends on which resolution is picked; see §Finding 6.

---

## Finding 5 — Structural completeness [SOUND]

Documentation (§8): rustdoc for atomic-method contracts, `crate-layout.md` + `architecture-overview.md`
threading-model update, a **required** user-facing migration note for the async API break, CLAUDE.md
status. Complete and correctly flags the public break as REQUIRED.

Testing (§9): additive parity/differential per prep stage with the _replaced code as oracle_
(crash-injection rollback, recall/ordering sweep, bit-identical fusion) + the ~245 converted engine
tests as the behavior-preservation gate + #632 conformance as the eventual cross-backend proof. This
is the right test architecture for a behavior-preserving refactor.

Verification (§10): per-stage `build`/`test`/`clippy`/`fmt`/`deny` + two cutover-only grep invariants
(`! grep self.pool|with_read|write_conn`, `! grep AsyncMemoryEngine`). The greps are good
machine-checkable exit criteria. **Note:** the second grep target is `src/async_engine.rs` (the file,
663 lines per the design doc) — the cutover deletes the file and the `lib.rs` re-export; the grep
correctly catches a stale reference. Verified `AsyncMemoryEngine` has no cli/mcp/embed consumers
(only `src/async_engine.rs` self-refs + `lib.rs` re-export), so deletion is clean. **The plan's
"audit: only lib.rs re-export + docs" claim is accurate.**

All three required structural sections present and substantive. **Sound.**

---

## Finding 6 — MISSED HAZARD: the sync `DreamCycle::run` boundary in an async engine [HIGH]

This is the one substantive gap. The plan's §6 catches two async-boundary hazards
(`Drop`→`write_snapshot`, and the `apply_cycle_deltas` single-connection deadlock) but does **not**
address the symmetric hazard on the **read** side of the cognitive pipeline.

**The facts (all verified):**

- `DreamCycle::run` is **sync by contract**: `fn run(&self, ctx: &CycleContext) -> Result<CycleReport>`
  (`src/traits.rs:344`).
- `CycleContext` **wraps** the capability handle `DreamContext` (`src/engine/cycle/context.rs:27`,
  exposed via `ctx.dream()`).
- A `DreamCycle::run` body reaches the engine through it: `DefaultDreamCycle::run` calls
  `ctx.dream().list_undreamt_in_period(window)?` and `ctx.dream().outcome_counts_batch(…)?`
  (`src/engine/cycle/default_impl.rs`), and `DreamContext::{list_active_facts,get_fact,
list_undreamt_in_period}` call `self.engine.with_read(…)` directly (`src/engine/cognitive.rs:73–148`).
- The engine invokes it as `cycle.run(&cycle_ctx)` inside `run_dream_cycle` / `run_dream_cycle_guarded`
  (`src/engine/cognitive.rs:211`), which **become `async fn` in the cutover.**

**The hazard:** after the cutover, `self.engine.with_read` is backed by the **async** backend. But
`DreamCycle::run` is a **sync** trait method — you cannot `.await` inside it. So an `async
run_dream_cycle_guarded` must execute a sync `cycle.run()` whose body needs async DB reads. The
resolutions, with consequences:

1. **`DreamContext` methods `block_on` the backend internally, `run_dream_cycle` stays async.**
   Deadlocks: `run_dream_cycle_guarded` runs on the runtime thread; `block_on` from there
   panics/deadlocks (Finding 1b). Only safe if `cycle.run()` is dispatched via `spawn_blocking` so it
   owns a blocking thread — but then `DreamContext` must use `Handle::current().block_on`, and the
   engine is `spawn_blocking`-ing its own work, partly resurrecting the `AsyncMemoryEngine` pattern
   the cutover deletes.
2. **Make `DreamCycle::run` async** (`async fn run` + `#[async_trait]`). Cleanest semantically.
   Ripples to the in-crate impls (`DefaultDreamCycle`, `LlmDreamCycle`, 2 test cycles) — all
   in-repo, no external consumer. CLI (`consolidate.rs:34`) and MCP (`tools/mod.rs:1665`) pass
   `&dyn DreamCycle` to `run_dream_cycle_guarded`; their call sites gain `.await` but no impl. This is
   the option I'd recommend, and it is **pre-stageable** (Finding 4, "A′").
3. **`DreamContext` methods stay sync over a sync read path.** Impossible — the read path _is_ the
   async backend post-cutover; there is no sync read path left (that is the whole point of #631).

**Why HIGH not BLOCKER:** every `DreamCycle` impl is in-crate (`grep 'impl DreamCycle'` → 4 hits, all
in `src/engine/`), so there is no surprise external break, and a working resolution (option 2) exists
and is mechanical. But the plan currently labels the entire cutover "pure translation — no new logic"
(§7), and **this is new logic / a design decision**, exactly like the `Drop` split the plan _did_
elevate to §6. Leaving it implicit means the cutover author hits it mid-PR with no pre-decided
answer — the precise failure the front-loading strategy exists to prevent.

**Required addition:** add a §6 bullet deciding the `DreamCycle::run` async boundary (recommend
option 2, async-trait, pre-staged as A′ landing the trait + `#[async_trait]` with `block_on`-bridged
sync bodies so Stage E only removes the bridge), symmetric to the existing `apply_cycle_deltas` /
`Drop` bullets. Until that decision is in the plan, "pure translation" overstates the cutover.

---

## Confirmed sound (positive findings)

- The irreducibility thesis is correct and survives adversarial probing (Finding 1) — the strongest
  part of the plan.
- The atomic-method "verbatim tx-body move" is real: `insert_fact_with_embedding`'s tx body
  (`ingest.rs:228–232`) and `commit_archive`'s manifest-insert+hard-delete (`archive.rs:253–273`)
  are exactly the bodies that relocate; HNSW `notify_insert` already sits in the post-commit tail
  _outside_ the tx (`ingest.rs:235–238`), so the plan's "notify fires backend-private post-commit"
  is structurally accurate.
- The `apply_cycle_deltas` full-push-down rationale is correct: `apply_cycle_report` holds one write
  connection across validate+apply specifically to avoid an in-memory self-deadlock
  (`apply.rs:361–363` comment confirms), and it is a `#[allow(clippy::too_many_lines)]` ~200-line
  single-transaction body — a validate/apply split would reopen the deadlock and add TOCTOU, as the
  plan states.
- The `StorageBackend` foundation is genuinely async-native and object-safe-proven already
  (`src/storage/backend.rs` proves object-safety _and_ callability-through-`dyn` under async_trait),
  so the prep stages build on solid ground.
- A–F boundaries and the test/doc/verification structure are right (Findings 4, 5).

---

## Recommendation

Proceed. Before authoring Stage E:

1. **[HIGH, required]** Add the `DreamCycle::run` async-boundary decision to §6 (recommend async-trait
   - an A′ pre-stage). This is the one missed hazard.
2. **[MEDIUM]** Correct the three figures (Finding 3): define/fix "14 methods", reframe "150–170
   sites" as 67 direct → ~150 await, fix "50–80 helpers" → ~16–18.
3. **[LOW]** State explicitly in §7 that the Stage-B private snapshot helper and the Stage-C second
   `hybrid_search` stay CI-green _because their parity tests consume them_ under `-D warnings` — the
   "nothing consumes these yet" phrasing is what a reviewer will (correctly) challenge.

None of these touch the core strategy, which is correct.
