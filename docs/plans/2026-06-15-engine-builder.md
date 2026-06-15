# Plan — Replace telescoping `MemoryEngine` constructors with a builder

**Closes:** #541 (telescoping `MemoryEngine::open*` → builder), #113 (duplicate: "6 constructor
variants instead of builder"), #149 (builder pattern for `EngineConfig`).

**Worktree:** `/home/mroynard/dev/memory-engine/.worktrees/refactor-541-engine-builder`
(branch `refactor/541-engine-builder`, off `main@281a122`, baseline green: 895 tests pass).

**End-state gate (non-negotiable):** `cargo build/test/clippy --workspace --all-features` green +
`cargo fmt --check` + `cargo doc --no-deps` clean, AND the default-feature triplet (no
`--all-features`) green. Every intermediate commit also compiles + tests green.

**Type:** `type:refactor` (behavior-preserving). Proven so by an equivalence harness (Task 1/6), not
just asserted.

---

## 1. Design decisions (resolved)

Synthesized from three lens drafts in `docs/plans/2026-06-15-engine-builder-drafts/`
(mvp-first, risk-first, architecture-first).

### D1 — Builder shape: consuming `self -> Self`, `build(self) -> Result<MemoryEngine>`

Idiomatic owned builder. Forced by the reranker: `Box<dyn Reranker>` is **not `Clone`**, so a
`&mut self` builder would need an `Option::take` dance at `build()`. Owned `self` moves the reranker
straight into `init_from_pool`. The builder is consequently **not `Clone`** — correct and intended
(builders are one-shot). No `Clone` bound leaks onto consumer `Reranker` impls.

### D2 — Backing via typestate (split-state payload), in-memory as the zero-cost default

Single entry point `MemoryEngine::builder(embed_dim) -> MemoryEngineBuilder<InMemory>`. `.path(p)`
transitions `InMemory -> File`. File-only setters (`read_only`, `backup_dir`, `read_pool_size`) exist
**only** on `MemoryEngineBuilder<File>`, so `builder(d).read_only(true)` is a **compile error** — the
nonsensical "in-memory + read*only/backup_dir" state is structurally unrepresentable (the pool's
`open_memory` hardcodes `read_only=false`, `backup_dir=None`). `embed_dim` is always required and
authoritative — this dissolves the "embed_dim redundant with config" problem because the engine
builder no longer \_takes* a pre-built `EngineConfig`; it **builds one internally** for the file path
(the user-mandated unification with #149).

**Split-state payload** (preferred over flat `Option<PathBuf>` + `expect`):

```rust
mod sealed { pub trait Backing {} }
pub struct InMemory(());                               // ZST
pub struct File { path: PathBuf, read_only: bool, backup_dir: Option<PathBuf>, read_pool_size: usize }
impl sealed::Backing for InMemory {}
impl sealed::Backing for File {}

#[must_use = "a builder does nothing until `.build()` is called"]
pub struct MemoryEngineBuilder<B: sealed::Backing = InMemory> {
    embed_dim: usize,
    search_config: Option<SearchConfig>,      // CAPS — both backings
    reranker: Option<Box<dyn Reranker>>,      // CAPS — both backings; non-Clone
    upcaster_registry: UpcasterRegistry,      // CAPS — both backings
    backing: B,                               // InMemory ZST or File{ path, knobs }
}
```

`path` is a non-`Option` `PathBuf` living _inside_ the `File` payload, so "file knobs unrepresentable
for in-memory" is literally true at the data level — no `expect`, no unreachable branch.

### D3 — Reranker / search_config / upcaster: CAPS setters on `impl<B: Backing>`, applied to both backings

Forwarded **verbatim** into the existing private `init_from_pool(pool, embed_dim, search_config,
upcaster_registry, reranker)`. The builder MUST NOT reimplement `init` — it only selects the pool and
forwards the same 5 args. This is the central behavior-preservation guard: the only thing that varies
vs the old constructors is pool selection, which the old constructors already encoded.

### D4 — `EngineConfig` (#149): `#[non_exhaustive]` + seal fields + `new` + owned-`self` `with_*` chain

- `#[non_exhaustive]` so future fields are non-breaking.
- **Seal fields** (remove `pub`); add chained setters `with_read_pool_size`, `with_search_config`,
  `with_backup_dir`, `with_upcaster_registry`, `with_read_only`. Keep `EngineConfig::new(path, dim)`.
- **STAGED sealing (mandatory — cross-model BLOCKER fix, Codex + agy):** the `#[non_exhaustive]` +
  `with_*` additions are purely additive and land in **Task 3** (fields stay `pub`, nothing breaks).
  Removing `pub` (the actual seal) is a **breaking** change that does NOT compile until every
  `config.<field> = …` field-poke site is migrated to `with_*` — so the `pub` removal is deferred to
  **Task 6**, after Task 4 (core pokes) and Task 5 (cli pokes) have migrated them. Sealing in Task 3
  would break Task 3's own `cargo test --workspace` gate (green-at-every-commit violation, R2/R8).
- **No separate `EngineConfigBuilder` type** — the `with_*` chain _is_ the builder pattern #149 asks
  for; a second named builder would be the "competing builders" anti-pattern. `MemoryEngineBuilder<File>`
  is the ergonomic front door; `EngineConfig` is the plain-data DTO that `restore_*`/async consume and
  the builder produces internally.
- `MemoryEngineBuilder<File>::into_config(self) -> EngineConfig` surfaces the assembled config for
  callers that need the value (drops the reranker — `EngineConfig` has no reranker field; documented).

> **Decision point flagged for reviewer/user veto (lead-with-ideal + pragmatic alternative):**
> _Sealing the fields_ is an extra public-API break beyond the agreed "remove the 5 constructors"
> (it breaks the `config.read_only = true` field-poke idiom — verified live only in
> `memory-engine-cli` and a few core tests; **zero** `EngineConfig { .. }` struct literals exist in
> the workspace). It is the architecturally ideal close of #149 (real encapsulation) and the churn is
> already inside the migration we are doing. **Pragmatic alternative if you prefer minimal churn:**
> keep fields `pub`, add `#[non_exhaustive]` + the `with_*` chain only (sealing deferred to a
> follow-up). The plan proceeds with **seal** (staged, per above — `pub` removed in Task 6); switching
> to keep-pub is a one-step delta (skip the Task 6 `pub` removal; `#[non_exhaustive]` + `with_*` from
> Task 3 still close #149).

### D5 — `read_only`/`backup_dir` on in-memory: compile-time (typestate), per D2. No runtime error branch.

### D6 — Public `open(&EngineConfig)` becomes private `open_from_config`; restore/async share it

The old `open` body survives as a private shared file funnel:
`fn open_from_config(config: &EngineConfig, reranker: Option<Box<dyn Reranker>>) -> Result<Self>`.

- `MemoryEngineBuilder<File>::build()` → `Self::open_from_config(&self.into_config_without_reranker(), self.reranker)`.
- `restore_sqlite` (restore.rs:157, currently calls `Self::open(config)`) → `open_from_config(config, None)`.
- `AsyncMemoryEngine::open` internals → route through the builder / `open_from_config`.
  This honors "remove all 5 **public** constructors" (the public `open` is one of them) while keeping the
  single `EngineConfig -> pool -> init_from_pool` mapping in exactly one place (best for R1/R3). It is
  the natural seam if restore is later folded into the builder.

### D7 — `MemoryEngineBuilder` lives in a new public submodule `src/engine/builder.rs`

`pub mod builder;` in `mod.rs`; re-export `MemoryEngineBuilder` (+ `InMemory`/`File` markers for
docs) at crate root in `lib.rs`. `init_from_pool` and `open_from_config` stay **private** in `mod.rs`
— a sibling module in the same crate can call private associated fns, so no visibility bump needed.

### D8 — Async: keep the public surface, reroute internals; defer `build_async` to a follow-up

`AsyncMemoryEngine::open(EngineConfig)` / `open_memory(embed_dim)` stay (they are a _separate_ surface,
not part of the 5 telescoping constructors). Their bodies reroute through the sync builder /
`open_from_config` inside `spawn_blocking`. This keeps the refactor behavior-preserving and bounded.
A symmetric `MemoryEngineBuilder::build_async()` (which would additionally let async wire a reranker —
a capability the async surface lacks today) is a **strict improvement but a new capability**; filed as
a follow-up (`type:enhancement` / `area:core`), not folded in.

### D9 — Restore family stays as-is (out of #541 scope)

`restore_json` / `restore_json_memory` / `restore_sqlite` keep their `&EngineConfig`/`&Path`
signatures. Only `restore_sqlite`'s internal `Self::open(config)` call rewires to `open_from_config`
(D6). Folding restore into `MemoryEngineBuilder::restore_from(..)` is the architectural ideal but
expands scope materially — filed as a follow-up (`type:refactor` / `area:storage`).

### Target public API (sketch)

```rust
// src/engine/builder.rs
impl MemoryEngine {
    pub fn builder(embed_dim: usize) -> MemoryEngineBuilder<InMemory> { /* defaults */ }
}
impl<B: sealed::Backing> MemoryEngineBuilder<B> {           // CAPS — both backings
    pub fn search_config(mut self, c: SearchConfig) -> Self { /* … */ }
    pub fn reranker(mut self, r: Box<dyn Reranker>) -> Self { /* … */ }
    pub fn upcaster_registry(mut self, u: UpcasterRegistry) -> Self { /* … */ }
}
impl MemoryEngineBuilder<InMemory> {
    pub fn path(self, p: impl Into<PathBuf>) -> MemoryEngineBuilder<File> { /* transition */ }
    pub fn build(self) -> Result<MemoryEngine> { /* open_memory pool → init_from_pool */ }
}
impl MemoryEngineBuilder<File> {                            // file-only knobs
    pub fn read_only(mut self, ro: bool) -> Self { /* … */ }
    pub fn backup_dir(mut self, d: impl Into<PathBuf>) -> Self { /* … */ }
    pub fn read_pool_size(mut self, n: usize) -> Self { /* … */ }
    pub fn into_config(self) -> EngineConfig { /* drops reranker */ }
    pub fn build(self) -> Result<MemoryEngine> { /* open_from_config(&cfg, reranker) */ }
}
```

Common sites read: `MemoryEngine::builder(768).build()?` (in-memory) /
`MemoryEngine::builder(768).path("a.db").read_only(true).build()?` (file).

### D6-table — old constructor → new call (this IS the migration map + the Task-1 oracle)

| Old constructor                  | New call                                                                                     | Pool                | search_config       | upcaster                | reranker |
| -------------------------------- | -------------------------------------------------------------------------------------------- | ------------------- | ------------------- | ----------------------- | -------- |
| `open(&cfg)`                     | `builder(cfg.embed_dim).path(cfg.path)…build()` _or_ internal `open_from_config(&cfg, None)` | per `cfg.read_only` | `cfg.search_config` | `cfg.upcaster_registry` | `None`   |
| `open_memory(d)`                 | `builder(d).build()`                                                                         | `open_memory(d)`    | `None`              | `new()`                 | `None`   |
| `open_memory_with_config(d, sc)` | `builder(d)[.search_config(sc?)].build()`                                                    | `open_memory(d)`    | `sc`                | `new()`                 | `None`   |
| `open_with_reranker(&cfg, rr)`   | `builder(cfg.embed_dim).path(cfg.path)…[.reranker(rr?)].build()`                             | per `cfg.read_only` | `cfg.search_config` | `cfg.upcaster_registry` | `rr`     |
| `open_memory_with(d, sc, rr)`    | `builder(d)[.search_config(sc?)][.reranker(rr?)].build()`                                    | `open_memory(d)`    | `sc`                | `new()`                 | `rr`     |

`[.x(v?)]` = applied only when the migrated site's `Option` is `Some` (mechanical sites have concrete
`None`/`Some`, so this collapses to a literal at each site; the transient shims in Task 3 handle the
runtime `Option` via `if let Some(v) = opt { b = b.x(v); }`).

---

## 2. Risk register (lead with these; each task retires one)

| ID     | Risk                                                                                                                                                                      | Mitigation (task)                                                                                                                                                                           |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **R1** | Builder defaults silently diverge from old constructors (`read_pool_size` default 4, embed_dim validate-vs-set, `search_config` None/Some, empty upcaster, reranker None) | **Task 1** golden equivalence harness frozen against OLD constructors; **Task 3** shim makes the whole existing suite run through the builder; **Task 6** re-point harness → byte-identical |
| **R2** | Non-compiling intermediate across ~226 sites / 3 crates blocks the workspace                                                                                              | **Task 2→7** additive introduce → shim → migrate per-crate → delete-last; full-workspace green gate ending each task                                                                        |
| **R3** | Feature-gated breakage: `ann` (`hnsw_strategy`, `search_config`), `async`; `--all-features` and default must both pass                                                    | **D3** (search_config flows unchanged into `init_from_pool`); **Task 8** both feature configs + `ann`/`async` spot builds in the gate                                                       |
| **R4** | `read_only`/`backup_dir` on in-memory nonsensical                                                                                                                         | **D2/D5** typestate — compile error; trybuild + `compile_fail` doctest prove it (Task 5)                                                                                                    |
| **R5** | Async + `restore_*` entry points missed → cli/mcp/async break                                                                                                             | **Task 0** exhaustive entry-point census; **Task 5** explicit async/restore handling (D6/D8/D9)                                                                                             |
| **R6** | Doctests / rustdoc referencing removed names break `cargo test --doc` / `cargo doc`                                                                                       | **Task 7** doc sweep; `cargo doc --no-deps` in the gate; fix intra-doc links (e.g. `async_engine.rs:29,64,82`)                                                                              |
| **R7** | Scripted rewrites hit false matches — `ConnectionPool::open_memory`, `schema::open_memory()` (no-arg), pool internal `open_memory` are DIFFERENT symbols                  | **Task 4** receiver-anchored `Edit` only (`MemoryEngine::`/`AsyncMemoryEngine::` prefix); never blind `sed` on bare `open_memory`                                                           |
| **R8** | Sealing `EngineConfig` fields (D4) breaks more sites than censused                                                                                                        | Grep `config\.<field>\s*=` and `EngineConfig\s*{` before Task 3 and again at the gate; the list is exhaustive (zero struct literals confirmed)                                              |
| **R9** | trybuild `.stderr` snapshots are toolchain-sensitive (churn)                                                                                                              | Lead with `compile_fail` **doctests** (looser matching); trybuild `.stderr` is the secondary, pinned to the repo toolchain, regenerated with `TRYBUILD=overwrite`                           |

---

## 3. Implementation tasks

- [ ] **Task 0 — Entry-point census + plan-as-issue (retires R5 recon).**
  - Enumerate every engine-construction entry point, line-cited, in the PR description:
    core sync (`mod.rs` 5 ctors), core restore (`restore.rs` 3), async (`async_engine.rs` open/open_memory
    - 3 restore wrappers), `EngineConfig::new`, sibling-crate sites (cli `src/db.rs`/commands/examples/tests,
      mcp `src/main.rs`/tests, embed).
  - List the **false-match symbols to never rewrite**: `ConnectionPool::{open, open_memory, open_read_only}`,
    `schema::open_memory()` (no-arg).
  - Publish this plan verbatim as a `type:plan` + `area:core` GitHub issue titled
    `plan(core): replace telescoping MemoryEngine constructors with a builder`, linking #541/#113/#149.
  - **Gate:** census complete + plan issue filed. No code yet.

- [ ] **Task 1 — Golden equivalence harness FIRST, against the OLD constructors (retires R1).**
  - New integration test `tests/engine_builder_equivalence.rs`. For each of the 5 constructors, open an
    engine and snapshot its observable config tuple — **extended per Codex [HIGH]** to prove the full
    `EngineConfig -> into_config -> open_from_config` round-trip, not just that the engine opens:
    `(embed_dim, is_file_backed, is_read_only, read_pool_size, backup_dir_is_some, search_config_effective, upcaster_len, reranker_name)`
    where `search_config_effective` snapshots the resolved `SearchConfig` value (e.g. its `ann_threshold`
    / serialized form), **not** merely `is_some` — so a precedence or value regression is caught.
    Inspectors: `embed_dim()`, `reranker_name()`, `active_strategy_name()`; add `pub(crate)` test getters
    `pool.read_pool_size()`, `pool.is_read_only()`, and a path/`backup_dir` presence probe if absent.
  - Behavioral pins: re-`open(&cfg)` with a different `embed_dim` → `MemoryError::Migration`; read-only
    open of a missing file → error pre-migration. Snapshot the error **variant**.
  - **Pin the File+reranker case**: the `open_with_reranker(&cfg, Some(rr))` snapshot MUST assert
    `reranker_name() == Some(..)`, so that after migration the "reranker flows into `open_from_config`'s
    2nd arg, not into `into_config` (which drops it)" wiring is proven, not just that the engine opens.
  - Use `insta`; **commit the snapshots now, while only old constructors exist** — the frozen oracle.
  - **Gate:** `cargo test --workspace` + `--all-features` green; snapshots committed.

- [ ] **Task 2 — Introduce `MemoryEngineBuilder` (typestate) + `open_from_config`, additive (retires R2-a, R3, R4).**
  - Create `src/engine/builder.rs` with `sealed::Backing`, `InMemory`/`File` markers (split-state payload),
    `MemoryEngineBuilder<B>`, `MemoryEngine::builder`, `.path` transition, CAPS setters, file-only setters,
    `build()` per state, `into_config`. `pub mod builder;` in `mod.rs`; re-export from `lib.rs`.
  - Add private `MemoryEngine::open_from_config(&EngineConfig, Option<Box<dyn Reranker>>)` = old `open`
    body. File-state `build()` routes through it.
  - **`build()` selects the pool exactly as the old constructors and calls the existing
    `init_from_pool` with the same 5 args — no `init` reimplementation.**
  - Old 5 constructors untouched. **Gate:** default + `--all-features` build/test/clippy/fmt green.

- [ ] **Task 3 — Refit `EngineConfig` (#149, D4, ADDITIVE only) + shim the 5 old constructors (retires R1, R2-b).**
  - `#[non_exhaustive]` on `EngineConfig`; add `with_*` chained setters; keep `new`. **Fields stay `pub`
    in this task** — sealing (removing `pub`) is deferred to Task 6 (cross-model BLOCKER fix: sealing
    here would break the not-yet-migrated `config.<field> =` sites at this task's own gate).
  - Reduce the 5 old constructors to one-line bodies delegating to the builder / `open_from_config`
    (handle their runtime `Option` params via `if let`). One open code path; existing suite now exercises
    the builder unchanged.
  - Re-run Task-1 harness → must still match frozen snapshots (**equivalence proof #1**).
  - **Gate:** `cargo test --workspace` + `--all-features` green, snapshots unchanged.

- [ ] **Task 4 — Migrate CORE call sites (retires R2-c, R7).**
  - Receiver-anchored `Edit` (never `sed` bare `open_memory`): `src/engine/tests.rs` (~96 hits, all 13
    reranker variants), `src/engine/{dormant,cognitive,lineage}.rs`, `src/inspect/*`, `benches/*`,
    core `tests/*.rs`. `config.read_only = true` / `config.backup_dir =` sites → `with_*` or the
    file-state builder.
  - **Gate:** default + `--all-features` build/test/clippy/fmt green. Shims still present.

- [ ] **Task 5 — Migrate sibling crates + async + restore + re-point harness (retires R1, R5, R6).**
  - `memory-engine-cli` (`src/db.rs` read*only/backup_dir idioms → builder/`with*\*`; commands; examples; tests).
  - `memory-engine-mcp` (`src/main.rs`; tests). `memory-engine-embed` (grep-confirm; likely test-only).
    Switch imports from `memory_engine::engine::{EngineConfig, MemoryEngine}` to crate-root re-exports.
  - **Async** (`src/async_engine.rs`): reroute `open`/`open_memory` internals through the builder /
    `open_from_config` (D8); keep public surface. **In the SAME commit**, rewrite the rustdoc that will
    dangle after Task 6: the intra-doc links `[\`MemoryEngine::open\`]` (`:64`) and
`[\`MemoryEngine::open_memory\`]` (`:82`), the "Returns errors from `[MemoryEngine::open]`" lines, and
the ignored doctest `MemoryEngine::open_memory(768)` (`:29`). This keeps Task 6's `cargo doc` gate
    clean (review finding: links land in Task 6 but were scheduled for fix in Task 7 — pulled forward).
  - **Restore** (`restore.rs`): `restore_sqlite` internal `Self::open(config)` → `open_from_config(config,
None)` (D6) — **preserve the surrounding `.inspect_err(|_| remove_file)` orphan-cleanup wrapper**;
    signatures unchanged (D9).
  - **Re-point `tests/engine_builder_equivalence.rs`** at direct builder calls (D6 table) → snapshots
    **byte-identical** (**equivalence proof #2**; any diff = stop & fix builder, not snapshot).
  - **Gate:** `cargo build/test -p {cli,mcp,embed}` + full workspace, default + `--all-features`,
    `cargo test --doc` green.

- [ ] **Task 6 — Seal `EngineConfig` fields + delete the 5 shims (final breaking commit, retires R2/R8).**
  - **Seal now (deferred from Task 3 per the cross-model BLOCKER fix):** remove `pub` from `EngineConfig`
    fields. Safe at this point because Task 4/5 migrated every `config.<field> =` poke to `with_*`. (If
    the keep-`pub` veto was taken, skip this bullet.)
  - Remove `open`, `open_memory`, `open_memory_with_config`, `open_with_reranker`, `open_memory_with`
    from `mod.rs`. Keep private `open_from_config` + `init_from_pool`. `cargo build` surfaces any missed
    site as a hard error — the intended final sweep.
  - **Gate:** full workspace default + `--all-features` build/test/clippy/fmt/doc green.

- [ ] **Task 7 — Clippy/fmt/doc pass on new code.**
  - `MemoryEngineBuilder` under pedantic+nursery: `must_use_candidate`, `return_self_not_must_use`,
    `missing_const_for_fn` — add `#[must_use]`, justify any `#[allow]`. Confirm all intra-doc links are
    clean (the dangling `async_engine.rs` references were already fixed in Task 5).

- [ ] **Task 8 — Feature-gated verification matrix (retires R3, cross-model fix: agy [MEDIUM]).**
  - Run the full §6 command set, both default-feature and `--all-features`, **plus** single-feature spot
    builds `cargo build --features ann` / `--features async` (the two features that touch construction).
  - **Include the `ann`-disabled + `SearchConfig`-provided case** (Gemini [MEDIUM]): a default-feature
    build/test that constructs `builder(d).search_config(c).build()` with `ann` OFF, asserting it compiles
    and runs (the builder must not force-link HNSW symbols when `ann` is absent).
  - **Gate:** entire matrix green; capture the output for the PR.

---

## 4. Documentation (mandatory)

- [ ] **Module rustdoc** on `src/engine/builder.rs`: the typestate (in-memory default → `.path` promotes
      to file → file-only knobs become callable), why the builder is non-`Clone` (reranker), and the
      "one setter per future capability" extensibility contract (capability growth O(1) additive vs the
      O(2ⁿ) constructor explosion #541 kills — headline this in the PR).
- [ ] **Runnable doctests** on `MemoryEngine::builder` and each setter: one in-memory
      (`MemoryEngine::builder(4).build()?`) and one file (`tempfile::tempdir()`) — pass `cargo test --doc`.
- [ ] **`compile_fail` doctest** showing `MemoryEngine::builder(4).read_only(true)` is rejected
      (documents the type-safety win inline; checked by `cargo test`).
- [ ] **`EngineConfig` rustdoc**: new role as the file-construction DTO produced by the builder;
      `#[non_exhaustive]` + `with_*` chain (#149).
- [ ] **ADR** in `docs/design/adr/`: "telescoping constructors → typestate builder" capturing D1–D9.
- [ ] **Narrative sweep** — replace every `open_memory*` / `EngineConfig { .. }` / `MemoryEngine::open(&cfg)`
      / `config.<field> =` snippet in: `docs/getting-started/quickstart.md`, `docs/usage/*.md`,
      `docs/advanced/{extensibility,hybrid-search}.md`, `docs/reference/{crate-layout,api}.md`,
      `README.md`, `GEMINI.md`. Add `engine/builder.rs` to the module map.
      **Do NOT touch** `docs/ROADMAP.md` (frozen), `docs/design/plans/*`, `docs/superpowers/plans/*`,
      `qa/super-qa/runs/*` (frozen historical artifacts).
- [ ] **Migration note** `docs/advanced/migration-builder.md`: before/after table for all 5 removed
      ctors + the `config.<field> =` → `with_*` change. The consumer's one-stop upgrade guide.

## 5. Testing (mandatory)

- [ ] **Per-axis unit tests** in `builder.rs` `#[cfg(test)]`: in-memory {minimal, +search*config,
      +reranker, +upcaster, all-caps}; file {minimal, +read_only (writes rejected), +backup_dir,
      +read_pool_size, full combo}; `into_config()` round-trips file fields; `EngineConfig` `with*\*`
      sets each field (covers #149 directly).
- [ ] **R1 equivalence golden tests** (Task 1 → Task 5): frozen `insta` snapshots, re-pointed, byte-identical.
- [ ] **R4 compile-fail**: `compile_fail` doctest (primary) + optional `trybuild` cases
      (`in_memory_read_only.rs`, `in_memory_backup_dir.rs`, `builder_not_clone.rs`) with toolchain-pinned
      `.stderr` (R9 — doctest is the durable backstop if `.stderr` churns).
- [ ] **R3 feature test** (`#[cfg(feature = "ann")]`): build with a low-`ann_threshold` `search_config`,
      assert `active_strategy_name()` flips to HNSW — proves the builder's `search_config` reaches the
      `#[cfg(feature="ann")]` HNSW build in `init_from_pool`.
- [ ] **Migrated suite = integration coverage**: all migrated sites (≈226 receiver-anchored
      `MemoryEngine::{open*,restore*}` calls, ~96 in `src/engine/tests.rs`) must pass with **no assertion edits**
      (only construction-site edits). Any assertion change to pass = drift → investigate, don't paper over.

## 6. Verification (exact commands — all green before PR)

```bash
cargo build  --workspace                         # default features
cargo build  --workspace --all-features          # async + ann + archive + compress
cargo build  --features ann                       # single-feature spot (construction-touching)
cargo build  --features async
cargo test   --workspace
cargo test   --workspace --all-features
cargo test   --workspace --doc                    # + --all-features --doc for the async doctest
cargo clippy --workspace --all-targets --all-features   # pedantic+nursery, no unjustified #[allow]
cargo clippy --workspace --all-targets
cargo fmt --check                                 # from worktree root (NOT --manifest-path: false-green)
cargo doc --no-deps                               # default-feature doc path clean
cargo doc --no-deps --all-features                # no broken intra-doc links (async/ann rustdoc)
cargo insta test                                  # NO pending .snap.new (equivalence proof intact)
# Zero-reference proof (must print nothing under src/, tests/, the 3 sibling crates):
#   Grep: open_memory_with | open_memory_with_config | open_with_reranker
#   Grep: MemoryEngine::open(  | MemoryEngine::open_memory(   (excl .worktrees/docs/qa)
```

## 7. Operational / git (mandatory)

- [ ] **Worktree** `.worktrees/refactor-541-engine-builder` (exists, clean baseline).
- [ ] **Plan-as-issue** (Task 0): `type:plan` + `area:core`, links #541/#113/#149.
- [ ] **Collateral issues** (per the labeling contract — do NOT fold into this PR):
      `enhancement(core): MemoryEngineBuilder::build_async + async reranker` (D8);
      `refactor(storage): fold restore family into MemoryEngineBuilder` (D9).
- [ ] **Atomic Conventional Commits**, each leaving the workspace green:
      (1) `test(core): freeze constructor-equivalence golden snapshots (#541)`
      (2) `feat(core): add typestate MemoryEngineBuilder + open_from_config (#541)`
      (3) `refactor(core): EngineConfig #[non_exhaustive] + with_* builder (#149); shim old ctors`
      (4) `refactor(core): migrate core call sites to the engine builder`
      (5) `refactor(cli,mcp): migrate sibling-crate construction to the builder`
      (6) `refactor(core)!: remove telescoping open* constructors (closes #541 #113 #149)`
      (7) `docs(core): document builder API + ADR + migration note`
      No co-author trailer.
- [ ] **PR** `refactor(core)!: replace telescoping MemoryEngine constructors with a builder`, body
      closes #541/#113/#149, headlines the O(2ⁿ)→O(1) capability-growth win + the typestate safety win,
      includes the before/after migration table, marks the breaking change (`!`). Expect
      `gemini-code-assist[bot]` auto-review (type-blind FPs on the typestate generic / non-`Clone`
      builder — triage in finish-pr).
- [ ] **`/super-review`** (cross-model) then **finish-pr**; resolve findings or file collateral issues.
- [ ] **Squash merge** to `main`.

## 8. Post-Implementation Audit

(To be appended after implementation per address-issue Feature-Researched step 5 — per-item
Implemented/Modified/Dropped/Added status.)
