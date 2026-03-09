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

### Phase 3: Hardening & Scoping 🔄

**Branch:** `feat/memory-engine-phase3`

| Feature                                                            | Status |
| ------------------------------------------------------------------ | ------ |
| Hierarchical scoping (`ScopeTree`, `ScopeStore`)                   | ✅     |
| `scope_id` on facts, edges, events, summaries + migration          | ✅     |
| SQL-level filters (`t_expired`, `fact_type`, `scope_id`)           | ✅     |
| `ConnectionPool` (N readers + 1 writer, parking_lot)               | ✅     |
| `MemoryEngine` Send+Sync with RwLock                               | ✅     |
| `AsyncMemoryEngine` (tokio spawn_blocking)                         | ✅     |
| `resume_context()` — basic, scope-aware (needs rework in 3b)       | ✅     |
| Criterion benchmarks (vector, FTS, hybrid, scoped)                 | ✅     |
| `AddFactOptions` builder (per-fact importance, metadata, temporal) | ✅     |
| Schema migration framework (versioned, forward-only)               | ✅     |

---

### Phase 3b: Temporal Memory & Agent Lifecycle 🔲

**Design:** [`docs/plans/2026-03-09-future-phases-design.md`](plans/2026-03-09-future-phases-design.md)

| Feature                 | Description                                                            |
| ----------------------- | ---------------------------------------------------------------------- |
| Unforgettable flag      | `is_pinned: bool` on facts, `PersistenceClassifier` trait hook         |
| Future memory           | `t_valid` filter — facts surface when their date arrives               |
| Scheduling API          | `resume_context(now)` rework, `drain_due(now)`, `next_due_time()`      |
| Materialized importance | Persist importance score on facts for fast retrieval                   |
| Event envelope v2       | `origin_node_id`, `sequence_id`, advisory `created_at` for future sync |

---

### Phase 4: Operability & MCP Server 🔲

**Design:** [`docs/plans/2026-03-09-future-phases-design.md`](plans/2026-03-09-future-phases-design.md)

| Feature                     | Description                                                         |
| --------------------------- | ------------------------------------------------------------------- |
| Inspection APIs             | `explain_fact()`, `replay_events()`, `dump_state()`, `statistics()` |
| CLI inspector               | `memory-engine-cli` — clean operator tool (like `gh`)               |
| MCP server                  | `memory-engine-mcp` workspace member, eventual separate repo        |
| Import/export               | JSON event log + SQLite backup, gzip/zstd compression               |
| Archival compression        | Cold storage `.pak` files for old non-pinned facts                  |
| Semantic extraction queries | Scope + temporal range + semantic search composition                |

---

### Phase 5: Knowledge Integration 🔲

**Design:** [`docs/plans/2026-03-09-future-phases-design.md`](plans/2026-03-09-future-phases-design.md)

| Feature                        | Description                                     |
| ------------------------------ | ----------------------------------------------- |
| `KnowledgeBaseConnector` trait | Transport-agnostic, consumer-implemented        |
| `KnowledgeRef` on facts        | URI field pointing to KB content                |
| Graceful degradation           | "Memory lapse" when KB unreachable, retry later |
| research-index bridge          | `memory-kb-research-index` middleware crate     |

---

### Deferred (not planned, trigger-based)

Tracked as individual GitHub issues. See design doc for details.

- **ANN vector index** — Trigger: benchmarks show >50ms at scale
- **Web UI** — WASM+Rust graph visualization. Separate project.
- **Auth** — Deployment layer concern, not engine core.
- **SaaS Sync** — Separate product. CRDT event-log merge + E2EE.
- **Evaluation harness** — Regression corpus for retrieval/consolidation quality.
- **Hierarchical summarization** — Multi-level abstractions (Memento-style).
- **Schema evolution discipline** — Versioning policy, backwards-compat testing.
- **Determinism guarantees** — Replay, merge, idempotency rules for sync.
- **Multimodal memory** — Image/audio embeddings. Schema supports it (BLOB).
- **Parameter updates** — Doc-to-LoRA. Out of scope — engine is retrieval-only.

---

## Architecture

```
Consumer (AI agent, CLI tool, MCP server)
    │
    ▼
┌──────────────────────────────────────┐
│           MemoryEngine               │
│  ingest · add_fact · query           │  ← Phase 1 ✅
│  consolidate · forget · resolve      │  ← Phase 2 ✅
│  resume_context · scoped queries     │  ← Phase 3
│  drain_due · next_due_time           │  ← Phase 3b
├──────────────────────────────────────┤
│  Search                              │
│  ├─ FTS5 (BM25)                      │
│  ├─ Vector (cosine, brute-force)     │
│  └─ Hybrid (RRF k=60)               │
├──────────────────────────────────────┤
│  Scoping                             │  ← Phase 3
│  ├─ ScopeTree (hierarchical)         │
│  └─ ScopeStore (SQLite-backed)       │
├──────────────────────────────────────┤
│  Store                               │
│  ├─ EventStore (append-only log)     │
│  ├─ FactStore (bi-temporal + BLOB)   │
│  ├─ EdgeStore (graph persistence)    │  ← Phase 2 ✅
│  └─ SummaryStore                     │  ← Phase 2 ✅
├──────────────────────────────────────┤
│  Graph (petgraph DiGraph)            │  ← Phase 2 ✅
├──────────────────────────────────────┤
│  Forgetting (Ebbinghaus decay)       │  ← Phase 2 ✅
├──────────────────────────────────────┤
│  Consolidation (3-pass)              │  ← Phase 2 ✅
├──────────────────────────────────────┤
│  Conflict (bi-temporal arbiter)      │  ← Phase 2 ✅
├──────────────────────────────────────┤
│  ConnectionPool (N readers + 1 wr)   │  ← Phase 3
├──────────────────────────────────────┤
│  AsyncMemoryEngine (spawn_blocking)  │  ← Phase 3
└──────────────────────────────────────┘
    │
    ▼
  SQLite WAL (rusqlite bundled-full)
  ├─ events, facts, edges, summaries, scopes, config
  ├─ facts_fts (FTS5 virtual table)
  └─ indexes + schema migration framework
```

## Consumer-Provided Traits

```
EmbeddingProvider::embed(text) → Vec<f32>              ← Phase 1 ✅
SummaryGenerator::summarize(facts) → String            ← Phase 2 ✅
SummaryGenerator::embed(text) → Vec<f32>               ← Phase 2 ✅
ConflictArbiter::arbitrate(old, new) → CrudDecision    ← Phase 2 ✅
PersistenceClassifier::should_pin(fact) → bool         ← Phase 3b
KnowledgeBaseConnector::resolve(uri) → KnowledgeChunk  ← Phase 5
```

The engine has zero LLM/network dependencies. All intelligence is injected by the consumer via these traits.

## Conceptual Boundaries

```
Knowledge (raw content)     → Knowledge Base (e.g., research-index)
Memory (internalized facts) → Memory Engine (this project)
Wisdom (meta-reasoning)     → The model itself (out of scope)
```

See [`docs/plans/2026-03-09-future-phases-design.md`](plans/2026-03-09-future-phases-design.md) for full rationale.
