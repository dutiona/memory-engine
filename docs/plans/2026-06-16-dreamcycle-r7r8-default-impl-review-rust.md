# Adversarial Rust Review — DreamCycle R7/R8 + Default DBSCAN Impl Plan

**Reviewed plan:** `2026-06-16-dreamcycle-r7r8-default-impl.md`
**Reviewer:** rust-development skill (adversarial mode)
**Date:** 2026-06-16

All findings cite file:line in the worktree at
`/home/mroynard/dev/memory-engine/.worktrees/feat-49-dreamcycle-default`.

---

## [BLOCKER] B1 — In-memory deadlock in `apply_cycle_report` when engine runs on in-memory pool

**Plan reference:** Task 5 — "single `write_conn()` + one `unchecked_transaction()`"

**Root cause.** `ConnectionPool::read()` for an in-memory pool acquires `write_conn.lock()` (the
`Mutex<Connection>`), returning `ReadConn::InMemory(WriteAsReadGuard { guard })` which holds the
mutex guard live until dropped (`connection_pool.rs:195–209`). `write_conn()` calls
`pool.try_write()`, which calls `self.write_conn.lock()` (`connection_pool.rs:220–224`).
`parking_lot::Mutex` is non-reentrant by design.

The plan's `validate_report` is described as "single read connection" pre-flight. If the engine is
in-memory (the most common test configuration — `MemoryEngine::builder(4).build()` produces an
in-memory pool with `read_pool_size = 0`), `with_read` → `pool.read()` acquires `write_conn.lock()`
and returns a `ReadConn::InMemory` guard. `apply_cycle_report` then calls `write_conn()` to acquire
the write lock for the transaction — deadlock on the same `parking_lot::Mutex`.

The plan is aware of the "lock trap" at the engine level for `promote_with_lineage`, but the
`validate_report` pre-flight is described as using "a single read connection" without acknowledging
that for in-memory engines this is the same lock. The entire `validate_report` → `write_conn()`
sequence is also unsafe if `validate_report` itself is called while holding a read guard.

**Fix:** `validate_report` must not call `with_read`; it must accept `&Connection` directly, or be
called strictly before any lock is acquired and operating on a separate read conn that is
dropped before `write_conn()` is called. For in-memory engines the only correct approach is to
do all reads on the already-held write connection: pass `validate_report(&conn)` after acquiring
the write lock in `apply_cycle_report`.

**Evidence:**
- `src/pool/connection_pool.rs:19` — `write_conn: Mutex<Connection>` (parking_lot, non-reentrant)
- `src/pool/connection_pool.rs:194–209` — in-memory `read()` locks `write_conn`
- `src/pool/connection_pool.rs:220–224` — `try_write()` = `write_conn.lock()`
- Tests for `apply_cycle_report` use `MemoryEngine::builder(N).build()` (in-memory) by the
  same pattern as all other engine tests (`cognitive.rs:331`), so the lock-safety regression
  test (Task 5g) would itself deadlock.

---

## [BLOCKER] B2 — `EventStore::new` requires `&UpcasterRegistry`; `apply_cycle_report` has no registry reference

**Plan reference:** Task 5 — "`TagOutcome→EventStore::insert OutcomeSignal`"

`EventStore::new` signature (`src/store/events.rs:112`):
```rust
pub(crate) const fn new(conn: &'a Connection, registry: &'a UpcasterRegistry) -> Self
```

`EventStore` carries a `&'a UpcasterRegistry` field (`events.rs:25–26`). Any call site that
constructs `EventStore::new` must supply a registry reference.

`apply_cycle_report` will live in `engine/cycle/apply.rs`, which is inside `MemoryEngine` impl
but not `MemoryEngine` itself (it's a standalone fn or an `impl MemoryEngine` method). Only
`MemoryEngine` owns `self.upcaster_registry`. Free functions operating on `&Connection` cannot
access `self.upcaster_registry` without being given a reference to it.

The plan describes `apply_cycle_report` as a free function (or `impl MemoryEngine` with only
`&Connection` passed down), but the `TagOutcome` delta path must construct an `EventStore`. It
either needs `self.upcaster_registry` (making it an `impl MemoryEngine` method, which is fine)
or a `&UpcasterRegistry` parameter (which the plan does not mention). The plan does not thread
this through at all — it silently assumes `EventStore::insert` takes only `&Connection`, which
is false.

**Fix:** `apply_cycle_report` must be an `impl MemoryEngine` method (not a free fn) so it can
pass `&self.upcaster_registry` to `EventStore::new`, OR accept `&UpcasterRegistry` as a
parameter. The `promote_in_conn` helper also indirectly benefits from being a method.

**Evidence:** `src/store/events.rs:25–26,112`.

---

## [BLOCKER] B3 — `promote_in_conn` savepoint nesting inside `unchecked_transaction` is incorrect for rusqlite

**Plan reference:** Task 5 — "one `unchecked_transaction()`/savepoint, replay deltas... `Promote→promote_in_conn` on the shared conn"

The current `promote_with_lineage` uses `conn.savepoint()` at the outermost level (no enclosing
transaction). The plan creates an `unchecked_transaction()` in `apply_cycle_report`, then calls
`promote_in_conn` which would call `conn.savepoint()` inside the already-open transaction.

SQLite supports savepoints nested inside transactions; rusqlite's `Connection::savepoint()` issues
`SAVEPOINT sp_N` which is valid SQL inside a transaction. This part is structurally sound —
savepoints nest correctly.

However: `unchecked_transaction()` in rusqlite starts a `BEGIN` (deferred by default). Calling
`conn.savepoint()` inside a live `unchecked_transaction` works correctly in SQLite, but the inner
`sp.commit()` issues `RELEASE sp_N` (not `COMMIT`), so only the outer `tx.commit()` actually
commits. This means any error after `sp.commit()` but before `tx.commit()` correctly rolls back
the savepoint's writes — which is the intended behavior.

**The actual blocker is different**: the plan says "`promote_in_conn` on the shared conn". But
`conn.savepoint()` on a `&Connection` borrows the connection mutably (rusqlite's savepoint takes
`&mut Connection` prior to rusqlite 0.31, or immutable if using the newer API). Check which
rusqlite version is in Cargo.toml before assuming immutable borrow.

**Evidence to verify before implementation:** Run `grep rusqlite Cargo.toml` in the worktree.
If rusqlite < 0.31, `savepoint()` requires `&mut Connection` and the borrow of `conn` for the
transaction would conflict. This is a compile-time blocker if the version is old. The plan does
not identify the rusqlite version.

**Additional structural concern:** `promote_in_conn` receives `&Connection` but `FactStore::insert`
and `LineageStore::insert` both take `&'a Connection` and call `conn.last_insert_rowid()`.
`last_insert_rowid()` requires only `&Connection`, which is fine. Both store constructors take
`&'a Connection` immutably (`facts.rs:54`, `lineage.rs:15`), so immutable borrow is sufficient
if rusqlite allows it — consistent with how `FactStore::new(&sp, ...)` is called in the existing
`promote_with_lineage`.

---

## [HIGH] H1 — `AdjustScore` clamp: current-score read inside transaction creates read-then-write gap

**Plan reference:** Task 5 — "`AdjustScore→update_importance_score(clamp(cur + adj*STEP))`"

The plan says to read the current `importance_score`, compute the delta, clamp, then write. Inside
a single `unchecked_transaction` on the write connection, this is a self-consistent read-then-write
on the same connection — SQLite's transaction isolation guarantees the read sees the pre-transaction
state and the write is visible within the transaction. No TOCTOU issue here from concurrent
writers (there is exactly one write connection). This is correct within a single connection.

**However**: the plan's `validate_report` is described as a "read-only pre-flight (single read
connection)" that checks score bounds before the transaction. If `validate_report` checks
`|adjustment| ≤ 2` on the `AdjustScore.adjustment` field (which is just a field validation, not a
score read) this is fine. But if `validate_report` reads the current `importance_score` to check
whether `cur + adj*STEP` would land in `[0,1]` — which the plan implies with "per-delta `fact_id`
existence, `AdjustScore` `|adjustment| ≤ 2`" — and there is concurrent activity between validation
and apply, a fact's score could drift. This is an edge case for file-backed engines with concurrent
readers but is worth noting. For in-memory engines there is no concurrency.

**Actual concern:** `update_importance_score` at `facts.rs:610–616` does NOT check for `NotFound`
— it silently succeeds even if `id` does not exist (no rows-affected check). The plan says
`validate_report` checks existence, but if a fact is expired/deleted between validate and apply,
the score update silently no-ops instead of returning `CycleError::UnknownFact`. This breaks
the test expectation that "one invalid delta → full rollback". The rollback won't fire because
`update_importance_score` doesn't error.

**Fix:** Add a rows-affected check inside `update_importance_score`, or have `apply_cycle_report`
re-check existence inside the transaction.

---

## [HIGH] H2 — `merge_metadata` via `json_set` is a new SQL primitive that does not exist in the codebase

**Plan reference:** Task 4 — "`pub(crate) fn merge_metadata` (read current metadata, shallow-merge object via `json_set`)"

Verified: no `json_set` usage exists anywhere in the codebase (`grep -rn "json_set"` returns 0
matches). The plan proposes implementing this for the first time as part of Task 4. This is not
a blocker by itself, but the plan describes it as trivially reusing existing infrastructure when
it is actually new code that needs careful design.

**SQLite `json_set` semantics pitfall:** SQLite's `json_set(json, path, value)` sets a *specific
path* (e.g. `json_set(metadata, '$.quarantine', json(?))`) but does NOT perform a shallow object
merge. To merge two JSON objects (e.g. preserve existing keys while adding new ones) you need
multiple `json_set` calls (one per key in the patch) or `json_patch` (SQLite 3.38+). The plan
says "shallow-merge object" which implies merging all keys of the patch object, not setting a
single path. If the implementation uses `json_set(metadata, '$.key', val)` for each key it is
correct but requires iterating keys in Rust; if it generates a single `json_set` call expecting
full-object merge, it will produce wrong output.

**NULL metadata path:** The `facts` DDL has `metadata TEXT NOT NULL DEFAULT '{}'`. The `CHECK(json_valid(metadata))` constraint is enforced on write. A `json_set` that produces invalid JSON (e.g., setting a key to a Rust `serde_json::Value::Array` without `json(?)` escaping) would violate the constraint and produce an `SqliteFailure` (SQLITE_CONSTRAINT_CHECK). The plan must use `json(?)` or serialize the value to a JSON string for the `json_set` value argument.

**Fix:** Clarify whether `merge_metadata` does Rust-side merge (read JSON, merge in Rust via
`serde_json`, serialize back) or SQL-side `json_set`. The Rust-side approach is simpler and
avoids the multiple-`json_set` complexity; the SQL approach is faster but requires careful
escaping. Choose one explicitly in the plan.

---

## [HIGH] H3 — `CycleContext<'a>` lifetime/borrow when constructing `prior_wisdom` from the store

**Plan reference:** Task 6, D5 — "`CycleContext<'a>{ ctx: DreamContext<'a>, prior_wisdom: Vec<Fact>, ... }`"

In `run_dream_cycle` (`cognitive.rs:139–146`), the sequence the plan implies is:
1. `write_conn()` access-check (drops guard immediately — correct)
2. Query the store to fill `prior_wisdom` (requires `with_read`, which produces a `ReadConn`)
3. Construct `DreamContext::new(self)` — borrows `&'a MemoryEngine`
4. Construct `CycleContext { ctx: DreamContext<'a>, prior_wisdom, ... }`
5. Call `cycle.run(&cycle_ctx)`

Steps 2 and 3 do not conflict: the `ReadConn` guard from step 2 is dropped before step 3
borrows `self` as `&'a MemoryEngine`. `Vec<Fact>` owns its data (no lifetime tied to the
connection), so step 4 is fine. The borrow-checker sequence is valid.

**Object safety:** `DreamCycle::run` with signature `fn run(&self, ctx: &CycleContext) -> Result<CycleReport>` is object-safe as long as `CycleContext` has no generic parameters and no `where Self: Sized` constraint. The current `DreamCycle` is already used as `&dyn DreamCycle` (`traits.rs:135`, `lib.rs:135`). Adding `&CycleContext` (a concrete reference) does not break object safety. The `async_engine.rs:587` uses `Arc<dyn DreamCycle + Send + Sync>`, which requires `DreamCycle: Send + Sync` — ensure `CycleContext` itself is `Send + Sync` (it will be, since it holds `&'a MemoryEngine` which is `Send + Sync`, plus owned `Vec<Fact>` which is also `Send + Sync`).

**Verdict:** No borrow or object-safety blocker here, but the plan should explicitly state that
the `ReadConn` guard from the `prior_wisdom` query is dropped (via scope end) before `DreamContext::new(self)` borrows `self`. This is a documentation gap, not a code bug.

---

## [HIGH] H4 — `#[non_exhaustive]` same-crate `match` claim is CORRECT but `apply_cycle_report` must still be updated when variants are added

**Plan reference:** D4 — "`#[non_exhaustive]` lets #57 add fields & #578 add `CycleDelta` variants non-breaking"

The plan's D4 claim is correct for *downstream crates*: `#[non_exhaustive]` on an enum forces
external matchers to include a wildcard arm. Within the same crate, `#[non_exhaustive]` has no
effect — exhaustive matches on `CycleDelta` inside `engine/cycle/apply.rs` (same crate) do NOT
require a wildcard and will produce an "unreachable pattern" warning if one is added, and a
compile error if a new variant is added without updating the match.

**The plan does not address this intra-crate match maintenance requirement.** When #578 adds a
new `CycleDelta` variant, `apply_cycle_report`'s match must be updated or it will fail to compile.
This is actually the *desired* behavior (force explicit handling), but the plan implies `#[non_exhaustive]` provides protection for internal code — it does not.

**Impact:** Not a bug in the plan's proposed code, but the rationale in D4 is misleading for
same-crate consumers. If `DefaultDreamCycle::run` also pattern-matches on `CycleDelta` variants
to build the report, those matches will similarly be compile errors when new variants arrive.

**Recommendation:** Explicitly note in the ADR (Task 11) that same-crate matches on `CycleDelta`
are exhaustive and intentionally so — adding a variant in #578 is a required, compile-forced
update of all internal match arms.

---

## [MEDIUM] M1 — `i16 adjustment` quantum: clamp arithmetic has a precision trap at boundary values

**Plan reference:** D1 — "`AdjustScore.adjustment: i16` is a quantum count clamped to `[-2,2]`, store delta = `adjustment * STEP` clamped `[0,1]`"

`IMPORTANCE_STEP = 0.05_f64`. The computation is `cur + (adjustment as f64) * 0.05`, clamped to
`[0.0, 1.0]`.

`0.05_f64` is not exactly representable in IEEE 754 binary64. Accumulated floating-point error:
- `2 * 0.05 = 0.10000000000000001` (1 ULP off)
- `(-2) * 0.05 = -0.10000000000000001`

For a score at exactly `1.0`, `1.0 + 2*0.05` = `1.1` → clamped to `1.0`. No issue here.
For a score at `0.95`, `0.95 + 2*0.05` → `1.0500000000000000888...` → clamped to `1.0`. Fine.

The test the plan requires: "+2 thrice on a fact at 0.5 → `0.5 + 6*STEP`" — i.e., three separate
apply calls each adding `2 * 0.05 = 0.10`. Starting at 0.5: after first: `0.6`, after second:
`0.7`, after third: `0.8`. Each intermediate value is stored as `REAL` in SQLite (f64). Accumulated
error over multiple cycles is bounded but could produce `0.7999999999999999` instead of `0.8`.

This is a documentation accuracy issue, not a correctness blocker: the test assertion should use
`(actual - expected).abs() < f64::EPSILON * 10.0` rather than exact equality. The plan's spike
test description does not specify this tolerance.

---

## [MEDIUM] M2 — `Supersede` delta: `LineageStore::insert` validates source fact existence but the superseded `old_id` will be expired *before* lineage insert

**Plan reference:** Task 5 — "`Supersede→expire(old) + LineageStore::insert(new ← [old])`"

`LineageStore::insert` validates that all `source_fact_ids` exist in the `facts` table
(`lineage.rs:37–55`):
```sql
SELECT COUNT(*) FROM facts WHERE id IN (SELECT value FROM json_each(?1))
```

This query does NOT filter by `t_expired IS NULL`. An expired fact still passes this check.
**However**: if the plan calls `expire(old_id)` then `LineageStore::insert(new_id ← [old_id])`,
the sequence is:
1. `FactStore::expire(old_id)` — sets `t_expired` on `old_id`
2. `LineageStore::insert` with `source_fact_ids = [old_id]` — selects `COUNT(*)` from `facts`
   where `id IN [old_id]` — passes because the row exists (just expired)

So the order does not break `LineageStore::insert`. But the plan also requires the new fact
(`new_id`) to come from an `AddFact` delta earlier in the report, or be an existing fact. If
`new_id` comes from a prior `AddFact` delta in the same report (forward-ref), its insert happened
inside the same transaction and `LineageStore::insert`'s `COUNT(*)` check on `facts` would see
it — correct.

**Actual concern:** The plan's validation in `validate_report` says "Supersede `old_id` exists +
`new_id` exists-or-introduced-earlier-in-vec". Checking that `new_id` was "introduced earlier"
requires tracking which `fact_id`s were returned by prior `AddFact` operations *during validation*
— but `validate_report` runs on a read connection *before* applying any writes. No `AddFact`
inserts have happened yet during validation. The plan must either (a) skip validation of `new_id`
for forward-refs and only validate at apply-time, or (b) make `validate_report` aware of the
report's own `AddFact` list by scanning the `CycleReport` deltas for `AddFact` entries and
accepting those `fact_id`s as "virtual existing" — but `AddFact` doesn't have a pre-assigned
`fact_id` (SQLite assigns it on insert). This validation is **not possible as described** for
forward-ref `Supersede`. The plan either needs to drop this validation or clarify that forward-ref
`Supersede` is only checked at apply-time (inside the transaction).

---

## [MEDIUM] M3 — `DefaultDreamCycle<'a>` with lifetime carries non-obvious lifetime constraint on the impl

**Plan reference:** Task 9 — "`pub struct DefaultDreamCycle<'a>{ generator: &'a dyn SummaryGenerator, embedder: &'a dyn EmbeddingProvider, config: DreamCycleConfig }`"

`DreamCycle` is used as `&dyn DreamCycle` at call sites (`cognitive.rs:139`, `async_engine.rs:587`).
`DefaultDreamCycle<'a>` implements `DreamCycle` but the `dyn DreamCycle` trait object has an
implicit `'static` bound unless relaxed. In particular, `run_dream_cycle(cycle: &dyn DreamCycle)`
requires `DreamCycle: 'static` (implied by `dyn Trait` without explicit lifetime bound) unless the
call site uses `&dyn DreamCycle + '_`.

If `DefaultDreamCycle<'a>` is created on the stack and passed as `&dyn DreamCycle`, the borrow
checker will accept it because the reference's lifetime is the shorter of `'a` and the call site
lifetime. But `async_engine.rs:587` uses `Arc<dyn DreamCycle + Send + Sync>` — an `Arc` requires
`T: 'static` unless explicitly relaxed (`Arc<dyn DreamCycle + Send + Sync + 'a>`). Storing
`DefaultDreamCycle<'a>` in an `Arc` is not possible without the `'static` bound being dropped.

**Impact:** Users cannot `Arc::new(DefaultDreamCycle { generator: &my_gen, ... })` for use with
the async engine. This is a usability constraint that the plan does not surface.

**Fix options:** (a) Make `DefaultDreamCycle` own its generator/embedder behind `Arc<dyn ...>`
rather than borrowing them; (b) document that `DefaultDreamCycle` is not `Arc`-compatible; (c)
relax `AsyncMemoryEngine::run_dream_cycle` to `Arc<dyn DreamCycle + Send + Sync + '_>`. Option (a)
is the ergonomic choice since the consumer must already own the provider.

---

## [MEDIUM] M4 — `merge_metadata` called for `Quarantine` delta: the patch content is not specified, creating an API contract gap

**Plan reference:** Task 5 — "`Quarantine→expire + merge_metadata`"; D6 — "`{"quarantine":{"reason","at"}}` metadata marker"

`merge_metadata` takes a `patch: &serde_json::Value`. For `Quarantine{fact_id, reason}`, the plan
says to merge `{"quarantine": {"reason": <reason>, "at": <now>}}`. This means `merge_metadata` must
accept a `serde_json::Value::Object` as the patch and merge it into the fact's existing metadata.

The `CHECK(json_valid(metadata))` constraint fires on the UPDATE. The patch must produce valid JSON.
If `reason` contains characters that break JSON serialization (it's a `String`, so standard serde
handles it), the constraint is satisfied. This is not a blocker but requires testing: a reason
containing double-quotes or backslashes (e.g., `"the user said \"don't\""`) must be correctly
serialized before storage.

The gap: the plan never specifies whether `merge_metadata`'s SQL implementation uses
`json_set(metadata, '$.quarantine', json(?))` or a Rust-side merge. As noted in H2, these have
different behaviors. The plan should commit to one approach.

---

## [LOW] L1 — `list_undreamt_in_period` helper: Rust-side filter on `Fact.metadata` is O(N)

**Plan reference:** Task 4 — "a `list_undreamt_in_period(...)` selection helper (or Rust-side filter on `Fact.metadata`)"

Filtering by `metadata->>'dream_cycled'` in Rust after fetching all active facts deserializes
all embeddings unnecessarily. SQLite's `json_extract` can push the filter to SQL:
`WHERE json_extract(metadata, '$.dream_cycled') IS NULL`. This avoids embedding deserialization
for facts that are excluded. For large corpora this is a meaningful performance difference.

The plan's parenthetical "(or Rust-side filter)" is too casual for what is the main selection
query driving the entire dream cycle. Recommend SQL-side filter in the task specification.

---

## [LOW] L2 — `CycleMetadata.processed_ids: Vec<FactId>` grows unboundedly in the config JSON

**Plan reference:** Task 2 — "`CycleMetadata{..., processed_ids: Vec<FactId>}`"; Task 7 — "bounded (last 8, FIFO)"

The plan bounds `prior_reports` to 8 entries (FIFO), but each `CycleMetadata` carries
`processed_ids: Vec<FactId>`. If a cycle processes 50,000 facts, the 8 retained entries contain
8 × 50,000 = 400,000 `i64` IDs serialized to JSON in the `config` table, which is a `TEXT NOT
NULL` column (`schema.rs`). SQLite has no column size limit by default, but this is a
correctness-threatening design for large engines.

`processed_ids` in `CycleMetadata` should either be capped (e.g., first/last 1,000), stored
separately (a dedicated `cycle_facts` table — but that requires a schema migration), or replaced
with a summary (e.g., a bloom filter or count). The plan's honest-limitations section does not
mention this.

---

## [LOW] L3 — Clippy `pedantic+nursery`: several patterns that will fire

**Plan reference:** Task 12 / Verification gate.

Likely clippy findings from the described code:

1. `CycleAnomaly` is defined as "reserved for future soft-fail; empty in v1" — an empty struct/enum
   with no methods or fields will trigger `clippy::empty_structs_with_brackets` or
   `clippy::manual_non_exhaustive` depending on shape. If it is an empty enum, it is
   `clippy::empty_enum` (pedantic). Requires `#[allow(...)]` or a placeholder variant.

2. `adjustment: i16` in `AdjustScore` paired with a clamp check `|adjustment| ≤ 2`: the range
   `[-2, 2]` fits in `i8`. Using `i16` for a value bounded to 5 possible values will trigger
   `clippy::cast_possible_truncation` when converting to `f64` (though `i16 as f64` is lossless,
   clippy pedantic may flag `i16::from(adjustment)` style). Minor.

3. `DefaultDreamCycle<'a>` with `&'a dyn Trait` fields — if the plan uses a `new()` constructor,
   clippy `clippy::missing_const_for_fn` will trigger if the constructor can be `const` (it
   cannot because trait objects are involved, so this is fine).

4. `pub struct DefaultDreamCycle` is re-exported at crate root. If its fields are `pub(crate)` but
   the type is `pub`, clippy `clippy::struct_field_names` and/or `clippy::redundant_field_names`
   may fire depending on naming.

5. `CycleReport` with `#[non_exhaustive]` and `derive(Default)` — `#[non_exhaustive]` on a struct
   with `Default` means callers cannot construct with `..Default::default()` spread syntax from
   outside the crate. Not a clippy issue, but a usability footgun.

---

## [LOW] L4 — `IdentityOutput::empty()` constructor alongside `Default` derive is redundant

**Plan reference:** Task 2 — "`IdentityOutput{...} #[non_exhaustive] + Default + empty()`"

If `Default` is derived and produces the same value as `empty()`, clippy `clippy::new_without_default`
(inverted: `empty()` without `Default`) or redundancy lints will apply. Either name it `new()` and
derive `Default` from it via `impl Default for IdentityOutput { fn default() -> Self { Self::empty() } }`,
or drop `empty()` and use `IdentityOutput::default()` at call sites. Having both is redundant and
clippy pedantic will note it.

---

## Summary of verification requirements not in the plan

1. **rusqlite version** — must confirm `Connection::savepoint()` takes `&self` (immutable) in the
   version used; otherwise B3 is a compile blocker.
2. **`json_set` vs Rust-side merge** — the implementation strategy for `merge_metadata` must be
   decided before Task 4, not discovered during implementation.
3. **`validate_report` scope** — must be redesigned to run inside the write lock (on the
   already-held `&Connection`) to avoid the in-memory deadlock (B1).
4. **`EventStore::new` registry threading** — `apply_cycle_report` must be `impl MemoryEngine`
   to access `self.upcaster_registry` (B2).

---

*All file:line citations verified against the worktree at plan-review time.*

## Resolution

- [B1 deadlock] two-guard validate(read)→apply(write) on in-memory engines → **Fixed**: Task 5 now does validate-then-apply on a SINGLE `write_conn()` acquisition.
- [B2 compile] `EventStore::new` needs `&UpcasterRegistry`; `apply_cycle_report` had no access → **Fixed**: `apply_cycle_report` is now explicitly an `impl MemoryEngine` method threading `self.upcaster_registry`; `TagOutcome` inserts on the shared tx (NOT `record_outcome`, whose own `write_conn` is the same self-deadlock trap).
- [M2 validation impossibility] forward-ref Supersede check at validation-time → **Fixed**: validation only checks "an earlier AddFact provides new_id" structurally; the real id resolves at apply-time.
- [non_exhaustive same-crate] → acknowledged: same-crate matches don't need a wildcard; the plan doesn't rely on one internally.
