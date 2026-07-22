# Architecture Overview

## High-Level Architecture

The memory-engine is an embedded Rust library that provides durable long-term memory for AI agents. It runs in-process (no external database servers) and exposes a single facade type, `MemoryEngine`, through which all operations flow.

```{mermaid}
graph TB
    subgraph Consumers
        agent["AI Agent"]
        cli["CLI Tool"]
        mcp["MCP Server"]
    end

    subgraph MemoryEngine Facade
        ingest["ingest()"]
        add_fact["add_fact()"]
        query["query()"]
        consolidate["consolidate()"]
        forget["forget()"]
        resolve["resolve_conflict()"]
        resume["resume_context()"]
    end

    subgraph Search Layer
        fts["FTS5 (BM25)"]
        vec["Vector (brute-force / HNSW)"]
        rrf["Hybrid (RRF k=60)"]
        strategy["VectorSearchStrategy dispatch"]
    end

    subgraph Storage Layer
        events["EventStore (append-only)"]
        facts["FactStore (bi-temporal)"]
        edges["EdgeStore"]
        summaries["SummaryStore"]
        scopes["ScopeStore"]
        sqlite["SQLite WAL"]
    end

    subgraph In-Memory Structures
        graph["MemoryGraph (petgraph DiGraph)"]
        scope_tree["ScopeTree (hierarchical)"]
    end

    subgraph Consumer Traits
        embed["EmbeddingProvider"]
        summary["SummaryGenerator"]
        arbiter["ConflictArbiter"]
    end

    agent --> ingest
    cli --> query
    mcp --> add_fact

    ingest --> events
    add_fact --> facts
    add_fact --> embed
    query --> rrf
    rrf --> fts
    rrf --> strategy
    strategy --> vec
    consolidate --> summary
    consolidate --> facts
    forget --> graph
    forget --> facts
    resolve --> arbiter
    resume --> facts

    fts --> sqlite
    vec --> sqlite
    events --> sqlite
    facts --> sqlite
    edges --> sqlite
    summaries --> sqlite
    scopes --> sqlite

    edges --> graph
    scopes --> scope_tree
```

## Module Relationships

The crate is organized into modules with clear responsibilities:

| Module             | Responsibility                                                                                                                                                                                                                                                                               |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `engine`           | `MemoryEngine` facade. Orchestrates all operations, holds the `ConnectionPool`, `RwLock<MemoryGraph>`, and `RwLock<ScopeTree>`.                                                                                                                                                              |
| `store/`           | Persistence layer. `EventStore`, `FactStore`, `EdgeStore`, `SummaryStore`, `ScopeStore` each own their SQL. Schema migrations live here.                                                                                                                                                     |
| `search/`          | Query layer. FTS5 (BM25), vector (cosine similarity, brute-force or HNSW via `ann` feature), strategy dispatch, and hybrid (RRF) search.                                                                                                                                                     |
| `graph/`           | `MemoryGraph` wrapper around `petgraph::DiGraph`. Loaded from SQLite on startup, kept in sync on mutations.                                                                                                                                                                                  |
| `scope/`           | `ScopeTree` for hierarchical isolation. In-memory tree structure, backed by `ScopeStore` in SQLite.                                                                                                                                                                                          |
| `resume/`          | Session bootstrapping. `MemoryEngine::resume_context()` (an `async` engine method) implements 4-tier retrieval (pinned → high_importance → due → recent).                                                                                                                                    |
| `consolidation/`   | Three-pass memory compression: local dedup, cluster fusion, global integration.                                                                                                                                                                                                              |
| `forgetting/`      | Ebbinghaus decay with multi-signal importance scoring.                                                                                                                                                                                                                                       |
| `engine::conflict` | Bi-temporal conflict resolution (`MemoryEngine::resolve_conflict`) delegated to the consumer `ConflictArbiter`.                                                                                                                                                                              |
| `pool/`            | `ConnectionPool`: N reader connections + 1 writer connection. Supports read-only mode for operator tools.                                                                                                                                                                                    |
| `traits`           | Consumer-provided trait definitions. Zero LLM/network dependencies in core.                                                                                                                                                                                                                  |
| `types`            | All data types: `Event`, `Fact`, `Edge`, `Summary`, `ScopeNode`, `ScopeQuery`, enums.                                                                                                                                                                                                        |
| `error`            | `MemoryError` enum with `thiserror` derivations.                                                                                                                                                                                                                                             |
| `storage/`         | `StorageBackend` trait family (plus `FactFilter`, capability flags, `StorageError`) and `SqliteBackend`. The async port the engine `.await`s; `SqliteBackend` wraps the `ConnectionPool`, reuses the `store/` SQL verbatim, and offloads blocking SQLite onto `tokio::task::spawn_blocking`. |

## Threading Model

`MemoryEngine` is `Send + Sync`. Consumers share it via `Arc<MemoryEngine>`. The engine is **async-native**: its DB-touching methods are `async fn` that `.await` an `Arc<dyn StorageBackend>` port, so a single tokio runtime fans many concurrent operations out across few threads (the design chosen over a thread-per-query wrapper). The `async` feature is default-on and provides that runtime.

Concurrency is provided by four mechanisms:

1. **`Arc<dyn StorageBackend>`** -- All persistence flows through the storage port. The default `SqliteBackend` owns the `ConnectionPool` (N SQLite reader connections, default 4, behind a semaphore + 1 exclusive writer connection behind a `parking_lot::Mutex`; readers use SQLite WAL mode for concurrent access without blocking writes) and offloads every blocking SQLite call onto the tokio blocking pool via `spawn_blocking`, so synchronous DB work never stalls the async executor. In read-only mode (`EngineConfig::read_only`), the writer slot holds a read-only connection and `try_write()` returns `MemoryError::ReadOnly`.

2. **RwLock\<MemoryGraph\>** -- The in-memory petgraph is behind a `parking_lot::RwLock`. Read operations (`graph_degree`, `graph_neighbors`, etc.) take a shared read lock; mutations (conflict resolution, forgetting, consolidation) take an exclusive write lock. The lock is **never held across an `.await`** -- the guard is scoped tightly around the synchronous in-memory access.

3. **RwLock\<ScopeTree\>** -- The hierarchical scope tree is behind a separate `RwLock`, with the same never-across-`.await` discipline. Scope resolution during queries takes a read lock; scope creation during `add_fact` takes a write lock.

4. **Offloaded consumer trait calls** -- Calls into consumer traits (`EmbeddingProvider`, `Reranker`, `SummaryGenerator`) may issue blocking HTTP, so the engine runs them on `spawn_blocking` rather than inline on the executor thread. This keeps slow network-based embedding/rerank/summary work off the async runtime and out of any held lock.

All public methods take `&self` (the engine mutates only `close`). A clean shutdown is `MemoryEngine::close(&mut self).await`, which flushes the sidecar HNSW/snapshot. `Drop` is now **warn-only**: an engine dropped without `close()` is still durable (the DB is the source of truth) but rebuilds its sidecar from the DB on the next open instead of loading the flushed snapshot.

```
                  Arc<MemoryEngine>  (one tokio runtime)
                           |
        ┌──────────────────┼──────────────────┐
        |                  |                  |
   Task A (read)     Task B (read)     Task C (write)
        |                  |                  |
        └─────── .await Arc<dyn StorageBackend> ───────┘
                           |
                   SqliteBackend
              (ConnectionPool + spawn_blocking)
        ┌──────────────────┼──────────────────┐
   pool.read()        pool.read()       pool.write()
   (any of N          (any of N         (Mutex<Connection>)
    readers)           readers)
```

This design replaced an earlier single-writer `!Send` design (Phase 1-2) where the engine owned a single `Connection` and could not be shared across threads, and the thread-pooled `ConnectionPool` of Phase 3. The #631 cutover made the engine fully async-native behind the `StorageBackend` port: the same trait will host a Postgres backend (#633/#634) with no engine change.

## Crate decomposition (Wave 2 #816)

The module boxes in the diagrams above describe the _logical_ layering; Wave 2 (#816) makes that layering a **physical, acyclic DAG of per-concern crates**, so the library boundary, the link graph, and the public/private surface are visible rather than "Cargo magic". The former single core crate is being carved into thirteen crates (plus a dev-only `me-test-support`); **S1 has landed** — `me-types`, `me-traits`, and `me-storage` are real crates — and slices S2–S6 carve the backends and primitives out of the `memory-engine` facade. The locked structural decisions are in [ADR 0018](adr/0018-wave2-crate-decomposition-memoryctx.md); the full crate map (with per-crate DONE/PENDING status) is in [the crate layout reference](../reference/crate-layout.md).

```{mermaid}
graph TD
    facade["memory-engine (L4 facade)"]
    subgraph L3["L3 — primitives"]
        prims["me-ingest · me-query · me-consolidate<br/>me-forget · me-resolve · me-archive"]
    end
    subgraph L2["L2 — backends + projections"]
        backends["me-backend-sqlite · me-backend-postgres · me-index"]
    end
    storage["me-storage (L1 — storage port)"]
    traits["me-traits (L0.5 — contracts)"]
    types["me-types (L0 — data + error)"]

    facade --> prims
    prims --> backends
    backends --> storage
    storage --> traits
    traits --> types
    storage --> types
```

**The acyclicity invariant.** Edges point strictly **down** by layer — a crate may depend only on crates in a lower layer. This is the primary invariant of the decomposition and it is **compiler-enforced**: `cargo` rejects any re-introduced cycle at resolve time, and a CI `cargo tree` check catches a back-edge before merge. `me-index` (the graph/scope projections) depends on `{me-storage, me-types}` only — not `me-traits` — so the projections stay backend-free and mockable. Two deliberate public-API breaks are gated by `cargo public-api` (ADR 0018 decision #8): `DreamCycle::run(&dyn CycleCtx)` (landed in S1) and `VectorSearchStrategy::search(&dyn SearchIndex)` (deferred to S4).

**Status today.** `me-types` (L0), `me-traits` (L0.5), and `me-storage` (L1) exist as separate workspace members under `crates/`. The `memory-engine` facade re-exports them (the four-layer `types` / `error` / `traits` / `storage` seam is unchanged) and still physically owns the backends and primitives as modules until they carve out in S2–S5.

## `MemoryCtx`

`MemoryCtx<'a>` is the **universal capability handle** every primitive receives. It carries _only_ what every primitive needs, so a primitive that needs nothing more can be called with just this handle:

| Field             | Type                          | Purpose                                                                        |
| ----------------- | ----------------------------- | ------------------------------------------------------------------------------ |
| `storage`         | `&'a Arc<dyn StorageBackend>` | The single persistence port. All DB-touching work `.await`s this.              |
| `embed_dim`       | `usize`                       | The embedding dimension the handle was opened at.                              |
| `read_only`       | `bool`                        | Whether the engine was opened read-only (write primitives check this).         |
| `reopen_required` | `&'a AtomicUsize`             | The reconstruction dimension fence (#742): `0` = open; non-zero `D′` = fenced. |

Two gates are relocated verbatim from `engine::mod` onto the handle:

- **`ensure_open()`** — the read-fence gate every embedding-touching primitive calls at entry; returns `MemoryError::EmbeddingReopenRequired { new_dim }` once a different-dim reconstruction has fenced the handle, until the consumer reopens at `D′`.
- **`ensure_writable()`** — the write gate; returns `MemoryError::ReadOnly` if the engine was opened read-only.

`MemoryCtx` is `Copy` (two references + a `usize` + a `bool`), so a primitive hands it to a helper without ceremony. The lifetime `'a` ties it to the facade-owned state for the duration of one call — it is intentionally **not** `'static`/`Serialize`.

**Universal borrow-bundle + loose extras.** The handle deliberately does _not_ carry every capability a primitive might use. Per-primitive extras — `graph`, `scope_tree`, `reranker`, `cold`, `db_path` — are passed as **explicit loose parameters**, so each free-fn signature _declares_ exactly which extra capabilities it uses (ADR 0018 decision #3). This keeps the universal handle small and makes each primitive's capability set greppable from its signature.

`MemoryCtx` is homed in `me-storage` (L1), not the facade and not a separate `me-ctx` crate, because its load-bearing field is the storage port — it is _about_ storage access. It is **defined in S1**; the L3 primitive crates (me-ingest / me-query / me-consolidate / me-forget / me-resolve / me-archive) **consume it in S3/S4**.

## Data Flow

### Event to Fact to Query

The core data flow follows an event-sourced pattern:

1. **Ingest** -- Raw events (interactions, tool calls, system events) are appended to an immutable event log. This log is the source of truth.

2. **Add Fact** -- The consumer explicitly derives facts from events. Facts are not auto-projected; the consumer decides what to remember. Each fact gets a blake3 content hash, an embedding vector (via `EmbeddingProvider`), bi-temporal timestamps, and a scope assignment.

3. **Query** -- Hybrid retrieval combines FTS5 (keyword, BM25-ranked), vector (cosine similarity via `VectorSearchStrategy` dispatch — brute-force or HNSW), or both (Reciprocal Rank Fusion with k=60). Results are filtered by temporal validity, fact type, and scope.

4. **Consolidate** -- Three-pass memory compression: (1) local dedup merges near-duplicates by embedding cosine similarity, (2) cluster fusion groups related facts into thematic summaries, (3) global integration produces high-level summaries. All three passes run in a single SQLite transaction.

5. **Forget** -- Ebbinghaus decay with multi-signal importance scoring. Each fact's importance is computed as a weighted sum of recency (exponential decay), access frequency, graph connectivity, and base importance. Facts below the threshold are soft-deleted (`t_expired` set).

6. **Resolve Conflict** -- When the consumer detects contradicting facts, it calls `resolve_conflict` with a `ConflictArbiter`. The arbiter returns a CRUD decision (Add, Update, Delete, Noop). Mutations are atomic; the graph is updated only after the transaction commits.

### Resume Context

Session bootstrapping uses 4-tier retrieval to populate the agent's context window:

| Tier                | Source          | Selection Criteria                                                           |
| ------------------- | --------------- | ---------------------------------------------------------------------------- |
| **Pinned**          | Cross-scope     | Unforgettable facts (`is_pinned = true`), sorted by `importance_score` desc. |
| **High-importance** | Scope ancestors | Facts with materialized `importance_score` above a configurable threshold.   |
| **Due**             | Scope ancestors | Future-memory facts whose `t_valid` has arrived (`t_valid <= now`).          |
| **Recent**          | Scope ancestors | Most recently created facts. Current working context.                        |

The four tiers are mutually exclusive (a fact appears in at most one tier). The consumer controls tier sizes and thresholds via `ResumeConfig`.

## Prospective Memory

The Memory layer supports two complementary sub-modes:

- **Retrospective memory** — what happened. Bi-temporal facts with Ebbinghaus decay, the consolidation pipeline, and the conflict arbiter. This is the path most of the engine surface area exercises.
- **Prospective memory** — what should happen _when_. Time-anchored intentions whose "due" status is a function of `t_valid` versus the current clock plus the active scope filter.

This split is shipped, not aspirational. The cognitive-science basis is the prospective-memory literature (McDaniel & Einstein 2007, _Prospective Memory: An Overview and Synthesis of an Emerging Field_) and the framing is consistent with DeepMind's cognitive taxonomy §7.5.4 "Prospective memory" sub-faculty (Burnell et al. 2026).

### API surface

| Method                               | Source                                                          | Behavior                                                                                                                                                              |
| ------------------------------------ | --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `MemoryEngine::list_due(now, scope)` | [`src/engine/scheduling.rs:17`](../../src/engine/scheduling.rs) | Returns active facts where `t_valid IS NOT NULL ∧ t_valid <= now` and `(t_invalid IS NULL ∨ t_invalid > now)`, scoped via `ScopeQuery::Subtree` semantics on `scope`. |
| `MemoryEngine::next_due_time(scope)` | [`src/engine/scheduling.rs:39`](../../src/engine/scheduling.rs) | Returns the earliest `t_valid` strictly in the future as a polling-interval hint. `None` if no future-dated facts in scope.                                           |
| `Fact::surfaced_at`                  | [`src/types.rs:141`](../../src/types.rs)                        | First-fire timestamp. `None` until the fact is returned by `list_due`; subsequent calls observe the persisted value. The DB-authoritative value always wins on read.  |

The store-level primitives live in [`src/store/facts.rs:397`](../../src/store/facts.rs) (`list_due`), [`src/store/facts.rs:432`](../../src/store/facts.rs) (`next_due_time`), and [`src/store/facts.rs:470`](../../src/store/facts.rs) (`stamp_surfaced`). Two unit tests guard the round-trip behavior: `list_due_surfaces_facts_with_past_t_valid` and `next_due_time_returns_earliest_future_t_valid`.

### Firing models

**Time-based firing (clock-triggered).** A fact is created with `t_valid` set to its scheduled fire time. The fact is invisible to retrospective queries until that moment because hybrid search filters on temporal validity. Once `now >= t_valid`, the fact becomes returnable from both `list_due` and from `resume_context()`'s "Due" tier (the table in [§ Resume Context](#resume-context)).

**Scope-based firing (context-bound).** The `scope` argument to `list_due` filters by the active scope subtree, so a fact scheduled for `user:michael/project:demo` will not surface to a consumer querying `user:michael/project:research`. The combination of clock + scope acts as a context cue in the McDaniel & Einstein sense: the right intention surfaces only when the right context is active.

**Lifecycle: scheduled → fired → re-read.** `surfaced_at` is `None` while the fact is scheduled but has not yet fired. The first `list_due` call that returns the fact stamps `surfaced_at` to the call's `now` argument, transitioning the fact to _fired_. Subsequent `list_due` calls return the same fact with the persisted `surfaced_at` (re-read state). This three-state lifecycle is what distinguishes prospective memory from a plain temporal filter — the consumer can tell a brand-new firing apart from a re-read without keeping its own bookkeeping.

### Polling model and the LLM-free invariant

The engine deliberately does not run its own event loop. `list_due` and `next_due_time` are pull primitives: the consumer schedules its own poll at `next_due_time` (or sooner if it has independent work) and the engine stays a passive store. This is the same trait-based discipline ADR-0004 establishes for embeddings and summaries — the engine exposes the primitives, the consumer owns the runtime. A push model would either (a) require an internal scheduler thread, or (b) require an LLM to interpret which intentions are now "active." Both would breach the LLM-free invariant. The polling model preserves it cleanly.

The only known gap in the prospective-memory surface is a deterministic predicate language for **event-based firing** ("fire when a new fact matching predicate P is ingested") as opposed to clock-based firing. That gap is tracked separately (ME-P1-E, ADR-0013) and is addressable via Allen Interval Algebra (ADR-0011) plus `scope_id` / `entity_id` matching, with semantic predicates remaining a consumer concern by design.

## Scope Tree

Scopes provide hierarchical isolation without separate databases. Each fact, edge, event, and summary carries a `scope_id`.

Scope paths are consumer-facing strings like `"user:michael/project:demo"`. The engine resolves them to internal integer IDs via the `ScopeTree`.

```
root (id=1)
├── user:michael (id=2)
│   ├── project:demo (id=3)
│   └── project:research (id=4)
└── user:other (id=5)
```

The `ScopeQuery` enum controls scope resolution for searches:

- **Exact** -- Facts at exactly this scope.
- **Subtree** -- Facts at this scope and all descendants.
- **Ancestors** -- Facts at this scope and all ancestors up to root.
- **Inherited** -- Ancestors + subtree (full inherited context).

## Consumer Traits

The engine has zero LLM or network dependencies. All intelligence is injected by the consumer:

| Trait                    | Method                                | Phase                  |
| ------------------------ | ------------------------------------- | ---------------------- |
| `EmbeddingProvider`      | `embed(text) -> Vec<f32>`             | Phase 1 (implemented)  |
| `SummaryGenerator`       | `summarize(facts) -> String`          | Phase 2 (implemented)  |
| `SummaryGenerator`       | `embed(text) -> Vec<f32>`             | Phase 2 (implemented)  |
| `ConflictArbiter`        | `arbitrate(old, new) -> CrudDecision` | Phase 2 (implemented)  |
| `PersistenceClassifier`  | `should_pin(fact) -> bool`            | Phase 3b (implemented) |
| `KnowledgeBaseConnector` | `resolve(uri) -> KnowledgeChunk`      | Phase 5 (planned)      |

This trait-based design means the engine can be tested with mock implementations (as the test suite demonstrates) and consumers can swap between local models, API-based models, or rule-based logic without touching the engine.
