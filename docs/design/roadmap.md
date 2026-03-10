# Roadmap

## Research Foundation

9 papers, 22 cross-paper relationships, community synthesis (OpenClaw, Reddit), and a 3-round multi-AI adversarial debate informed the design.

| Paper                          | Key Contribution to Design                                                                                                                                         |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **CoALA** (2309.02427)         | 4 memory types mapped to `fact_type` enum (Episodic, Semantic, Procedural). Working memory stays in the consumer's context window.                                 |
| **Graphiti** (2410.13790)      | Bi-temporal model: 4 timestamps per fact (`t_created`/`t_expired` system, `t_valid`/`t_invalid` real-world). Conflict detection via LLM.                           |
| **Mem0** (2504.19413)          | CRUD conflict resolution pattern (Add/Update/Delete/Noop). Graph-based memory with entity-centric + semantic triplet retrieval.                                    |
| **A-Mem** (2502.12110)         | Self-organizing memory without predefined schemas. Zettelkasten-inspired linking. Informed the "one store, multiple projections" decision.                         |
| **Memory Survey** (2512.13564) | Three-pass consolidation taxonomy: local dedup, cluster fusion, global integration. Forgetting strategies: time expiration, access frequency, informational value. |
| **AgeMem** (2601.01885)        | Agentic memory framework with LTM+STM as tool-based operations. Validated trait-based design.                                                                      |
| **SparseMemFT**                | Sparse memory fine-tuning. Informed the boundary: engine is retrieval-only, not fine-tuning.                                                                       |
| **Memento** (2508.16153)       | Hierarchical memory with summarization. Informed consolidation levels (local, cluster, global).                                                                    |
| **Doc-to-LoRA**                | Document-to-adapter pipeline. Confirmed the engine boundary: store and retrieve, consumer decides what to do with results.                                         |

## Storage Technology Decisions

The papers use diverse backends (Neo4j, Qdrant, custom stores), but all are external services. The constraint for this project: **embedded, single-process, no JVM or external database**.

| Technology            | Verdict   | Rationale                                                                                                                                                    |
| --------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Neo4j**             | Discarded | External JVM process. Mature but overkill for embedded single-agent use.                                                                                     |
| **Qdrant**            | Discarded | External service. Violates embedded constraint.                                                                                                              |
| **SurrealDB 3.0**     | Deferred  | Rust-native, vector+graph+KV+temporal in single binary. Too young when evaluated (Feb 2026). Event-sourced log enables replay-based migration if it matures. |
| **LanceDB**           | Deferred  | Embedded Rust-native columnar vector DB. Candidate for ANN when benchmarks show need.                                                                        |
| **SQLite + Petgraph** | Adopted   | Battle-tested, embedded, simple. FTS5 for keyword search. WAL for concurrent reads. Petgraph for in-memory graph loaded from SQLite.                         |

**Migration safety net:** The event-sourced architecture guarantees replay into any storage backend. No lock-in.

## Phase Summary

### Phase 1: Ingest, Query Loop -- DONE

**PR:** [#2](https://github.com/dutiona/memory-engine/pull/2) (squash-merged)

Core types, error handling, schema + migrations, event log, fact CRUD with bi-temporal timestamps and blake3 hashing, FTS5 search (BM25), vector search (cosine, partial sort), hybrid search (RRF), and the `MemoryEngine` facade with Phase 2 stubs.

**Deliverable:** Working `MemoryEngine` with `ingest()`, `add_fact()`, `query()`. 59 tests.

### Phase 2: Graph, Consolidation, Forgetting, Conflict Resolution -- DONE

**PR:** [#8](https://github.com/dutiona/memory-engine/pull/8) (squash-merged)

Graph module (petgraph wrapper + `EdgeStore`), summary store, Ebbinghaus decay with multi-signal importance scoring, three-pass consolidation (dedup, cluster fusion, global integration), bi-temporal conflict resolution with `ConflictArbiter` trait.

**Deliverable:** All 5 primitives operational: `ingest()`, `add_fact()`, `query()`, `consolidate()`, `forget()`, `resolve_conflict()`. 107 tests.

Key implementation learnings:

- Edge cascade invariant: every fact expiry must also expire its edges in both SQLite and the in-memory graph.
- Atomic consolidation: all three passes in a single `unchecked_transaction`.
- Graph rebuild after consolidation to keep degree-based scoring consistent.

### Phase 3: Hardening & Scoping -- DONE

| Feature                                                            | Status |
| ------------------------------------------------------------------ | ------ |
| Hierarchical scoping (`ScopeTree`, `ScopeStore`)                   | Done   |
| `scope_id` on facts, edges, events, summaries + migration          | Done   |
| SQL-level filters (`t_expired`, `fact_type`, `scope_id`)           | Done   |
| `ConnectionPool` (N readers + 1 writer, `parking_lot`)             | Done   |
| `MemoryEngine` Send+Sync with `RwLock`                             | Done   |
| `AsyncMemoryEngine` (`tokio::task::spawn_blocking`)                | Done   |
| `resume_context()` -- 3-tier retrieval, scope-aware                | Done   |
| Criterion benchmarks (vector, FTS, hybrid, scoped)                 | Done   |
| `AddFactOptions` builder (per-fact importance, metadata, temporal) | Done   |
| Schema migration framework (versioned, forward-only)               | Done   |

### Phase 3b: Temporal Memory & Agent Lifecycle -- PLANNED

| Feature                 | Description                                                            |
| ----------------------- | ---------------------------------------------------------------------- |
| Unforgettable flag      | `is_pinned: bool` on facts, `PersistenceClassifier` trait hook         |
| Future memory           | `t_valid` filter -- facts surface when their date arrives              |
| Scheduling API          | `resume_context(now)` rework, `drain_due(now)`, `next_due_time()`      |
| Materialized importance | Persist computed importance score on facts for fast retrieval          |
| Event envelope v2       | `origin_node_id`, `sequence_id`, advisory `created_at` for future sync |

### Phase 4: Operability & MCP Server -- PLANNED

| Feature                     | Description                                                             |
| --------------------------- | ----------------------------------------------------------------------- |
| Inspection APIs             | `explain_fact()`, `replay_events()`, `dump_state()`, `statistics()`     |
| CLI inspector               | `memory-engine-cli` -- operator tool for inspecting and managing memory |
| MCP server                  | `memory-engine-mcp` workspace member, eventual separate repo            |
| Import/export               | JSON event log + SQLite backup, gzip/zstd compression                   |
| Archival compression        | Cold storage `.pak` files for old non-pinned facts                      |
| Semantic extraction queries | Scope + temporal range + semantic search composition                    |

### Phase 5: Knowledge Integration -- PLANNED

| Feature                        | Description                                     |
| ------------------------------ | ----------------------------------------------- |
| `KnowledgeBaseConnector` trait | Transport-agnostic, consumer-implemented        |
| `KnowledgeRef` on facts        | URI field pointing to knowledge base content    |
| Graceful degradation           | "Memory lapse" when KB unreachable, retry later |
| research-index bridge          | `memory-kb-research-index` middleware crate     |

## Deferred Items

These are not planned for any phase. Each has a trigger condition; when the trigger fires, the item gets scheduled.

| Item                        | Trigger                                                |
| --------------------------- | ------------------------------------------------------ |
| ANN vector index            | Benchmarks show >50ms at scale                         |
| Web UI                      | Separate project (WASM+Rust graph visualization)       |
| Auth                        | Deployment layer concern, not engine core              |
| SaaS Sync                   | Separate product (CRDT event-log merge + E2EE)         |
| Evaluation harness          | Regression corpus for retrieval/consolidation quality  |
| Hierarchical summarization  | Multi-level abstractions (Memento-style)               |
| Schema evolution discipline | Versioning policy, backwards-compat testing            |
| Determinism guarantees      | Replay, merge, idempotency rules for sync              |
| Multimodal memory           | Image/audio embeddings (schema supports it via BLOB)   |
| Parameter updates           | Doc-to-LoRA. Out of scope -- engine is retrieval-only. |

## Architecture Diagram

```
Consumer (AI agent, CLI tool, MCP server)
    |
    v
+--------------------------------------+
|           MemoryEngine               |
|  ingest . add_fact . query           |  <-- Phase 1
|  consolidate . forget . resolve      |  <-- Phase 2
|  resume_context . scoped queries     |  <-- Phase 3
|  drain_due . next_due_time           |  <-- Phase 3b
+--------------------------------------+
|  Search                              |
|  +- FTS5 (BM25)                      |
|  +- Vector (cosine, brute-force)     |
|  +- Hybrid (RRF k=60)               |
+--------------------------------------+
|  Scoping                             |  <-- Phase 3
|  +- ScopeTree (hierarchical)         |
|  +- ScopeStore (SQLite-backed)       |
+--------------------------------------+
|  Store                               |
|  +- EventStore (append-only log)     |
|  +- FactStore (bi-temporal + BLOB)   |
|  +- EdgeStore (graph persistence)    |  <-- Phase 2
|  +- SummaryStore                     |  <-- Phase 2
+--------------------------------------+
|  Graph (petgraph DiGraph)            |  <-- Phase 2
+--------------------------------------+
|  Forgetting (Ebbinghaus decay)       |  <-- Phase 2
+--------------------------------------+
|  Consolidation (3-pass)              |  <-- Phase 2
+--------------------------------------+
|  Conflict (bi-temporal arbiter)      |  <-- Phase 2
+--------------------------------------+
|  ConnectionPool (N readers + 1 wr)   |  <-- Phase 3
+--------------------------------------+
|  AsyncMemoryEngine (spawn_blocking)  |  <-- Phase 3
+--------------------------------------+
    |
    v
  SQLite WAL (rusqlite bundled-full)
  +- events, facts, edges, summaries, scopes, config
  +- facts_fts (FTS5 virtual table)
  +- indexes + schema migration framework
```
