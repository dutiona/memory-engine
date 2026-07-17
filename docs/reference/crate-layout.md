# Crate Layout

Where things live in the `memory-engine` **workspace**. Wave 2 (#816) **decomposed** the
former single core crate into an acyclic DAG of per-concern crates. **S1–S5 are done** —
all **18 crates** exist:

- **L0** `me-types` · **L0.5** `me-traits` · **L1** `me-storage` (the port)
- **L2** `me-index`, `me-backend-sqlite`, `me-backend-postgres`
- **L3** `me-ingest`, `me-query`, `me-consolidate`, `me-forget`, `me-resolve`,
  `me-archive`, `me-cognitive`
- **L4** `memory-engine` (the facade)
- plus `me-test-support` (dev-only) and three facade consumers: `memory-engine-cli`,
  `memory-engine-mcp`, `memory-engine-embed`

Only **S6** (#942 — the `dylib` ship-mode spike + legibility tooling) remains. This page is
the "where does X live _now_" reference: the crate DAG, a per-crate table, and the module
map of what lives inside the facade.

The locked structural decisions are in
[ADR 0018](../design/adr/0018-wave2-crate-decomposition-memoryctx.md).

## Target crate DAG

Edges point strictly **down** by layer — a crate may depend only on crates in a lower
layer. `cargo` rejects any re-introduced cycle at resolve time, and the CI `cargo tree`
check catches a back-edge before merge. This acyclicity is the primary invariant of the
decomposition.

```{mermaid}
graph TD
    subgraph L4["L4 — facade"]
        facade["memory-engine"]
    end
    subgraph L3["L3 — primitives"]
        ingest["me-ingest"]
        query["me-query"]
        consolidate["me-consolidate"]
        cognitive["me-cognitive"]
        forget["me-forget"]
        resolve["me-resolve"]
        archive["me-archive"]
    end
    subgraph L2["L2 — backends + projections"]
        sqlite["me-backend-sqlite"]
        postgres["me-backend-postgres"]
        index["me-index"]
    end
    subgraph L1["L1 — storage port"]
        storage["me-storage"]
    end
    subgraph L05["L0.5 — contracts"]
        traits["me-traits"]
    end
    subgraph L0["L0 — data + error"]
        types["me-types"]
    end

    facade --> ingest & query & consolidate & cognitive & forget & resolve & archive
    facade --> sqlite & postgres
    ingest & query & consolidate & cognitive & forget & resolve & archive --> storage & index
    sqlite & postgres --> storage
    storage --> traits
    storage --> types
    traits --> types
    index --> types
```

**The invariant this diagram encodes** — read it before changing any edge: an **L3
primitive depends on the _port_ (`me-storage`), never on a concrete backend.** The
**facade** is the only crate that selects a backend. That is what makes the backends
swappable (epic #628) and the primitives testable without one. `cargo` enforces it:
`cargo tree -p <L3 crate> --edges normal` must show no `me-backend-*`.

`me-index` (L2) depends on **`me-types` only** — not `me-storage`, not `me-traits` — so
the graph/scope projections stay backend-free, *storage*-free, trait-free, and mockable
(ADR 0018 decision #4). The conn-based bulk loaders that hydrate them live facade-side in
`engine/graph_load.rs`, precisely to keep that property. (Verified: `me-index/Cargo.toml`
lists `me-types` + `petgraph` and nothing else.)

**`me-test-support`** is dev-only (`publish = false`, in `[dev-dependencies]` everywhere)
and does not inflate the shipped graph.

## Target crates

Fourteen crates in the strict DAG, plus the dev-only `me-test-support`. The `memory-engine`
facade re-exports the extracted crates so the public four-layer seam
(`types` / `error` / `traits` / `storage`) is unchanged.

| Crate                    | Layer | Status                                              | Responsibility                                                                                                                                                                                                                                                             | Extracted from                                                             |
| ------------------------ | ----- | --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `me-types`               | L0    | ✅ **DONE**                                         | Pure data + error vocabulary: domain DTOs (`Fact`, `Event`, `NewFact`, …), the snapshot / cycle-report / search-result sidecar types, `MemoryError` + `Result`, and `limits`. The only crate with no internal deps.                                                        | `types/`, `error`, `engine::snapshot`, `engine::cycle::report`, `limits`   |
| `me-traits`              | L0.5  | ✅ **DONE**                                         | Consumer-implemented capability traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker`) + the `CycleCtx` read-set trait for `DreamCycle`.                                                                                 | `traits`                                                                   |
| `me-storage`             | L1    | ✅ **DONE**                                         | The persistence **port**: the `StorageBackend` trait family (6 bounded traits + `ColdStorage`), `FactFilter`/`TemporalFilter`, `MemoryCtx`, and `UpcasterRegistry`. No SQL string or driver type appears here.                                                             | `storage` (port half), `MemoryCtx` (from `engine::mod`), `store::upcaster` |
| `memory-engine` (facade) | L4    | ✅ **DONE** (thin, still holds unextracted modules) | The `MemoryEngine` facade + `EngineConfig`. Re-exports the extracted crates as modules (`types`, `error`, `traits`, `storage`) and — until S3–S5 — still physically owns the L3 primitives (see [Facade-internal modules](#facade-internal-modules-until-s3s5)). | — (the shrinking remainder)                                                |
| `me-backend-sqlite`      | L2    | ✅ **DONE**                                         | `SqliteBackend`: the SQLite impl of the port. Owns the `ConnectionPool` (N readers + 1 writer) and the `block_read`/`block_write`/`for_each_streamed` `spawn_blocking` seam.                                                                                               | `storage/sqlite`, `pool`, and the `store/` + `search/` SQL it delegates to |
| `me-backend-postgres`    | L2    | ✅ **DONE**                                         | `PgBackend`: the Postgres impl (`deadpool-postgres` pool + native-PG migration chain + `SchemaManager`; #633 skeleton, CRUD #634, search #635).                                                                                                                            | `storage/postgres`                                                         |
| `me-index`               | L2    | ✅ **DONE**                                         | Backend-agnostic in-memory projections: the `MemoryGraph` and `ScopeTree` caches. Trait-free, mockable; the #763 freshness registry wants this home.                                                                                                                       | `graph/`, `scope/`                                                         |
| `me-ingest`              | L3    | ✅ **DONE** (S4)                                    | The **Ingest** primitive: append-only event log writes.                                                                                                                                                                                                                    | `engine::ingest`                                                           |
| `me-query`               | L3    | ✅ **DONE** (S4)                                    | The **Query** primitive: hybrid FTS5 + vector + graph retrieval, RRF merge. Carried the `VectorSearchStrategy` break — resolved as an **un-export**, not the originally-planned signature change (see ADR 0018 §8b).                                                        | `search/`, `engine::query`                                                 |
| `me-consolidate`         | L3    | ✅ **DONE** (S4)                                    | The **Consolidate** primitive: the 3-pass dedup → cluster → global pipeline. At S4 this deliberately excluded the dream-cycle layer (`DreamContext` held `&MemoryEngine`, an L3→L4 back-edge); **S5's `me-cognitive` carve (below) resolves that** by inverting the bag into a trait, so the two are siblings now, not one waiting on the other. | `consolidation/`, `engine::consolidation`                                  |
| `me-cognitive`           | L3    | ✅ **DONE** (S5, #981)                              | The **Cognitive/dream-cycle** primitive (Phase 5a): `DreamCycle` produce/apply orchestration (`run_dream_cycle`/`run_dream_cycle_guarded`/`apply_cycle_report`), `CycleContext`, `DefaultDreamCycle`, `LlmDreamCycle`, and the pure DBSCAN core. Depends on `me-traits`'s new `DreamCtx` capability trait (the ADR 0014 decision #3 bag, restored). The engine's implementation stays in the facade, carried by the private `EngineDreamCtx(&MemoryEngine)` newtype — **not** `impl DreamCtx for MemoryEngine`, which would make an inherent-method rename a silent stack overflow (ADR 0014's S5 amendment). | `engine::cycle`, `engine::cognitive` (orchestration half)                  |
| `me-forget`              | L3    | ✅ **DONE** (S3)                                    | The **Forget** primitive: Ebbinghaus decay + multi-signal importance scoring.                                                                                                                                                                                              | `forgetting/`, `engine::forgetting`                                        |
| `me-resolve`             | L3    | ✅ **DONE** (S3)                                    | The **Resolve** primitive: bi-temporal conflict arbitration.                                                                                                                                                                                                               | `engine::conflict`                                                         |
| `me-archive`             | L3    | ✅ **DONE** (S4)                                    | The **Archive** primitive: cold-storage `.pak` snapshots (feature-gated). Owning `build_pak` here makes the `.pak` schema guard compiler-enforced: no backend edge exists, so neither half can name a backend's schema constant.                                            | `archive/`, `engine::archive`                                              |
| `me-test-support`        | dev   | ✅ **DONE** (S3)                                    | Cross-crate test-only helpers (`publish = false`, `[dev-dependencies]`).                                                                                                                                                                                                   | `test_utils`                                                               |

> Per-primitive slice assignment (which L3 crate is CLEAN vs MODERATE) is not pinned by
> the ADR beyond `me-query` landing in **S4** (it carries the `VectorSearchStrategy` break);
> the rest of the L3 six carve across **S3–S4**. The facade shrink is **S5**, and a
> `dylib` spike is **S6**.

### Deliberate, gated public-API breaks

Three signature breaks are intentional and guarded by `cargo public-api` in the per-slice
gate (ADR 0018 decision #8; see that decision for the full, amended history of each):

- **`DreamCycle::run(&dyn CycleCtx)`** (replacing `&CycleContext`) — **landed in S1**. It
  lets `me-traits` avoid naming whichever crate owns the cycle's read-set — that turned
  out to be `me-cognitive` (S5), not `me-consolidate` as originally assumed;
  `CycleContext` _implements_ the `CycleCtx` read-set trait wherever it lands.
- **`VectorSearchStrategy::search(&dyn SearchIndex)`** (replacing `&Connection`) —
  **never happened; superseded in S4** (ADR 0018 §8b). The `me-query` carve resolved the
  problem by *un-exporting* `VectorSearchStrategy` from the facade instead: it no longer
  crosses the port boundary at all, so its signature stopped being public API and the
  planned break became moot. It lives on as a `me-backend-sqlite`-internal HNSW-vs-brute-force
  dispatch trait (`me-backend-sqlite/src/search/strategy.rs`). See the `me-query` row above.
- **`DreamContext` deleted; `DreamCtx` trait added** (S5, #981) — the ADR 0014 decision
  #3 capability bag is inverted into a `me-traits` trait (`CycleCtx: DreamCtx`
  supertrait), unblocking the `me-cognitive` carve. See the `me-cognitive` row above and
  ADR 0018 decision #8(d).

## Facade-internal modules (until S3–S5)

Until the S3–S5 slices carve them out, the `memory-engine` facade physically contains the
following modules. They are reachable through the facade exactly as before; the paths below
are their homes _inside_ the facade crate today. Each row also notes its eventual target
crate where ADR 0018 assigns one — where it does not, the module stays facade-internal
until a later slice. (`store/`, `storage/sqlite/`, `storage/postgres/`, `graph/`, `scope/`,
and `pool/` — the S2 scope — are **no longer facade modules**; they physically live in
`me-backend-sqlite`, `me-backend-postgres`, and `me-index` now. See
[the extracted crates](#the-extracted-crates-s1--s2) below.)

`engine`
: Facade over all memory primitives. Defines `MemoryEngine` (the main entry point) and `EngineConfig`. The engine is async-native: its DB-touching methods are `async fn` that `.await` an `Arc<dyn StorageBackend>` port, so thread safety and blocking-IO offload live in the backend (`spawn_blocking`); the in-memory caches the engine still owns stay `RwLock`-protected. `engine::conflict` holds the bi-temporal `MemoryEngine::resolve_conflict` (delegated to the consumer `ConflictArbiter`) → **me-resolve**. `engine::query` → **me-query**; `engine::ingest` → **me-ingest**; `engine::forgetting` → **me-forget**; `engine::archive` → **me-archive**. `MemoryCtx` and its `ensure_open`/`ensure_writable` gates were relocated **out** of `engine::mod` into `me-storage` in S1.

`engine::cognitive` (Phase-5a dream-cycle subsystem, #49)
: `engine::cycle` (the former `CycleContext`/`DefaultDreamCycle`/`LlmDreamCycle`/`apply_cycle_report`/`dbscan` module tree) and the orchestration half of `engine::cognitive` → **me-cognitive** (S5, #981). What stays in the facade's (now much smaller) `engine::cognitive`: `EngineDreamCtx` — a **private borrow-newtype** over `&MemoryEngine` carrying the 9 capability delegates of the `DreamCtx` trait that replaced `DreamContext`. It is deliberately **not** `impl DreamCtx for MemoryEngine`: five of the trait's names collide with inherent engine methods, and Rust's inherent-before-trait resolution would turn any future rename into a silent stack overflow (qualification does not help; `unconditional_recursion` does not fire through `#[async_trait]`). The newtype makes that `E0599` instead — see ADR 0014's S5 amendment. Also staying: `promote_with_lineage` (needs the engine-owned `ScopeTree` cache, a loose parameter `MemoryCtx` does not carry), and four thin delegates (`record_insight`, `run_dream_cycle`, `run_dream_cycle_guarded`, `apply_cycle_report`). See `docs/advanced/dream-cycle.md` and ADR 0014 (+ its Wave 2 #816 / S5 amendment) for the design, and ADR 0018 decision #8(d) for the break.

`storage/`
: The persistence **port** — carved into the `me-storage` crate (L1). This facade module **re-exports** the port (both the submodules and the flat trait names) so every existing `crate::storage::graph::FactGraph` and `crate::storage::FactGraph` path keeps resolving, plus the concrete backend impls: `SqliteBackend` (from **me-backend-sqlite**) and `PgBackend` (from **me-backend-postgres**, `backend-postgres` feature, #633). What still lives here physically: the `#[cfg(test)]` cross-backend **`storage/conformance/`** battery (#632) that encodes the trait _contract_ once and runs it against every backend via a `ConformanceBackend` factory (`SqliteBackend` always; `PgBackend` inert/`#[ignore]`d until #635).

`search/`
: Hybrid search pipeline → **me-query** (S4). Combines three retrieval modes:

- `search::fts` -- FTS5 full-text search with BM25 ranking.
- `search::vector` -- brute-force cosine similarity over stored embeddings (`VectorSearchStrategy` dispatch; the `&Connection` → `&dyn SearchIndex` break is deferred to S4).
- `search::hybrid` -- orchestrator that dispatches to FTS, vector, or both, then merges via Reciprocal Rank Fusion (RRF). Defines `SearchQuery`, `SearchResult`, `SearchMode`, and `MatchType`.

`consolidation/`
: Three-pass consolidation pipeline → **me-consolidate**:

1. **Local dedup** -- expire near-duplicate facts (cosine similarity above threshold).
2. **Cluster fusion** -- group related facts and generate cluster-level summaries.
3. **Global integration** -- produce cross-cluster summaries.
   Accepts a `SummaryGenerator` trait object, an `EmbeddingProvider` trait object (to embed the generated summaries), and `ConsolidationConfig`.

`forgetting/`
: Ebbinghaus-based decay with multi-signal importance scoring → **me-forget**. Computes a weighted combination of recency (exponential decay), access frequency, graph connectivity, and base importance. Facts scoring below `ForgetPolicy::min_importance` are soft-deleted (their `t_expired` is set). Returns `PruneStats`.

`engine::conflict`
: Bi-temporal conflict resolution (`MemoryEngine::resolve_conflict`) → **me-resolve**. Given an existing fact and a candidate, delegates to a `ConflictArbiter` for the decision (`Add`, `Update`, `Delete`, `Noop`). On `Update`, the old fact is expired and a `superseded_by` edge is created in the graph. All mutations run in a single transaction.

`engine::reconstruct`
: Background reconstruction orchestration (`MemoryEngine::reconstruct`, #623). Re-embeds stored fact content under a new **same-dimension** identity with no downtime: open (or resume) a `populating` space → backfill it off the write lock (embedding under `spawn_blocking`) → catch-up pass → atomic copy-swap promote. The embedder stays engine-side; the backend does pure DB ops. Returns a `PromoteOutcome`. Different-dim is the #742 follow-up (its read-fence is the `MemoryCtx::ensure_open` gate, now in `me-storage`); the live HNSW rebuild is #624. See `docs/advanced/reconstruction.md`.

`archive/`
: Cold-storage `.pak` snapshot subsystem (feature-gated) → **me-archive**.

`bootstrap/`
: Session log bootstrap pipeline (facade-internal). Parses Claude Code JSONL session logs and imports noteworthy episodes (bug fixes, decisions, conventions, learnings) as historical facts. Sub-modules handle each pipeline stage: `parse` (JSONL deserialization), `filter` (turn reconstruction and keyword pre-filter), `outcome` (heuristic session outcome classification), `extract` (fact extraction via the `SessionExtractor` trait), and `metrics` (configuration, reporting, and prewarm quality metrics). Uses savepoint transactions for crash safety and event-based idempotency to prevent duplicate imports.

`resume/`
: Session bootstrapping via `ResumeConfig` and `ResumeContext` → **facade** (stays home; ADR 0018 decision #5). Implements 4-tier retrieval:

1. **Pinned** -- unforgettable facts (`is_pinned = true`), cross-scope, sorted by `importance_score` descending.
2. **High-importance** -- facts with materialized `importance_score` above a configurable threshold.
3. **Due** -- future-memory facts whose `t_valid` has arrived (`t_valid <= now`).
4. **Recent** -- most recent facts from scope ancestors.
   Tiers are mutually exclusive (a fact appears in at most one tier).

`inspect/`
: Inspection APIs for debugging and observability (facade-internal). Sub-modules handle distinct concerns:

- `inspect::types` -- all inspection-specific types (`FactExplanation`, `FactState`, `EngineStatistics`, `ReplayFilter`, `DumpFormat`, etc.).
- `inspect::explain` -- `explain_fact()` state analysis and `fact_history()` temporal reconstruction.
- `inspect::replay` -- `ReplayFilter` to `EventFilter` conversion for event replay.
- `inspect::dump` -- JSON snapshot serialization and SQLite `VACUUM INTO` backup.
- `inspect::statistics` -- SQL count queries for aggregate statistics.

## The extracted crates (S1 + S2)

The six L0–L2 crates that already exist as separate workspace members under
`memory-engine/lib/`:

`me-types` (L0 — `memory-engine/lib/me-types/`)
: Three module trees, relocated verbatim so the cycle-break and the crate-carve stayed independently verifiable. `types` — the domain DTOs (`Event`, `Fact`, `Edge`, `Summary`, `ScopeNode`, insertion structs, enums, option structs) **plus** the relocated `snapshot`/`cycle-report`/search-result sidecar vocabularies, and (S2) the `EmbeddingSpace`/`SpaceStatus` embedding-space-registry DTOs (moved out of `me-backend-sqlite` so `me-backend-postgres` can share them without a backend-to-backend dependency), and the forget/prune pair `PruneStats` (S1) + `ForgetPolicy` (S5, #985) — pure data-out/config-in with no L3 deps; `ForgetPolicy` was hoisted out of `me-forget` to rejoin `PruneStats` so an L0.5 trait signature can name it (`DreamCtx::forget`, #981). `error` — `MemoryError` (`thiserror`), its typed sub-enums, and the `Result` alias. `limits` — size caps enforced during (de)serialization. No internal (`me-*`) deps — the acyclic leaf.

`me-traits` (L0.5 — `memory-engine/lib/me-traits/`)
: The consumer-implemented capability traits the engine delegates to — `EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`, `PersistenceClassifier`, `Reranker` — plus the trait-adjacent policy types (`ConsolidationConfig`, `CrudDecision`, …) and the `CycleCtx` read-set trait that lets `DreamCycle::run` take `&dyn CycleCtx` without pulling in `me-consolidate`. Depends only on `me-types`. (`ForgetPolicy` is **not** here — it is a pure-data policy DTO and lives in `me-types` alongside its `PruneStats` output; see S5.)

`me-storage` (L1 — `memory-engine/lib/me-storage/`)
: The persistence **port**. Owns the `StorageBackend` umbrella supertrait over six bounded-context traits (`FactGraph`, `EventLog`, `SearchIndex`, `ConsolidationStore`, `SessionStore`, `SchemaManager`) + the feature-gated `ColdStorage`; the closed `FactFilter`/`TemporalFilter` query vocabulary; the `BackendCapabilities`/`LexicalRanker` tier signal; `MemoryCtx` (the universal capability handle — see the [architecture overview](../design/architecture-overview.md)); and `UpcasterRegistry` (event-payload versioning the port applies on read). No SQL string or driver type appears here — backends implement the traits and map driver errors to the driver-opaque `StorageError` at the seam. Depends on `{me-types, me-traits}`.

`me-index` (L2 — `memory-engine/lib/me-index/`)
: Backend-agnostic in-memory projections: `MemoryGraph` (the `petgraph`-backed knowledge graph, ex-`graph/`) and `ScopeTree` (the hierarchical scope cache, ex-`scope/`). Trait-free and mockable — depends only on `me-types`, not `me-traits` or `me-storage` (ADR 0018 decision #4) — so the #763 freshness registry and the #247/#243 context work get a backend-free home. Rebuilt from `me-types` DTOs by the facade's `engine::graph_load` glue on engine open.

`me-backend-sqlite` (L2 — `memory-engine/lib/me-backend-sqlite/`)
: The `SqliteBackend` `StorageBackend` impl. Owns the `ConnectionPool` (N readers + 1 writer, ex-`pool/`), the `store/` table accessors + schema migrations (ex-`store/`), the `block_read`/`block_write`/`for_each_streamed` `spawn_blocking` seam, the FTS5/vector search cores (`search::fts`/`search::vector`; the RRF merge + the `MemoryQuery` API stay in the facade until `me-query`, S4), and the sidecar `.snapshot` file I/O. Depends on `{me-types, me-traits, me-storage}`.

`me-backend-postgres` (L2 — `memory-engine/lib/me-backend-postgres/`)
: The `PgBackend` `StorageBackend` impl (#633 skeleton): a `deadpool-postgres` pool + a fresh v14-logical migration chain (native FK constraints, `GENERATED ALWAYS AS IDENTITY` ids, a `tsvector` generated column + GIN index, `vector(N)` pgvector columns) + the `SchemaManager` lifecycle/identity/config core. Inspection and the #623 background-reconstruction methods are `MemoryError::NotImplemented` stubs; data CRUD is #634, lexical+vector search and the conformance-arm flip are #635. Depends only on `{me-types, me-storage}` — **not** `me-backend-sqlite` (no backend-to-backend edge). Optional: pulled by the facade's `backend-postgres` feature so a `backend-sqlite`-only build never compiles `tokio-postgres`/`deadpool-postgres`.

## Re-exports from the facade `lib.rs`

The `memory-engine` facade re-exports the extracted crates so the four-layer public seam is
unchanged and consumers keep `use memory_engine::*`:

- `pub use me_types::types` — all public domain types.
- `pub use me_types::error` — `MemoryError`, `Result`.
- `pub use me_traits as traits` — the consumer traits module (kept as a **module** so
  `memory-engine-embed` reaches them via `memory_engine::traits::*`; ADR 0018 decision #6).
- `pub use me_storage::…` (via `crate::storage`) — the port trait family + `MemoryCtx`.
- `MemoryEngine`, `EngineConfig` (from `engine`).
- `SessionExtractor`, `KeywordExtractor`, `BootstrapConfig`, `BootstrapReport` (from `bootstrap`).
- `serialize_embedding`, `deserialize_embedding` (from `store`).
- `inspect_types` (from `inspect::types`).
