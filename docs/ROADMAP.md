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

| Technology            | Research Verdict | Current Status     | Rationale                                                                                                                                                                                                                       |
| --------------------- | ---------------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Neo4j**             | Discarded        | —                  | External JVM process. Mature graph + temporal support, but overkill for embedded single-agent use. Graphiti/Zep use it as a service.                                                                                            |
| **Qdrant**            | Discarded        | —                  | External service. Mem0 uses it for vector search, but violates our embedded constraint.                                                                                                                                         |
| **SurrealDB 3.0**     | Deferred         | Monitor stability  | "Best if stable" — Rust-native, vector+graph+KV+temporal in single binary. Too young (Feb 2026) when we evaluated. Event-sourced log enables replay-based migration if it matures.                                              |
| **LanceDB**           | Deferred         | Superseded by HNSW | Original ANN candidate (embedded, Rust-native, columnar). HNSW (`hnsw` crate) chosen instead for in-memory ANN behind `ann` feature flag. LanceDB remains a hypothetical future option for disk-backed ANN if data exceeds RAM. |
| **SQLite + Petgraph** | Adopted          | Phase 1 ✅         | Battle-tested, embedded, simple. FTS5 for keyword search. WAL for concurrent reads. Petgraph for in-memory graph loaded from SQLite.                                                                                            |

**Migration safety net:** The event-sourced architecture guarantees we can replay into any storage backend. No lock-in.

### Key Design Decisions (from research convergence)

1. **No layers** — One store, multiple projections. `fact_type` is a tag, not a partition. The 5-layer hierarchy (L0-L4) collapsed during design.
2. **Traits for LLM ops** — `EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`. Engine has zero network/LLM dependencies.
3. **Event-sourced** — Append-only event log is source of truth. Facts are consumer-derived (explicit `add_fact`), not auto-projected.
4. **Soft deletion** — Facts are expired (`t_expired` set), never hard-deleted. Full audit trail for temporal reasoning.
5. **Async via spawn_blocking** — `AsyncMemoryEngine` wraps sync calls in `tokio::spawn_blocking`. SQLite is local I/O; true async would over-complicate.
6. **Send + Sync** — `MemoryEngine` uses `ConnectionPool` (N readers + 1 writer) with `parking_lot::RwLock`. Thread-safe by default since Phase 3.
7. **Pluggable vector search** — Brute-force cosine by default; HNSW ANN behind `ann` feature flag (Phase 3). `VectorSearchStrategy` trait for future backends.

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
**Plan:** Issues [#3](https://github.com/dutiona/memory-engine/issues/3)–[#7](https://github.com/dutiona/memory-engine/issues/7) (originally scoped as "Phase 2", executed here)

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

**PR:** [#34](https://github.com/dutiona/memory-engine/pull/34) (cherry-picked into main)
**Plan:** [Issue #25](https://github.com/dutiona/memory-engine/issues/25)

| Task | Component                                                            | Status |
| ---- | -------------------------------------------------------------------- | ------ |
| 1    | Schema migration v3→v4 (is_pinned, importance_score, event envelope) | ✅     |
| 2    | Type changes (Fact, NewFact, Event, NewEvent, AddFactOptions)        | ✅     |
| 3    | FactStore: list_pinned, list_due, next_due_time, set_pinned          | ✅     |
| 4    | `PersistenceClassifier` trait (auto-pinning)                         | ✅     |
| 5    | Forgetting: pinned bypass + importance_score materialization         | ✅     |
| 6    | Consolidation: pinned skip + importance_score inheritance            | ✅     |
| 7    | `resume_context()` rework — 5-tier pipeline                          | ✅     |
| 8    | Engine facade: list_due, next_due_time, pin/unpin, classifier        | ✅     |
| 9    | AsyncMemoryEngine mirror                                             | ✅     |
| 10   | Documentation                                                        | ✅     |
| 11   | Integration tests                                                    | ✅     |

**Deliverable:** Time-aware memory with unforgettable facts, future memory surfacing, scheduling API, 5-tier cognitive boot sequence.

**Key features:**

- **Pinned facts** (`is_pinned`) — unforgettable, bypass forgetting and dedup. Agent identity and core beliefs.
- **Future memory** (`t_valid`) — facts with `t_valid` in the future remain invisible until their scheduled time. `list_due(now)` for incremental, `resume_context()` for full boot.
- **Scheduling API** — `list_due()`, `next_due_time()` let consumers implement timer-based polling.
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

### Phase 4: Operability & MCP Server 🔲

**Design:** [`docs/design/plans/2026-03-09-future-phases-design.md`](design/plans/2026-03-09-future-phases-design.md)

#### Prerequisites (gate Phase 4 — can be done in parallel)

| Item                                                                                    | Description                                                                                                                                                               |
| --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Documentation gap ([#35](https://github.com/dutiona/memory-engine/issues/35))           | ✅ Updated 13 doc files for Phase 3b features (pinned facts, 5-tier resume, scheduling API, classifier). [PR #60](https://github.com/dutiona/memory-engine/pull/60)       |
| Schema evolution discipline ([#18](https://github.com/dutiona/memory-engine/issues/18)) | ✅ Storage epoch versioning, WAL-safe backup, event envelope versioning, upcaster registry, migration testing. [PR #61](https://github.com/dutiona/memory-engine/pull/61) |

#### Phase 4a: Introspection & Data (library)

| Feature                                                                                 | Description                                                                               |
| --------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Inspection APIs ([#39](https://github.com/dutiona/memory-engine/issues/39))             | `explain_fact()`, `replay_events()`, `dump_state()`, `statistics()`                       |
| Import/export ([#40](https://github.com/dutiona/memory-engine/issues/40))               | JSON event log + SQLite backup, gzip/zstd compression                                     |
| Semantic extraction queries ([#41](https://github.com/dutiona/memory-engine/issues/41)) | Query builder composing scope + temporal range + semantic search                          |
| `Reranker` trait ([#42](https://github.com/dutiona/memory-engine/issues/42))            | Cross-encoder reranking on top-K candidates after RRF (+5-15% nDCG@10). Consumer-provided |
| Session log bootstrap ([#43](https://github.com/dutiona/memory-engine/issues/43))       | Parse Claude Code JSONL session logs into historical memory facts                         |

#### Phase 4b: Tooling (new workspace binaries)

| Feature                                                                   | Description                                                                                                 |
| ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| CLI inspector ([#44](https://github.com/dutiona/memory-engine/issues/44)) | `memory-engine-cli` — operator tool (subcommands: inspect, dump, query, explain, stats, import, export)     |
| MCP server ([#45](https://github.com/dutiona/memory-engine/issues/45))    | `memory-engine-mcp` — maps 1:1 to engine API. Includes pre-compaction flush endpoint for push-based capture |

#### Phase 4c: Quality & Cold Storage

| Feature                                                                          | Description                                                                                               |
| -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| Evaluation harness ([#16](https://github.com/dutiona/memory-engine/issues/16))   | Regression corpus for retrieval quality, consolidation correctness, forgetting behavior. After 4a/4b ship |
| Archival compression ([#46](https://github.com/dutiona/memory-engine/issues/46)) | Cold storage `.pak` files for old non-pinned facts (zstd, explicit trigger, slow fallback)                |
| Fast cold-start ([#31](https://github.com/dutiona/memory-engine/issues/31))      | Snapshot + incremental replay for rapid engine boot                                                       |

---

### Phase 5: Cognitive Pipelines 🔲

**Theme:** Close the Memory → Wisdom gap identified by the [four-layer cognitive architecture](https://github.com/dutiona/research-index/blob/master/docs/insights/four-layer-cognitive-architecture.md). Make the engine self-improving.

**Design:** Community research synthesis (5 projects) + three-way debate (Claude/Codex/Gemini, 2 rounds, 7 questions). See `docs/design/debate-phase5/synthesis.md` and `docs/design/2026-03-12-community-research-synthesis.md`.

| Feature                                                                                                                                                               | Description                                                                                                                                                                |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| InsightStream trait — fast-path capture ([#48](https://github.com/dutiona/memory-engine/issues/48))                                                                   | `record()` method for high-value observations. Consumer-implemented. **Note:** Gemini dissent — may simplify to `FactType::Insight` via `add_fact()` during implementation |
| DreamCycle trait — cognitive pipeline ([#49](https://github.com/dutiona/memory-engine/issues/49), [#47](https://github.com/dutiona/memory-engine/issues/47) absorbed) | Full batch pipeline: consolidation → pattern detection → behavioral compression → promotion → rescoring. Returns `CycleReport`                                             |
| `sample_dormant()` API ([#54](https://github.com/dutiona/memory-engine/issues/54))                                                                                    | Passive resonance for autonomous agents. HNSW search filtered for dormant facts. Consumer-driven                                                                           |
| Provenance infrastructure ([#55](https://github.com/dutiona/memory-engine/issues/55))                                                                                 | `PromotionProvenance` envelope + sidecar `LineageTable` in SQLite. Source fact expiry (`t_expired` set) with lineage preservation                                          |
| `DreamCycleConfig` ([#56](https://github.com/dutiona/memory-engine/issues/56))                                                                                        | Per-FactType compression ratios, ±2 symmetric rescoring, quarantine path for contradictions                                                                                |
| Three-layer identity output ([#57](https://github.com/dutiona/memory-engine/issues/57))                                                                               | ANCHORS/CORE/PREDICTIONS structure in `CycleReport`. Each item: `{pattern, directive, false_positive}`                                                                     |

#### Sub-phasing

- **Phase 5a (Minimum Viable Cognitive Pipeline):**
  - InsightStream trait (or `FactType::Insight` — decide during implementation)
  - DreamCycle trait
  - `PromotionProvenance` + `LineageTable`
  - `DreamCycleConfig`
  - three-layer identity output
- **Phase 5b (Behavioral Intelligence):** Targeted scanning (correction pairs, avoidance patterns), quarantine/suppress path for contradictions
- **Phase 5 (independent, any time):** `sample_dormant()` API (passive resonance for autonomous agents)
- **Deferred (not in Phase 5):** `compress_behavior()` hook on DreamCycle (depends on consumer LLM integration)

---

### Phase 6: Knowledge Integration 🔲

**Design:** [`docs/design/plans/2026-03-09-future-phases-design.md`](design/plans/2026-03-09-future-phases-design.md)

| Feature                                                                                                                            | Description                                                                               |
| ---------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `KnowledgeBaseConnector` trait + `KnowledgeRef` + graceful degradation ([#50](https://github.com/dutiona/memory-engine/issues/50)) | Transport-agnostic trait, optional URI field on facts, "memory lapse" when KB unreachable |
| Knowledge change notification ([#51](https://github.com/dutiona/memory-engine/issues/51))                                          | When KB content is superseded/updated, notify memory to re-evaluate dependent facts       |
| research-index bridge ([#52](https://github.com/dutiona/memory-engine/issues/52))                                                  | `memory-kb-research-index` middleware crate implementing `KnowledgeBaseConnector`         |

---

### Phase 7: Visualization 🔲

| Feature                                                            | Description                                                                                                                                                                 |
| ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Web UI ([#13](https://github.com/dutiona/memory-engine/issues/13)) | WASM+Rust graph visualization. Hybrid: petgraph+fdg-sim (WASM) for layout, sigma.js (WebGL) for rendering. Scope filtering, fact editing, import/export, event log timeline |

---

### Deferred (not planned, trigger-based)

Tracked as individual GitHub issues. Not scheduled for any phase — each has a trigger condition.

| Item                                                                                     | Trigger                                                                                               |
| ---------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Auth ([#14](https://github.com/dutiona/memory-engine/issues/14))                         | Multi-user deployment decision. Deployment layer concern, not engine core                             |
| SaaS Sync ([#15](https://github.com/dutiona/memory-engine/issues/15))                    | Product decision for multi-device. CRDT event-log merge + E2EE. Requires determinism guarantees first |
| Hierarchical summarization ([#17](https://github.com/dutiona/memory-engine/issues/17))   | Usage exceeds flat consolidation. Memento-style multi-level abstractions                              |
| Determinism guarantees ([#19](https://github.com/dutiona/memory-engine/issues/19))       | Before sync work begins. Replay, merge, idempotency rules                                             |
| Cross-session memory sharing ([#36](https://github.com/dutiona/memory-engine/issues/36)) | Multi-agent deployments. Session isolation or namespace via ScopeTree                                 |
| Multimodal memory ([#37](https://github.com/dutiona/memory-engine/issues/37))            | Non-text memories needed. Schema supports it (BLOB embeddings)                                        |
| Multi-node sync ([#38](https://github.com/dutiona/memory-engine/issues/38))              | Multi-device eventual consistency. Event envelope fields are forward-compatible                       |

---

## Architecture

```
Consumer (AI agent, CLI tool, MCP server)
    │
    ▼
┌──────────────────────────────────────────┐
│           MemoryEngine (Send + Sync)     │
│  ingest · add_fact · query               │  ← Phase 1 ✅
│  consolidate · forget · resolve          │  ← Phase 2 ✅
│  resume_context · list_due · pin/unpin  │  ← Phase 3/3b ✅
│  explain · replay · dump · statistics   │  ← Phase 4a
│  dream_cycle · sample_dormant           │  ← Phase 5
├──────────────────────────────────────────┤
│  AsyncMemoryEngine (tokio wrapper)       │  ← Phase 3 ✅
├──────────────────────────────────────────┤
│  ConnectionPool (N readers + 1 writer)   │  ← Phase 3 ✅
├──────────────────────────────────────────┤
│  Search                                  │
│  ├─ FTS5 (BM25, scope-filtered)          │
│  ├─ Vector (cosine / HNSW ANN)           │
│  ├─ Hybrid (RRF k=60)                   │
│  └─ Reranker (cross-encoder, optional)  │  ← Phase 4a
├──────────────────────────────────────────┤
│  Store                                   │
│  ├─ EventStore (append-only, upcasting)  │
│  ├─ FactStore (bi-temporal + pinned)     │
│  ├─ EdgeStore (graph persistence)        │
│  ├─ SummaryStore                         │
│  ├─ ScopeStore (hierarchical scoping)   │  ← Phase 3 ✅
│  └─ LineageTable (provenance sidecar)   │  ← Phase 5
├──────────────────────────────────────────┤
│  Graph (petgraph DiGraph)                │  ← Phase 2 ✅
├──────────────────────────────────────────┤
│  Forgetting (Ebbinghaus + pinned bypass) │  ← Phase 3b ✅
├──────────────────────────────────────────┤
│  Consolidation (3-pass + pinned skip)    │  ← Phase 3b ✅
├──────────────────────────────────────────┤
│  Conflict (bi-temporal arbiter)          │  ← Phase 2 ✅
├──────────────────────────────────────────┤
│  Resume (5-tier cognitive boot)          │  ← Phase 3b ✅
├──────────────────────────────────────────┤
│  Cognitive Pipelines                     │  ← Phase 5
│  ├─ InsightStream (fast-path capture)   │
│  └─ DreamCycle (full cognitive pipeline) │
├──────────────────────────────────────────┤
│  Knowledge Bridge                        │  ← Phase 6
│  ├─ KnowledgeRef (URI on facts)         │
│  └─ ChangeNotification (KB → memory)   │
└──────────────────────────────────────────┘
    │
    ▼
  SQLite WAL (rusqlite bundled-full)
  ├─ events, facts, edges, summaries, scopes, config
  ├─ facts_fts (FTS5 virtual table)
  └─ indexes + schema migration framework (v5, epoch-gated)
```

## Consumer-Provided Traits

```
EmbeddingProvider::embed(text) → Vec<f32>              ← Phase 1 ✅
SummaryGenerator::summarize(facts) → String            ← Phase 2 ✅
SummaryGenerator::embed(text) → Vec<f32>               ← Phase 2 ✅
ConflictArbiter::arbitrate(old, new) → CrudDecision    ← Phase 2 ✅
PersistenceClassifier::should_pin(fact) → bool         ← Phase 3b ✅
Reranker::rerank(query, candidates) → Vec<ScoredFact>  ← Phase 4a
InsightStream::record(insight) → Result<()>             ← Phase 5
DreamCycle::run(engine) → Result<CycleReport>           ← Phase 5
KnowledgeBaseConnector::resolve(uri) → KnowledgeChunk  ← Phase 6
```

The engine has zero LLM/network dependencies. All intelligence is injected by the consumer via these traits.

## Four-Layer Cognitive Architecture

```
Intelligence (inference-time) → The model (out of scope)
Wisdom (durable patterns)     → CLAUDE.md, skills, feedback files
Memory (internalized facts)   → Memory Engine (this project)
Knowledge (raw content)       → Knowledge Base (research-index)
```

The engine sits at the **Memory** layer. It consolidates experiences, forgets irrelevant ones, and (Phase 5) proposes promotions to the **Wisdom** layer. It does not own knowledge (that's research-index) or intelligence (that's the model). This separation prevents the category error of applying decay to knowledge or treating all persistent data identically.

See [`docs/design/plans/2026-03-09-future-phases-design.md`](design/plans/2026-03-09-future-phases-design.md) for Phase 4-6 design rationale.
See [four-layer cognitive architecture](https://github.com/dutiona/research-index/blob/master/docs/insights/four-layer-cognitive-architecture.md) for the foundational framework.
