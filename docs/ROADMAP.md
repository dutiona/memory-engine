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

### Phase 2: Graph, Consolidation, Forgetting, Conflict Resolution 🔲

**Tasks:** 9-13 from the original plan
**Dependencies:** All build on Phase 1's schema, fact store, and vector search.

| Task | Component           | Depends On                   | Description                                                                                                                                                                                       |
| ---- | ------------------- | ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 9    | Graph module        | Schema (T3)                  | `MemoryGraph` (petgraph `DiGraph`), `EdgeStore` (SQLite persistence), `neighbors`, `degree`, `connected_component`, `shortest_path`. Write-then-update sync: SQLite first, petgraph after commit. |
| 10   | Summary store       | Schema (T3)                  | `SummaryStore` for consolidation outputs. Embedding BLOBs + JSON `source_fact_ids`.                                                                                                               |
| 11   | Forgetting          | Graph (T9), Facts (T5)       | Ebbinghaus decay with per-`fact_type` half-life overrides. Multi-signal importance: recency + frequency + graph degree + base. Soft-delete via `t_expired`.                                       |
| 12   | Consolidation       | Vector (T7), Summaries (T10) | Three-pass: local dedup (cosine >0.92, O(new×active)), cluster fusion (single-linkage + `SummaryGenerator` trait), global integration.                                                            |
| 13   | Conflict resolution | Graph (T9), Facts (T5)       | `ConflictArbiter` trait → `CrudDecision` (Add/Update/Delete/Noop). Creates graph edges ("contradicts", "supplements"). Bi-temporal update of `t_expired`/`t_invalid`.                             |

**Parallelizable:** Tasks 9+10 can run in parallel. Task 11 needs 9. Task 12 needs 10+vector. Task 13 needs 9.

**Engine facade updates:** Wire `consolidate()`, `forget()`, `resolve_conflict()` to replace `NotImplemented` stubs. Add `graph()` accessor.

---

### Phase 3: Hardening & Scalability 🔲

Design tensions surfaced during Phase 1 implementation and code review. Tracked as individual issues.

| Issue                                                   | Concern                                                  | Trigger                                                |
| ------------------------------------------------------- | -------------------------------------------------------- | ------------------------------------------------------ |
| [#3](https://github.com/dutiona/memory-engine/issues/3) | Brute-force vector search → ANN index                    | Fact count exceeds ~50K-100K                           |
| [#4](https://github.com/dutiona/memory-engine/issues/4) | Post-overfetch filtering → SQL-level filters             | Skewed type distributions or restrictive `valid_at`    |
| [#5](https://github.com/dutiona/memory-engine/issues/5) | `!Send`/`!Sync` → thread-safe engine                     | Async-first consumers, concurrent MCP requests         |
| [#6](https://github.com/dutiona/memory-engine/issues/6) | Hardcoded `add_fact` defaults → `AddFactOptions` builder | Consumers needing per-fact importance/metadata control |

**Approach:** Data-driven. Add criterion benchmarks first, then migrate when numbers justify it. No speculative optimization.

---

### Future (not planned)

These are not committed to any phase. They represent directions the research suggests but that we haven't validated need for.

- **Async API** — `async fn query(...)` wrapper, likely via `tokio::task::spawn_blocking`
- **MCP server adapter** — Expose engine as a Model Context Protocol tool server
- **Schema migrations** — `schema_version` is 1. Migration framework when schema evolves.
- **Hierarchical summarization** — Multi-level abstractions (Memento-style). Currently consolidation is flat (local/cluster/global).
- **Cross-session memory sharing** — Multiple agents sharing one store. Requires session isolation or namespacing.
- **Multimodal memory** — Image/audio embeddings alongside text. Schema supports it (BLOB embeddings are dimension-agnostic) but no consumer integration.
- **Parameter updates** — Doc-to-LoRA style adapter generation from memory contents. Explicitly out of scope — engine is retrieval-only.

---

## Architecture

```
Consumer (AI agent, CLI tool, MCP server)
    │
    ▼
┌──────────────────────────────────────┐
│           MemoryEngine               │
│  ingest · add_fact · query           │  ← Phase 1 ✅
│  consolidate · forget · resolve      │  ← Phase 2
├──────────────────────────────────────┤
│  Search                              │
│  ├─ FTS5 (BM25)                      │
│  ├─ Vector (cosine, brute-force)     │
│  └─ Hybrid (RRF k=60)               │
├──────────────────────────────────────┤
│  Store                               │
│  ├─ EventStore (append-only log)     │
│  ├─ FactStore (bi-temporal + BLOB)   │
│  ├─ EdgeStore (graph persistence)    │  ← Phase 2
│  └─ SummaryStore                     │  ← Phase 2
├──────────────────────────────────────┤
│  Graph (petgraph DiGraph)            │  ← Phase 2
├──────────────────────────────────────┤
│  Forgetting (Ebbinghaus decay)       │  ← Phase 2
├──────────────────────────────────────┤
│  Consolidation (3-pass)              │  ← Phase 2
├──────────────────────────────────────┤
│  Conflict (bi-temporal arbiter)      │  ← Phase 2
└──────────────────────────────────────┘
    │
    ▼
  SQLite WAL (rusqlite bundled-full)
  ├─ events, facts, edges, summaries, config
  ├─ facts_fts (FTS5 virtual table)
  └─ 9 indexes
```

## Consumer-Provided Traits

```
EmbeddingProvider::embed(text) → Vec<f32>        ← Phase 1 ✅
SummaryGenerator::summarize(facts) → String      ← Phase 2
SummaryGenerator::embed(text) → Vec<f32>         ← Phase 2
ConflictArbiter::arbitrate(old, new) → CrudDecision  ← Phase 2
```

The engine has zero LLM/network dependencies. All intelligence is injected by the consumer via these traits.
