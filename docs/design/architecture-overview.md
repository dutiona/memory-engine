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

| Module           | Responsibility                                                                                                                           |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `engine`         | `MemoryEngine` facade. Orchestrates all operations, holds the `ConnectionPool`, `RwLock<MemoryGraph>`, and `RwLock<ScopeTree>`.          |
| `store/`         | Persistence layer. `EventStore`, `FactStore`, `EdgeStore`, `SummaryStore`, `ScopeStore` each own their SQL. Schema migrations live here. |
| `search/`        | Query layer. FTS5 (BM25), vector (cosine similarity, brute-force or HNSW via `ann` feature), strategy dispatch, and hybrid (RRF) search. |
| `graph/`         | `MemoryGraph` wrapper around `petgraph::DiGraph`. Loaded from SQLite on startup, kept in sync on mutations.                              |
| `scope/`         | `ScopeTree` for hierarchical isolation. In-memory tree structure, backed by `ScopeStore` in SQLite.                                      |
| `resume/`        | Session bootstrapping. `resume_context()` implements 5-tier retrieval (pinned → high_importance → due → recent → kb_stubs).              |
| `consolidation/` | Three-pass memory compression: local dedup, cluster fusion, global integration.                                                          |
| `forgetting/`    | Ebbinghaus decay with multi-signal importance scoring.                                                                                   |
| `conflict/`      | Bi-temporal conflict resolution delegated to `ConflictArbiter`.                                                                          |
| `pool/`          | `ConnectionPool`: N reader connections + 1 writer connection.                                                                            |
| `traits`         | Consumer-provided trait definitions. Zero LLM/network dependencies in core.                                                              |
| `types`          | All data types: `Event`, `Fact`, `Edge`, `Summary`, `ScopeNode`, `ScopeQuery`, enums.                                                    |
| `error`          | `MemoryError` enum with `thiserror` derivations.                                                                                         |
| `async_engine`   | `AsyncMemoryEngine` (behind `async` feature flag). Wraps `MemoryEngine` with `tokio::task::spawn_blocking`.                              |

## Threading Model

`MemoryEngine` is `Send + Sync`. Consumers share it via `Arc<MemoryEngine>`.

Thread safety is provided by three mechanisms:

1. **ConnectionPool** -- Bounded pool of N SQLite reader connections (default 4) protected by a semaphore, plus 1 exclusive writer connection behind a `parking_lot::Mutex`. Readers use SQLite WAL mode for concurrent access without blocking writes.

2. **RwLock\<MemoryGraph\>** -- The in-memory petgraph is behind a `parking_lot::RwLock`. Read operations (`graph_degree`, `graph_neighbors`, etc.) take a shared read lock. Mutations (conflict resolution, forgetting, consolidation) take an exclusive write lock.

3. **RwLock\<ScopeTree\>** -- The hierarchical scope tree is behind a separate `RwLock`. Scope resolution during queries takes a read lock. Scope creation during `add_fact` takes a write lock.

All public methods take `&self`. The embedding computation in `add_fact` happens _before_ acquiring the write lock, so slow network-based embedding calls do not block readers.

```
                    Arc<MemoryEngine>
                           |
        ┌──────────────────┼──────────────────┐
        |                  |                  |
   Thread A (read)   Thread B (read)   Thread C (write)
        |                  |                  |
   pool.read()        pool.read()       pool.write()
   (any of N          (any of N         (Mutex<Connection>)
    readers)           readers)
        |                  |                  |
   graph.read()       scope_tree.read()  graph.write()
   (shared RwLock)    (shared RwLock)    (exclusive RwLock)
```

This design replaced an earlier single-writer `!Send` design (Phase 1-2) where the engine owned a single `Connection` and could not be shared across threads. The Phase 3 rework introduced `ConnectionPool` to make the engine usable from async runtimes and multithreaded consumers.

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

Session bootstrapping uses 5-tier retrieval to populate the agent's context window:

| Tier                | Source          | Selection Criteria                                                           |
| ------------------- | --------------- | ---------------------------------------------------------------------------- |
| **Pinned**          | Cross-scope     | Unforgettable facts (`is_pinned = true`), sorted by `importance_score` desc. |
| **High-importance** | Scope ancestors | Facts with materialized `importance_score` above a configurable threshold.   |
| **Due**             | Scope ancestors | Future-memory facts whose `t_valid` has arrived (`t_valid <= now`).          |
| **Recent**          | Scope ancestors | Most recently created facts. Current working context.                        |
| **KB stubs**        | —               | Placeholder for Phase 5 knowledge-base references.                           |

The five tiers are mutually exclusive (a fact appears in at most one tier). The consumer controls tier sizes and thresholds via `ResumeConfig`.

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
