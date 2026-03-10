# memory-engine Roadmap

## Research Foundation

9 papers, 22 cross-paper relationships, community synthesis (OpenClaw, Reddit), 3-round multi-AI debate.

| Paper                          | Key Contribution to Design                                                                                                                                   |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **CoALA** (2309.02427)         | 4 memory types → `fact_type` tag (Episodic, Semantic, Procedural). Working memory stays in consumer's context window.                                        |
| **Graphiti** (2410.13790)      | Bi-temporal model: 4 timestamps per fact (`t_created`/`t_expired` system, `t_valid`/`t_invalid` real-world). Conflict detection via LLM.                     |
| **Mem0** (2504.19413)          | CRUD conflict resolution pattern (Add/Update/Delete/Noop). Graph-based memory with entity-centric + semantic triplet retrieval.                              |
| **A-Mem** (2502.12110)         | Self-organizing memory without predefined schemas. Zettelkasten-inspired linking. Informed our "one store, multiple projections" decision.                   |
| **Memory Survey** (2512.13564) | Three-pass consolidation taxonomy: local dedup → cluster fusion → global integration. Forgetting via time expiration, access frequency, informational value. |
| **AgeMem** (2601.01885)        | Agentic memory framework where LTM and STM are jointly managed via explicit tool-based operations. Validated our trait-based design.                         |
| **SparseMemFT**                | Sparse memory fine-tuning. Informed our decision to keep parameter updates out of scope (engine is retrieval-only, not fine-tuning).                         |
| **Memento**                    | Hierarchical memory with summarization. Informed consolidation levels (local → cluster → global).                                                            |
| **Doc-to-LoRA**                | Document-to-adapter pipeline. Confirmed our boundary: engine stores and retrieves, consumer decides what to do with results.                                 |

### Storage Technology Decisions

Research (OQ1 in `docs/research/08-open-questions-research.md`) evaluated 4 storage stacks. The papers use diverse backends — Neo4j (Graphiti/Zep), Qdrant (Mem0), custom (A-Mem) — but all are external services. Our constraint: **embedded, single-process, no JVM/external DB**.

| Technology            | Research Verdict                                                         | Current Status        | Rationale                                                                                                                                                                             |
| --------------------- | ------------------------------------------------------------------------ | --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Neo4j**             | Discarded                                                                | —                     | External JVM process. Mature graph + temporal support, but overkill for embedded single-agent use. Graphiti/Zep use it as a service.                                                  |
| **Qdrant**            | Discarded                                                                | —                     | External service. Mem0 uses it for vector search, but violates our embedded constraint.                                                                                               |
| **SurrealDB 3.0**     | Deferred                                                                 | Monitor stability     | "Best if stable" — Rust-native, vector+graph+KV+temporal in single binary. Too young (Feb 2026) when we evaluated. Event-sourced log enables replay-based migration if it matures.    |
| **LanceDB**           | Deferred → Issue [#3](https://github.com/dutiona/memory-engine/issues/3) | Candidate for Phase 3 | Original plan for vector search (embedded, Rust-native, columnar, versioned). Dropped for brute-force cosine in Phase 1 — <50ms at expected scale. Revisit when benchmarks show need. |
| **SQLite + Petgraph** | Adopted                                                                  | Phase 1 ✅            | Battle-tested, embedded, simple. FTS5 for keyword search. WAL for concurrent reads. Petgraph for in-memory graph loaded from SQLite.                                                  |

**Migration safety net:** The event-sourced architecture guarantees we can replay into any storage backend. No lock-in.

### Key Design Decisions (from research convergence)

1. **No layers** — One store, multiple projections. `fact_type` is a tag, not a partition. The 5-layer hierarchy (L0-L4) collapsed during design.
2. **Traits for LLM ops** — `EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`. Engine has zero network/LLM dependencies.
3. **Event-sourced** — Append-only event log is source of truth. Facts are consumer-derived (explicit `add_fact`), not auto-projected.
4. **Soft deletion** — Facts are expired (`t_expired` set), never hard-deleted. Full audit trail for temporal reasoning.
5. **Synchronous** — No async. SQLite is local I/O. Async adapter is future work.
6. **Single-writer, `!Send`** — `MemoryEngine` owns one `Connection`, not thread-safe. Consumer wraps in actor or mutex.
7. **Brute-force vector** — O(N) scan with cosine similarity. Migrate to ANN when benchmarks show need.

---

## Phases

### Phase 1: Ingest → Query Loop ✅

**PR:** [#2](https://github.com/dutiona/memory-engine/pull/2) (squash-merged)
**Plan:** [Issue #1](https://github.com/dutiona/memory-engine/issues/1) (closed)
**Tasks:** 1-8, 14 from the original plan

| Task | Component                                                 | Status |
| ---- | --------------------------------------------------------- | ------ |
| 1    | Core types (`Event`, `Fact`, `Edge`, `Summary`, enums)    | ✅     |
| 2    | Error types (`MemoryError` with thiserror)                | ✅     |
| 3    | Schema + migrations (DDL, FTS5 triggers, indexes, config) | ✅     |
| 4    | Event log (append-only `EventStore`)                      | ✅     |
| 5    | Fact CRUD (bi-temporal, embedding BLOBs, blake3 hashing)  | ✅     |
| 6    | FTS5 search (BM25 ranking)                                | ✅     |
| 7    | Vector search (pure Rust cosine, partial sort)            | ✅     |
| 8    | Hybrid search (RRF merge, `SearchQuery` API)              | ✅     |
| 14   | Engine facade (`MemoryEngine` with Phase 2 stubs)         | ✅     |

**Deliverable:** A working `MemoryEngine` with `ingest()`, `add_fact()`, `query()`. 59 tests, 0 failures.

**Implementation learnings applied:**

- Hash truncation: blake3 hex[:32] (128 bits), not [:16]
- Error propagation: `?` not `unwrap_or_default()` in hybrid search
- FTS row resilience: `filter_map` + `tracing::warn!` per-row, not all-or-nothing
- Vector search: `select_nth_unstable_by` partial sort for top-K
- Query embedding dimension validation at the search boundary
- Redundant content_hash computation removed from engine facade

---

### Phase 2: Graph, Consolidation, Forgetting, Conflict Resolution ✅

**PR:** [#8](https://github.com/dutiona/memory-engine/pull/8) (squash-merged)
**Plan:** Comment on [Issue #1](https://github.com/dutiona/memory-engine/issues/1) (closed)
**Tasks:** 9-13, 14b from the original plan

| Task | Component                                                      | Status |
| ---- | -------------------------------------------------------------- | ------ |
| 9    | Graph module (`MemoryGraph` petgraph wrapper, `EdgeStore`)     | ✅     |
| 10   | Summary store (`SummaryStore` with embedding BLOBs)            | ✅     |
| 11   | Forgetting (Ebbinghaus decay, multi-signal importance scoring) | ✅     |
| 12   | Consolidation (three-pass: dedup, cluster fusion, global)      | ✅     |
| 13   | Conflict resolution (bi-temporal, `ConflictArbiter` trait)     | ✅     |
| 14b  | Engine facade wiring (replaced all `NotImplemented` stubs)     | ✅     |

**Deliverable:** All 5 primitives fully operational: `ingest()`, `add_fact()`, `query()`, `consolidate()`, `forget()`, `resolve_conflict()`. 107 tests, 0 failures.

**Implementation learnings applied:**

- **Edge cascade invariant:** Every fact expiry (conflict, forgetting, dedup) must also expire its edges in SQLite and update the in-memory graph. This was caught by multi-model review and applied uniformly across all three codepaths.
- **Atomic consolidation:** All three passes (dedup, cluster, global) wrapped in a single `unchecked_transaction`. Generator failures roll back cleanly.
- **O(degree) edge removal:** `petgraph::visit::EdgeRef` + `edges_directed()` instead of scanning all edges.
- **Dedup double-expire bug:** Inner loop must `break` when the new_fact itself is the one expired, preventing re-expiry on subsequent iterations.
- **Graph rebuild after consolidation:** Engine rebuilds in-memory graph from SQLite after dedup removes facts, keeping degree-based scoring consistent for subsequent `forget()` calls.
- **Stale config handling:** `last_consolidated_at` parse errors return `MemoryError::Migration` instead of being silently ignored.
- **Importance scoring normalization:** `ln_1p` for numerical accuracy near zero, with named constants for ceiling values (101.0 for frequency, 51.0 for connectivity).

---

### Phase 3: Hardening & Scalability ✅

**PR:** [#20](https://github.com/dutiona/memory-engine/pull/20) (squash-merged)
**Plan:** [Issue #15](https://github.com/dutiona/memory-engine/issues/15)

| Task | Component                                                   | Status |
| ---- | ----------------------------------------------------------- | ------ |
| T1   | Thread safety (`Send + Sync`, `parking_lot`)                | ✅     |
| T2   | `AddFactOptions` builder (importance, metadata, temporal)   | ✅     |
| T3   | Connection pool (N readers + 1 writer, WAL)                 | ✅     |
| T4   | Async engine (`AsyncMemoryEngine`, tokio `spawn_blocking`)  | ✅     |
| T5   | Schema migration framework (versioned, forward-only, v1→v2) | ✅     |
| T6   | Hierarchical scoping (`ScopeTree`, path resolution)         | ✅     |
| T7   | Criterion benchmarks (search, ingest, consolidate)          | ✅     |
| T8   | FTS/SQL-level scope filtering                               | ✅     |
| T9   | `resume_context()` — tiered cognitive boot                  | ✅     |

**Deliverable:** Thread-safe engine with connection pool, async wrapper, scoping, migration framework, and `resume_context()`. 158 tests.

---

### Phase 3b: Temporal Memory & Agent Lifecycle ✅

**PR:** (pending)
**Plan:** [Issue #25](https://github.com/dutiona/memory-engine/issues/25)

| Task | Component                                                            | Status |
| ---- | -------------------------------------------------------------------- | ------ |
| 1    | Schema migration v2→v3 (is_pinned, importance_score, event envelope) | ✅     |
| 2    | Type changes (Fact, NewFact, Event, NewEvent, AddFactOptions)        | ✅     |
| 3    | FactStore: list_pinned, list_due, next_due_time, set_pinned          | ✅     |
| 4    | `PersistenceClassifier` trait (auto-pinning)                         | ✅     |
| 5    | Forgetting: pinned bypass + importance_score materialization         | ✅     |
| 6    | Consolidation: pinned skip + importance_score inheritance            | ✅     |
| 7    | `resume_context()` rework — 5-tier pipeline                          | ✅     |
| 8    | Engine facade: drain_due, next_due_time, pin/unpin, classifier       | ✅     |
| 9    | AsyncMemoryEngine mirror                                             | ✅     |
| 10   | Documentation                                                        | ✅     |
| 11   | Integration tests                                                    | ✅     |

**Deliverable:** Time-aware memory with unforgettable facts, future memory surfacing, scheduling API, 5-tier cognitive boot sequence. 183 tests (179 unit + 4 integration).

**Key features:**

- **Pinned facts** (`is_pinned`) — unforgettable, bypass forgetting and dedup. Agent identity and core beliefs.
- **Future memory** (`t_valid`) — facts with `t_valid` in the future remain invisible until their scheduled time. `drain_due(now)` for incremental, `resume_context()` for full boot.
- **Scheduling API** — `drain_due()`, `next_due_time()` let consumers implement timer-based polling.
- **`PersistenceClassifier` trait** — consumer-provided auto-pinning logic. Explicit `opts.pinned` always overrides.
- **Materialized `importance_score`** — composite score updated during `prune()` and `consolidate()`. O(1) sort in `resume_context()`.
- **Event envelope** — `origin_node_id`, `sequence_id`, `created_at` for future multi-node sync. No behavioral change.
- **5-tier `resume_context()`** — pinned → high_importance → due → recent → kb_stubs.

**Implementation learnings:**

- `importance_score` is on `Fact` only (engine-computed composite), not on `NewFact` (consumer input). DB default handles it.
- `PersistenceClassifier` receives a synthetic `Fact` with `id=0` during `add_fact()` — classifiers should rely on `content`, `fact_type`, `importance`, `metadata` only.
- Event envelope fields are metadata-only — no behavioral change, defaults to `'local'/0/NULL`.
- Pinned facts are cross-scope in `resume_context()` tier 1 (always present regardless of scope filter).
- Consolidation dedup skips pinned facts in both inner and outer loops to prevent merging identity facts.

---

### Future (not planned)

These are not committed to any phase. They represent directions the research suggests but that we haven't validated need for.

- **MCP server adapter** — Expose engine as a Model Context Protocol tool server
- **Hierarchical summarization** — Multi-level abstractions (Memento-style). Currently consolidation is flat (local/cluster/global).
- **Cross-session memory sharing** — Multiple agents sharing one store. Requires session isolation or namespacing.
- **Multimodal memory** — Image/audio embeddings alongside text. Schema supports it (BLOB embeddings are dimension-agnostic) but no consumer integration.
- **Parameter updates** — Doc-to-LoRA style adapter generation from memory contents. Explicitly out of scope — engine is retrieval-only.
- **Knowledge Base integration** — `kb_stubs` placeholder in `ResumeContext` for Phase 5 external knowledge references.
- **Multi-node sync** — Event envelope fields (`origin_node_id`, `sequence_id`) are forward-compatible for distributed sync.

---

## Architecture

```
Consumer (AI agent, CLI tool, MCP server)
    │
    ▼
┌──────────────────────────────────────────┐
│           MemoryEngine (Send + Sync)     │
│  ingest · add_fact · query               │  ← Phase 1
│  consolidate · forget · resolve          │  ← Phase 2
│  resume_context · drain_due · pin/unpin  │  ← Phase 3/3b
├──────────────────────────────────────────┤
│  AsyncMemoryEngine (tokio wrapper)       │  ← Phase 3
├──────────────────────────────────────────┤
│  ConnectionPool (N readers + 1 writer)   │  ← Phase 3
├──────────────────────────────────────────┤
│  Search                                  │
│  ├─ FTS5 (BM25, scope-filtered)          │
│  ├─ Vector (cosine, brute-force)         │
│  └─ Hybrid (RRF k=60)                   │
├──────────────────────────────────────────┤
│  Store                                   │
│  ├─ EventStore (append-only log)         │
│  ├─ FactStore (bi-temporal + pinned)     │
│  ├─ EdgeStore (graph persistence)        │
│  ├─ SummaryStore                         │
│  └─ ScopeStore (hierarchical scoping)   │  ← Phase 3
├──────────────────────────────────────────┤
│  Graph (petgraph DiGraph)                │
├──────────────────────────────────────────┤
│  Forgetting (Ebbinghaus + pinned bypass) │  ← Phase 3b
├──────────────────────────────────────────┤
│  Consolidation (3-pass + pinned skip)    │  ← Phase 3b
├──────────────────────────────────────────┤
│  Conflict (bi-temporal arbiter)          │
├──────────────────────────────────────────┤
│  Resume (5-tier cognitive boot)          │  ← Phase 3b
└──────────────────────────────────────────┘
    │
    ▼
  SQLite WAL (rusqlite bundled-full)
  ├─ events, facts, edges, summaries, scopes, config
  ├─ facts_fts (FTS5 virtual table)
  └─ 19 indexes (schema v3)
```

## Consumer-Provided Traits

```
EmbeddingProvider::embed(text) → Vec<f32>              ← Phase 1
SummaryGenerator::summarize(facts) → String            ← Phase 2
SummaryGenerator::embed(text) → Vec<f32>               ← Phase 2
ConflictArbiter::arbitrate(old, new) → CrudDecision    ← Phase 2
PersistenceClassifier::should_pin(fact) → bool         ← Phase 3b
```

The engine has zero LLM/network dependencies. All intelligence is injected by the consumer via these traits.
