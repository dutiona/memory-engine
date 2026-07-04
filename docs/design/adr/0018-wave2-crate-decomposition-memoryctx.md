# ADR 0018: Wave 2 — per-concern crate decomposition + `MemoryCtx`

**Status:** Accepted (Wave 2, #816). S1 (keystone + L0/L0.5/L1 leaves) and S2 (backends +
projections: `me-index`, `me-backend-sqlite`, `me-backend-postgres`) landed; S3–S6 pending.
**Date:** 2026-07-01

## Context

The core was **one crate** (`memory-engine`, ~18 modules; `engine` ~17k LOC and
`store` ~14k LOC were god-modules) and **not acyclic** — 5+ structural cycles, all
back-edges into `engine`, caused by two misplaced DTO groups (`engine::snapshot`,
`engine::cycle`) physically living in `engine`. Wave 1 (#814) made the _directory_
layout explicit; Wave 2 makes the _architecture_ explicit: an **acyclic DAG of
per-concern crates** so the library/binary boundary, the link graph, and the
public/private surface are visible rather than "Cargo magic". #631 already did the
hard part — write-serialization, the pool, HNSW, and all transactional atomicity live
behind `Arc<dyn StorageBackend>` (6 bounded traits + `ColdStorage`), and the facade is
already thin.

The design + implementation plan were built via a 3-lens `/super-plan` synthesis with
Codex + agy + clean-slate review, and are archived verbatim in **#925**. This ADR
records the **locked structural commitments** (plan §0) so the #761-family epics that
build on these boundaries have the durable _why_.

## Decision

1. **13-crate strict-downward DAG.** `me-types` (L0) ← `me-traits` (L0.5) ←
   `me-storage` (L1) ← {`me-backend-sqlite`, `me-backend-postgres`, `me-index`} (L2) ←
   {`me-ingest`, `me-query`, `me-consolidate`, `me-forget`, `me-resolve`, `me-archive`}
   (L3) ← `memory-engine` facade (L4). Edges point strictly down by layer; `cargo`
   rejects any re-introduced cycle at resolve, and the CI `cargo tree` check catches a
   back-edge before merge. A 14th crate `me-test-support` is dev-only (`publish=false`,
   `[dev-dependencies]` everywhere), so it does not inflate the shipped graph.

2. **`me-types` is the name (not `me-core`); `error` folds into it.** L0 is the only
   crate every other crate may depend on. `error` and `types` are mutually dependent
   (`MemoryError` names domain types; domain ops return `MemoryError`), so they are one
   crate. `me-types` also owns the relocated `snapshot`/`cycle-report`/search-result
   DTO vocabularies + `limits`.

3. **`me-storage` (L1) owns `MemoryCtx`.** `MemoryCtx<'a>` is a `Copy` borrow-bundle of
   the **universal** capabilities every primitive needs: `&Arc<dyn StorageBackend>` +
   `embed_dim` + `read_only` + the `&AtomicUsize` reconstruction fence, with
   `ensure_open`/`ensure_writable` gates relocated verbatim from `engine::mod`.
   Per-primitive extras (`graph`, `scope_tree`, `reranker`, `cold`, `db_path`) are
   **loose parameters**, so each free-fn signature _declares_ exactly which extra
   capabilities it uses. It is homed in `me-storage` (not a 14th `me-ctx` crate, not the
   facade) because its load-bearing field is the storage port; the resulting
   `me-storage → me-traits` edge (for the `Reranker`/`ColdStorage` names in the extras)
   is acyclic (L1 → L0.5). Defined in S1; the L3 primitives consume it in S3/S4.

4. **`me-index` stays a separate L2 crate** (graph + scope projections), not folded into
   `me-backend-sqlite`: projections are backend-agnostic, and #763's freshness registry
   - #247/#243 context work want a backend-free, trait-free, mockable home. `me-index`
     depends on `{me-storage, me-types}` only (no `me-traits`).

5. **Orphan-module homes:** `limits` → `me-types`; `upcaster` (`UpcasterRegistry`) →
   `me-storage` (event-payload versioning the port applies on read; both backends
   consume one definition); `resume` → facade; `pool` → `me-backend-sqlite`;
   `test_utils` → `me-test-support`.

6. **embed consumes the facade (Option A).** `memory-engine-embed` reaches traits/types
   _through_ the facade (`memory_engine::traits::*`, `memory_engine::types::*`), so the
   facade must re-export `traits`/`types` as **modules**, keeping one stable four-layer
   seam and not forcing consumers to learn internal crate names.

7. **`publish` partition.** The 11 internal crates are `publish = false` (bare path-deps,
   exempt from cargo-deny's wildcard rule); only `memory-engine` (facade) and
   `memory-engine-embed` publish. The publishable crates carry **versioned** path-deps on
   their internal deps (cargo-deny's wildcard rule does not exempt public crates).

8. **Three deliberate, gated public-API breaks** (`cargo public-api` in the per-slice gate,
   this ADR the record): (a) **`DreamCycle::run(&dyn CycleCtx)`** replacing
   `&CycleContext` — necessary so `me-traits` does not depend on `me-consolidate` (the DTO
   move alone leaves that cycle); `CycleCtx` is a `me-traits` capability trait enumerating
   the exact read-set the shipped `DefaultDreamCycle`/`LlmDreamCycle` use, and
   `me-consolidate`'s `CycleContext` _implements_ it. Landed in **S1** (keystone).
   (b) **`VectorSearchStrategy::search`** taking a storage port (`&dyn SearchIndex`)
   instead of `&Connection` — deferred to **S4** (still `&Connection` after S1).
   (c) **`MemoryError::Database` variant removed** (#926, at **S2**-start) — the SQLite
   driver's `rusqlite::Error` no longer appears in the L0 public error enum; backend driver
   errors now surface via `MemoryError::Storage(StorageError::Backend(String))`, mapped at
   each backend call site through the new `StorageError::backend(impl Display)` helper. The
   orphan rule previously pinned `impl From<rusqlite::Error> for MemoryError` — and thus
   `rusqlite` + bundled SQLite — into L0 `me-types`; removing the variant lets `me-types`
   drop `rusqlite` entirely (unblocking #633's `dep:rusqlite` gating). `#[non_exhaustive]`
   on `MemoryError` softens the impact — only explicit `Database(_)` arms break, not
   exhaustiveness. The `storage::sqlite::map_seam_err` boundary remap is deleted (subsumed:
   the guarantee moves from a runtime match to compiler-enforced source-mapping). `§M`'s
   "public API unchanged" is amended to "unchanged **except** these three, gated".

## Consequences

- **Acyclicity is compiler-enforced** — the primary invariant. `cargo tree` +
  per-crate `//!` "depends only on {…}" docs + a reviewer lens back it up.
- **`pub(crate)` → `pub` widening** (~21, dominated by S2's `store` table-accessor
  constructors; S1 widened `SearchQuery` fields, `registered_count`, and the relocated
  DTO/seam-type fields). Each internal crate is `publish=false`, so "public" =
  workspace-internal.
- **No behavior change** — every slice is move + rewire + test-migration;
  `reexports_are_accessible` stays green and there is no test-count regression
  (`anti-#903`), asserted at every commit.
- **S1 + S2 done:** `me-types`/`me-traits`/`me-storage` exist as true leaves; all cycles
  broken; `MemoryCtx` defined. `me-index` (backend-free `MemoryGraph`/`ScopeTree`
  projections), `me-backend-sqlite` (`SqliteBackend`: store + pool + search + snapshot
  I/O), and `me-backend-postgres` (`PgBackend` skeleton, #633, optional behind
  `backend-postgres`, zero back-deps on `me-backend-sqlite`) are carved as separate L2
  leaves — three independently-gated sub-PRs under the #930 slice tracker. The CLEAN
  primitives (S3), MODERATE primitives (S4), facade shrink (S5), and dylib spike (S6)
  are fast-follow slices.
- **`cargo public-api` caveat:** for a crate decomposition its raw diff is
  representation-noise-dominated (relocated types + module re-exports enumerate under the
  sub-crates, not the facade), so the consumer-facing contract is guarded by
  `reexports_are_accessible` + the fact that cli/mcp/embed compile — the tool's value is
  making the _deliberate_ signature breaks greppable.
- **`test-util` is the SOLE switch for the cross-crate test seam** — the port's test-only
  hooks (`SchemaManager::raw_exec`) are gated `#[cfg(feature = "test-util")]`, **not**
  `cfg(any(test, feature = "test-util"))`. `cfg(test)` is per-crate — never set for a
  dependency — so once the trait decl (`me-storage`) and its impls + consumers (facade)
  split across crates, a `test` disjunct desyncs them: `cargo build --workspace --tests`
  with default features drops the decl while the facade's `cfg(test)` pulls the impl in
  (`E0407`), and a naive dev-dep forcing `me-storage/test-util` inverts it to `E0046` (decl
  without impl on the plain-lib target, since a dependency's feature never enables the
  facade's own). The fix makes the feature the single source of truth: the facade forwards
  `test-util = ["me-storage/test-util"]`, and the #632 conformance battery + the one
  `raw_exec`-driving engine test that _consume_ the seam are gated
  `cfg(all(test, feature = "test-util"))` / `cfg(feature = "test-util")`. Consequence:
  they run under `--all-features` (CI's test job) but not bare `cargo test`. This was the
  **S1 review-remediation A1 finding** — the original S1 gate missed it because it never
  ran the CI MSRV job's exact `cargo build --workspace --tests --examples` (default
  features), the one command that exercises the desync.
- **S1 review-remediation B1 (bootstrap idempotency atomicity):** the bootstrap-seam trim
  (Opt 2) split the `skip_existing` count (facade, read pool) from the marker insert
  (`ingest_bootstrap_batch_atomic`, write pool), losing the pre-#816 check-and-write
  atomicity → a TOCTOU where concurrent same-session bootstraps could double-insert a
  marker. Fixed by giving the port primitive a `skip_if_present: Option<&EventFilter>`
  guard checked _inside_ the savepoint and a `BootstrapIngestOutcome::{Skipped, Ingested}`
  return; the facade `count_events` early-out stays as a cheap embed-skip optimization.
  This is internal-port evolution (consumers call the `MemoryEngine` facade, not the port
  trait), guarded by `reexports_are_accessible` + cli/mcp/embed builds like the rest of the
  carve — not a consumer-facing break, so point 8's "unchanged except three" still holds.

## References

- Design + implementation plan (3-lens synthesis + Codex/agy/subagent review), verbatim:
  [memory-engine#925](https://github.com/dutiona/memory-engine/issues/925).
- Epic: [memory-engine#816](https://github.com/dutiona/memory-engine/issues/816).
- Predecessor: Wave 1 (#814). Builds on the `Arc<dyn StorageBackend>` port (#628/#631).
- Follow-up: `rusqlite`-off-`me-types` decoupling ([#926](https://github.com/dutiona/memory-engine/issues/926)).
