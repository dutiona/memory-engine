# memory-engine Roadmap

## Research Foundation

9 core papers + 6 context adaptation papers, 22+ cross-paper relationships, community synthesis (OpenClaw, Reddit), 3-round multi-AI debate + context adaptation survey (2026-03-19).

| Paper                               | Key Contribution to Design                                                                                                                                                 |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **CoALA** (2309.02427)              | 4 memory types → `fact_type` tag (Episodic, Semantic, Procedural). Working memory stays in consumer's context window.                                                      |
| **Graphiti** (2410.13790)           | Bi-temporal model: 4 timestamps per fact (`t_created`/`t_expired` system, `t_valid`/`t_invalid` real-world). Conflict detection via LLM.                                   |
| **Mem0** (2504.19413)               | CRUD conflict resolution pattern (Add/Update/Delete/Noop). Graph-based memory with entity-centric + semantic triplet retrieval.                                            |
| **A-Mem** (2502.12110)              | Self-organizing memory without predefined schemas. Zettelkasten-inspired linking. Informed our "one store, multiple projections" decision.                                 |
| **Memory Survey** (2512.13564)      | Three-pass consolidation taxonomy: local dedup → cluster fusion → global integration. Forgetting via time expiration, access frequency, informational value.               |
| **AgeMem** (2601.01885)             | Agentic memory framework where LTM and STM are jointly managed via explicit tool-based operations. Validated our trait-based design.                                       |
| **SparseMemFT**                     | Sparse memory fine-tuning. Informed our decision to keep parameter updates out of scope (engine is retrieval-only, not fine-tuning).                                       |
| **Memento**                         | Hierarchical memory with summarization. Informed consolidation levels (local → cluster → global).                                                                          |
| **Doc-to-LoRA**                     | Document-to-adapter pipeline. Confirmed our boundary: engine stores and retrieves, consumer decides what to do with results.                                               |
| **Dynamic Cheatsheet** (2504.07952) | Test-time learning with adaptive memory. Identified **context collapse** failure mode. DC-RS (retrieve-before-reflect) > DC-Cu (accumulate).                               |
| **ACE** (2510.04618)                | Agentic Context Engineering (ICLR 2026). Incremental delta updates prevent context collapse. Helpful/harmful counters for outcome tracking. Comprehensive playbook thesis. |
| **AWM** (2409.07429)                | Agent Workflow Memory. Abstract parameterized workflows > concrete examples. Hierarchical composition (snowball effect). Success-gated induction.                          |
| **Reflexion** (2303.11366)          | Verbal RL (NeurIPS 2023). Self-reflection as episodic memory. Explicitly calls for structured memory (SQL/vector DBs) as future work — validates memory-engine.            |
| **GEPA** (2507.19457)               | Genetic-Pareto prompt evolution (ICLR 2026 Oral). NL traces > scalar rewards. Pareto diversity prevents local optima in promotion selection.                               |
| **APC** (2506.14852)                | Agentic Plan Caching (NeurIPS 2025). Plan templates (abstracted, context-stripped). Keyword > embedding for intent matching. Cold-start pre-warming.                       |

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

## Notes 23-28 Gap Analysis Integration

The following additions (Phases 5a/5b/6/Deferred) stem from the gap analysis conducted in `~/dev/autonomous-agent-project/docs/summaries/gap-analysis-notes-23-25.md` (notes 23-28). Five new architectural requirements identified:

1. **Multi-agent identity** — `agent_id` on events and facts for per-agent audit trails and scope isolation.
2. **Evidence grounding** — `EvidenceBasis` enum to prevent frequency-based strengthening of ungrounded claims.
3. **Metacognitive transparency** — `importance_rationale` field so DreamCycle promotion decisions are auditable.
4. **Adversarial self-review** — "Wait a minute" gate in DreamCycle to counter sycophancy-driven promotion (Cheng et al.).
5. **Closed-loop behavioral feedback** — Usage outcomes feed back into retrieval ranking weights, closing the observe-retrieve loop.

### Notes 29-30 Integration (2026-04-04)

Two new Phase 5 issues (#212, #213) and two existing-issue updates (#49, #133) from notes 29 (Claude Code reverse-engineering) and 30 (RAG/memory landscape scan). See `~/dev/autonomous-agent-project/docs/summaries/steal-list-notes-29-30.md`.

1. **Structural invariant preservation** (#212) — post-trim validation ensuring edge pairs, session co-occurrence, and provenance chains survive context trimming together. Source: Claude Code `adjustIndexToPreserveAPIInvariants()`.
2. **Expose recency/decay signal for KB** (#213) — public API returning decay-weighted importance scores consumable by KB retrieval ranking. Bridges ME Ebbinghaus decay with KB hybrid search.
3. **Hindsight spreading activation parameters** (#133 update) — concrete reference defaults from BEAM SOTA system: 0.7× decay/hop, 2.0× causal boost, 100-node budget cap, 15-40ms.
4. **PromptBreeder Lamarckian operator** (#49 update) — DreamCycle promotion mechanism: extract behavioral patterns from successful reasoning traces (Intelligence → Wisdom consolidation primitive).

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

### Phase 4: Operability & MCP Server ✅

**Design:** [`docs/design/plans/2026-03-09-future-phases-design.md`](design/plans/2026-03-09-future-phases-design.md)

#### Prerequisites (gate Phase 4 — completed in parallel)

| Item                                                                                       | Description                                                                                                                                                            |
| ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ Documentation gap ([#35](https://github.com/dutiona/memory-engine/issues/35))           | Updated 13 doc files for Phase 3b features. [PR #60](https://github.com/dutiona/memory-engine/pull/60)                                                                 |
| ✅ Schema evolution discipline ([#18](https://github.com/dutiona/memory-engine/issues/18)) | Storage epoch versioning, WAL-safe backup, event envelope versioning, upcaster registry, migration testing. [PR #61](https://github.com/dutiona/memory-engine/pull/61) |

#### Phase 4a: Introspection & Data (library) ✅

| Feature                                                                                    | Description                                                                                                                                       |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ Inspection APIs ([#39](https://github.com/dutiona/memory-engine/issues/39))             | `explain_fact()`, `fact_history()`, `replay_events()`, `dump_state()`, `statistics()`. [PR #70](https://github.com/dutiona/memory-engine/pull/70) |
| ✅ Import/export ([#40](https://github.com/dutiona/memory-engine/issues/40))               | JSON event log + SQLite backup, gzip/zstd compression. [PR #86](https://github.com/dutiona/memory-engine/pull/86)                                 |
| ✅ Semantic extraction queries ([#41](https://github.com/dutiona/memory-engine/issues/41)) | `MemoryQuery` fluent builder. `execute_query()` on engine + async mirror. [PR #71](https://github.com/dutiona/memory-engine/pull/71)              |
| ✅ Reranker trait ([#42](https://github.com/dutiona/memory-engine/issues/42))              | Cross-encoder reranking on top-K candidates after RRF (+5-15% nDCG@10). [PR #68](https://github.com/dutiona/memory-engine/pull/68)                |
| ✅ Session log bootstrap ([#43](https://github.com/dutiona/memory-engine/issues/43))       | Parse Claude Code JSONL session logs into historical memory facts. [PR #69](https://github.com/dutiona/memory-engine/pull/69)                     |
| ✅ Co-session edges ([#62](https://github.com/dutiona/memory-engine/issues/62))            | Auto-create `co_session` edges between facts sharing a `session_id`. [PR #67](https://github.com/dutiona/memory-engine/pull/67)                   |

#### Phase 4a follow-ups (polish & hardening) — 14/14 resolved ✅

| Issue                                                          | Category  | Description                                                                                                                       |
| -------------------------------------------------------------- | --------- | --------------------------------------------------------------------------------------------------------------------------------- |
| ✅ [#73](https://github.com/dutiona/memory-engine/issues/73)   | refactor  | Scope-aware session lookup in `link_session_facts`. [PR #101](https://github.com/dutiona/memory-engine/pull/101)                  |
| ✅ [#76](https://github.com/dutiona/memory-engine/issues/76)   | perf      | Streaming JSON dump for large databases. [PR #98](https://github.com/dutiona/memory-engine/pull/98)                               |
| ✅ [#77](https://github.com/dutiona/memory-engine/issues/77)   | feat      | Populate `source_event` in `FactProvenance`. [PR #89](https://github.com/dutiona/memory-engine/pull/89)                           |
| ✅ [#78](https://github.com/dutiona/memory-engine/issues/78)   | feat      | Dedicated `surfaced_at` column for due facts. [PR #92](https://github.com/dutiona/memory-engine/pull/92)                          |
| ✅ [#79](https://github.com/dutiona/memory-engine/issues/79)   | refactor  | Drop `RwLock` guards before DB read in `explain_fact`. [PR #94](https://github.com/dutiona/memory-engine/pull/94)                 |
| ✅ [#80](https://github.com/dutiona/memory-engine/issues/80)   | fix       | Allow `VACUUM INTO` from in-memory databases. [PR #88](https://github.com/dutiona/memory-engine/pull/88)                          |
| ✅ [#82](https://github.com/dutiona/memory-engine/issues/82)   | hardening | Harden sequential fallback pairing in bootstrap `filter.rs`. [PR #97](https://github.com/dutiona/memory-engine/pull/97)           |
| ✅ [#83](https://github.com/dutiona/memory-engine/issues/83)   | hardening | Propagate interrupted flag through bootstrap `filter.rs`. [PR #100](https://github.com/dutiona/memory-engine/pull/100)            |
| ✅ [#85](https://github.com/dutiona/memory-engine/issues/85)   | hardening | Reranker output validation — subset/permutation guard. [PR #102](https://github.com/dutiona/memory-engine/pull/102)               |
| ✅ [#93](https://github.com/dutiona/memory-engine/issues/93)   | fix       | Stamp `surfaced_at` for due facts in non-due `resume_context` tiers. [PR #174](https://github.com/dutiona/memory-engine/pull/174) |
| ✅ [#104](https://github.com/dutiona/memory-engine/issues/104) | perf      | Add `LIMIT` to `list_active_facts` query. [PR #173](https://github.com/dutiona/memory-engine/pull/173)                            |
| ✅ [#105](https://github.com/dutiona/memory-engine/issues/105) | docs      | Mark issue #82 as complete in ROADMAP.md                                                                                          |
| ✅ [#106](https://github.com/dutiona/memory-engine/issues/106) | docs      | Fix incorrect `MemoryEngine::open` API usage in GEMINI.md                                                                         |
| ✅ [#144](https://github.com/dutiona/memory-engine/issues/144) | hardening | Reranker output validation — index-based trait redesign. [PR #175](https://github.com/dutiona/memory-engine/pull/175)             |

#### Phase 4b: Tooling (new workspace binaries) ✅

| Feature                                                                              | Description                                                                                                                                                     |
| ------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ Read-only open path ([#103](https://github.com/dutiona/memory-engine/issues/103)) | `MemoryEngine::open_readonly()` — defense-in-depth for CLI/MCP. [PR #145](https://github.com/dutiona/memory-engine/pull/145)                                    |
| ✅ CLI inspector ([#44](https://github.com/dutiona/memory-engine/issues/44))         | `memory-engine-cli` — operator tool (inspect, dump, query, explain, stats, import, export). [PR #99](https://github.com/dutiona/memory-engine/pull/99)          |
| ✅ MCP server ([#45](https://github.com/dutiona/memory-engine/issues/45))            | `memory-engine-mcp` — P0 tools (query, add_fact, ingest, resume_context, explain, stats, resolve). [PR #148](https://github.com/dutiona/memory-engine/pull/148) |

#### Phase 4b follow-ups (MCP completeness) — 5/5 resolved ✅

| Issue                                                          | Priority | Description                                                                                                                |
| -------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------- |
| ✅ [#95](https://github.com/dutiona/memory-engine/issues/95)   | P1       | MCP tools: consolidate, forget, dump_state, pin/unpin. [PR #177](https://github.com/dutiona/memory-engine/pull/177)        |
| ✅ [#150](https://github.com/dutiona/memory-engine/issues/150) | P1       | MCP: batch embedding + batch `add_fact` for `flush_insights`. [PR #178](https://github.com/dutiona/memory-engine/pull/178) |
| ✅ [#96](https://github.com/dutiona/memory-engine/issues/96)   | P2       | MCP tools: replay_events, fact_history, bootstrap. [PR #179](https://github.com/dutiona/memory-engine/pull/179)            |
| ✅ [#151](https://github.com/dutiona/memory-engine/issues/151) | P2       | MCP: integration tests for tool handlers. [PR #176](https://github.com/dutiona/memory-engine/pull/176)                     |
| ✅ [#152](https://github.com/dutiona/memory-engine/issues/152) | P1       | Abstention type exposure in Query results (4-type taxonomy). [PR #180](https://github.com/dutiona/memory-engine/pull/180)  |

#### Phase 4b follow-ups (CLI enhancements) — 3/3 resolved ✅

| Issue                                                          | Priority | Description                                                                                                                                                     |
| -------------------------------------------------------------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ [#214](https://github.com/dutiona/memory-engine/issues/214) | P1       | CLI `add-fact` command: create facts with pre-computed embeddings + temporal metadata. [PR #218](https://github.com/dutiona/memory-engine/pull/218)             |
| ✅ [#216](https://github.com/dutiona/memory-engine/issues/216) | P1       | CLI query: `--valid-at` temporal filtering + temporal columns in output. [PR #217](https://github.com/dutiona/memory-engine/pull/217)                           |
| ✅ [#215](https://github.com/dutiona/memory-engine/issues/215) | P1       | CLI `batch-ingest`: bulk JSONL fact loading via embedding API. Shared `memory-engine-embed` crate. [PR #219](https://github.com/dutiona/memory-engine/pull/219) |

#### Phase 4b follow-ups (hook integration — #221 umbrella)

[#221](https://github.com/dutiona/memory-engine/issues/221) is an umbrella spanning Phases 4b/5/6. Closes when all three sub-issues are done.

| Issue                                                          | Priority | Phase | Description                                                                                                                                                 |
| -------------------------------------------------------------- | -------- | ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ [#224](https://github.com/dutiona/memory-engine/issues/224) | P0       | 4b    | Activity stream + session lifecycle (`record_activity`, `checkpoint_session`, `load_context`). [PR #230](https://github.com/dutiona/memory-engine/pull/230) |
| 🔲 [#225](https://github.com/dutiona/memory-engine/issues/225) | P1       | 5a    | Cognitive pipeline MCP endpoints (`dream_cycle`, `get_recent_insights`). Prereqs: #48 ✅, #49 (trait ✅)                                                    |
| 🔲 [#226](https://github.com/dutiona/memory-engine/issues/226) | P1       | 6     | Cross-layer linking MCP endpoints (`link`, `query_linked`). Prereqs: #50                                                                                    |

#### Phase 4c: Quality & Cold Storage ✅

| Feature                                                                             | Description                                                                                                                                                                                                                |
| ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ Evaluation harness ([#16](https://github.com/dutiona/memory-engine/issues/16))   | 52-test 2-tier harness (conformance + quality gates), Criterion lifecycle benchmarks. [PR #198](https://github.com/dutiona/memory-engine/pull/198). Phase 5 skeletons for R4 (context collapse) and R5 (outcome retrieval) |
| ✅ Archival compression ([#46](https://github.com/dutiona/memory-engine/issues/46)) | Cold storage `.pak` files for old non-pinned facts (zstd, explicit trigger, slow fallback). [PR #196](https://github.com/dutiona/memory-engine/pull/196)                                                                   |
| ✅ Fast cold-start ([#31](https://github.com/dutiona/memory-engine/issues/31))      | Sidecar snapshot (rmp-serde named MessagePack + blake3 checksum), composite DB fingerprint, atomic write, graceful fallback to full rebuild. [PR #195](https://github.com/dutiona/memory-engine/pull/195)                  |

#### Phase 4c follow-ups (snapshot improvements) — 0/5 resolved

Non-blocking incremental improvements to #31.

| Issue                                                          | Category | Description                                                             |
| -------------------------------------------------------------- | -------- | ----------------------------------------------------------------------- |
| 🔲 [#199](https://github.com/dutiona/memory-engine/issues/199) | perf     | Avoid DB re-scan in `HnswStrategy::to_snapshot` on shutdown             |
| 🔲 [#200](https://github.com/dutiona/memory-engine/issues/200) | perf     | Explore direct HNSW serde (`serde1` feature) to skip O(N log N) rebuild |
| 🔲 [#201](https://github.com/dutiona/memory-engine/issues/201) | perf     | Add zstd compression to snapshot sidecar file                           |
| 🔲 [#204](https://github.com/dutiona/memory-engine/issues/204) | bench    | Cold-start benchmark suite (10K–500K facts) per #31 requirements        |
| 🔲 [#205](https://github.com/dutiona/memory-engine/issues/205) | feat     | Snapshot GC — keep last 2 sidecar files for rollback safety             |

---

### Phase 5: Cognitive Pipelines 🔲

**Theme:** Close the Memory → Wisdom gap identified by the [four-layer cognitive architecture](https://github.com/dutiona/research-index/blob/master/docs/insights/four-layer-cognitive-architecture.md). Make the engine self-improving.

**Design:** Community research synthesis (5 projects) + three-way debate (Claude/Codex/Gemini, 2 rounds, 7 questions) + context adaptation survey (6 papers, 2026-03-19). See `docs/design/debate-phase5/synthesis.md`, `docs/design/2026-03-12-community-research-synthesis.md`, and `~/dev/autonomous-agent-project/docs/summaries/05-context-adaptation-research.md`.

#### Phase 5a: Minimum Viable Cognitive Pipeline

**Goal:** Trait contracts, provenance, outcome tracking, and the infrastructure for DreamCycle to run end-to-end.

**Completed (7/18):**

| Issue                                                                | Description                                                                                                         | PR                                                           |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| ✅ [#48](https://github.com/dutiona/memory-engine/issues/48)         | InsightStream trait — `record()` fast-path capture. Kept as separate trait (not `FactType::Insight`)                | [PR #228](https://github.com/dutiona/memory-engine/pull/228) |
| ✅ [#54](https://github.com/dutiona/memory-engine/issues/54)         | `sample_dormant()` API — resonance, surfaces low-importance facts semantically related to context embedding         | [PR #228](https://github.com/dutiona/memory-engine/pull/228) |
| ✅ [#55](https://github.com/dutiona/memory-engine/issues/55)         | `PromotionProvenance` + `LineageTable` — sidecar provenance in SQLite, source fact expiry with lineage preservation | [PR #223](https://github.com/dutiona/memory-engine/pull/223) |
| ✅ [#56](https://github.com/dutiona/memory-engine/issues/56)         | `DreamCycleConfig` — per-`FactType` compression ratios + promotion percentile, validated at construction            | [PR #228](https://github.com/dutiona/memory-engine/pull/228) |
| ✅ [#63](https://github.com/dutiona/memory-engine/issues/63)         | Outcome tracking — `EventType::OutcomeSignal`, `record_outcome(fact_id, outcome)` API. Feeds DreamCycle rescoring   | [PR #222](https://github.com/dutiona/memory-engine/pull/222) |
| ✅ [#47](https://github.com/dutiona/memory-engine/issues/47)         | Wisdom promotion API — absorbed into #49 (DreamCycle trait contract)                                                | [PR #228](https://github.com/dutiona/memory-engine/pull/228) |
| ✅ partial [#49](https://github.com/dutiona/memory-engine/issues/49) | DreamCycle **trait contract** + `DreamContext` + `CycleReport` delta output. Default DBSCAN impl still pending      | [PR #228](https://github.com/dutiona/memory-engine/pull/228) |

**Remaining (11 open):**

| Issue                                                          | Category | Description                                                                                                           | Prereqs                                 |
| -------------------------------------------------------------- | -------- | --------------------------------------------------------------------------------------------------------------------- | --------------------------------------- |
| 🔲 [#49](https://github.com/dutiona/memory-engine/issues/49)   | feat     | DreamCycle **default DBSCAN implementation** — batch retroactive consolidation with review gates                      | #55 ✅, #63 ✅ (trait contract shipped) |
| 🔲 [#57](https://github.com/dutiona/memory-engine/issues/57)   | feat     | Three-layer identity output — ANCHORS/CORE/PREDICTIONS in `CycleReport`. Each: `{pattern, directive, false_positive}` | #49 trait ✅                            |
| 🔲 [#132](https://github.com/dutiona/memory-engine/issues/132) | feat     | `FactType::Prediction` with `t_predicted` — predictive memory (JEPA gap)                                              | None                                    |
| 🔲 [#133](https://github.com/dutiona/memory-engine/issues/133) | feat     | Spreading activation on retrieval — return clusters not isolated facts (RMH). Structure-first retrieval               | None                                    |
| 🔲 [#158](https://github.com/dutiona/memory-engine/issues/158) | feat     | `agent_id` on Event and Fact schemas — schema migration, prerequisite for all multi-agent work                        | None                                    |
| 🔲 [#159](https://github.com/dutiona/memory-engine/issues/159) | feat     | `EvidenceBasis { Observed, Inferred, Synthesized }` enum on Fact — prevents frequency-based strengthening             | Pairs with #55 ✅                       |
| 🔲 [#160](https://github.com/dutiona/memory-engine/issues/160) | feat     | `importance_rationale: Option<String>` on Fact — metacognitive transparency for DreamCycle promotion                  | None                                    |
| 🔲 [#161](https://github.com/dutiona/memory-engine/issues/161) | feat     | Adversarial self-review step in DreamCycle promotion gate — "Wait a minute" pattern (Cheng et al.)                    | #49 trait ✅                            |
| 🔲 [#206](https://github.com/dutiona/memory-engine/issues/206) | feat     | High-water mark cursor on event log — idempotent reprocessing, cursor survives compaction                             | None                                    |
| 🔲 [#207](https://github.com/dutiona/memory-engine/issues/207) | feat     | Distributed lock for DreamCycle — mtime+PID lock, 1h staleness, rollback on failure                                   | None                                    |
| 🔲 [#208](https://github.com/dutiona/memory-engine/issues/208) | feat     | Circuit breaker for DreamCycle failures — 3× consecutive → stop. Prevents runaway API waste                           | None                                    |
| 🔲 [#209](https://github.com/dutiona/memory-engine/issues/209) | feat     | Skip DreamCycle if caller already wrote facts — mutual exclusion prevents redundant runs                              | None                                    |
| 🔲 [#225](https://github.com/dutiona/memory-engine/issues/225) | feat     | Cognitive pipeline MCP endpoints — `dream_cycle` + `get_recent_insights`. Part of #221 umbrella                       | #48 ✅, #49 (default impl)              |

**Phase 5a exit criteria:** #49 default impl ships, #57 identity output works, #225 MCP endpoints let consumers invoke the pipeline.

#### Phase 5b: Behavioral Intelligence

**Goal:** Targeted scanning, closed-loop feedback, and lightweight maintenance between DreamCycles.

| Issue                                                          | Category | Description                                                                                                      | Prereqs          |
| -------------------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------- | ---------------- |
| 🔲 [#64](https://github.com/dutiona/memory-engine/issues/64)   | feat     | Grow-and-refine semantic dedup — lightweight maintenance between DreamCycles (R12)                               | #49 default impl |
| 🔲 [#138](https://github.com/dutiona/memory-engine/issues/138) | feat     | Recursive sub-query decomposition for multi-hop retrieval (RMH constraint 2). Consumer-provided decomposer trait | None             |
| 🔲 [#154](https://github.com/dutiona/memory-engine/issues/154) | feat     | Retrieval-induced forgetting — stability penalty for competitor memories                                         | None             |
| 🔲 [#162](https://github.com/dutiona/memory-engine/issues/162) | feat     | Behavioral feedback loop — usage outcomes feed retrieval weights. Closes the observe-retrieve loop               | #63 ✅, #134     |
| 🔲 [#163](https://github.com/dutiona/memory-engine/issues/163) | feat     | Shadow/dry-run mode — full pipeline execution without committing, returns what would change (Signet pattern)     | #49 default impl |

#### Phase 5 — Design issues (context management)

These are design specifications for the integration layer between memory-engine and consumer agents.

| Issue                                                          | Category | Description                                                                                                       | Prereqs |
| -------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------- | ------- |
| 🔲 [#210](https://github.com/dutiona/memory-engine/issues/210) | design   | `ContextDecayPolicy` trait — importance-weighted tool-result pruning for integration layer                        | None    |
| 🔲 [#211](https://github.com/dutiona/memory-engine/issues/211) | design   | Dual-minimum context retention policy — tokens + fact count + hard cap                                            | None    |
| 🔲 [#212](https://github.com/dutiona/memory-engine/issues/212) | design   | Structural invariant preservation — post-trim validation for edge pairs, session co-occurrence, provenance chains | None    |

#### Phase 5 — Independent (parallelizable, any order)

No phase gate depends on these. Can be done alongside 5a/5b or after.

| Issue                                                          | Category | Description                                                                                   | Prereqs                   |
| -------------------------------------------------------------- | -------- | --------------------------------------------------------------------------------------------- | ------------------------- |
| 🔲 [#134](https://github.com/dutiona/memory-engine/issues/134) | feat     | Vitality boosts on access — importance boost with distance decay to graph neighbors (RMH)     | None                      |
| 🔲 [#153](https://github.com/dutiona/memory-engine/issues/153) | feat     | Graph-walk pruning — BFS from seed facts through relationship edges. Complements #133         | None (benefits from #133) |
| 🔲 [#155](https://github.com/dutiona/memory-engine/issues/155) | feat     | Reasoning-strategy-aware reranking — extend `Reranker` trait with task-type context           | None                      |
| 🔲 [#156](https://github.com/dutiona/memory-engine/issues/156) | feat     | Decay-as-deliberate-abstention — decayed facts as "I used to know this" (4th abstention type) | None                      |
| 🔲 [#157](https://github.com/dutiona/memory-engine/issues/157) | research | Mimir 5-signal retrieval weight allocation for episodic memory                                | None                      |
| 🔲 [#213](https://github.com/dutiona/memory-engine/issues/213) | feat     | Expose recency/decay signal for KB — public API returning decay-weighted importance scores    | None                      |

---

### April 2026 Landscape Gaps (notes 31–32)

Sourced from `~/dev/autonomous-agent-project/raw/docs/summaries/04-results-and-roadmap.md` §11.1 (gap statements) and `02-system-design.md` §11.1–11.4 (design refinements). Issue numbers added once filed.

**Verified non-gap (no work needed).** Prospective memory was incorrectly listed as a gap in an earlier pass. It is shipped — see [§ Prospective Memory in `architecture-overview.md`](design/architecture-overview.md#prospective-memory) and [`src/engine/scheduling.rs`](../src/engine/scheduling.rs). The §11.1 retraction in `02-system-design.md` documents the verification.

**P0 (ADR-drafted, implementation pending review).**

- 🔲 [#232](https://github.com/dutiona/memory-engine/issues/232) **ME-P0-A — Wisdom Revision Gate DSL.** Typed Rust DSL for declarative promotion predicates, borrowing Papr's schema-policy vocabulary. `Auto(prompt)` becomes a `ConsumerTraitCallback` so the LLM-free engine invariant is preserved. ADR: [`adr/0010-wisdom-revision-gate-dsl.md`](design/adr/0010-wisdom-revision-gate-dsl.md).
- 🔲 [#233](https://github.com/dutiona/memory-engine/issues/233) **ME-P0-B — Allen Interval Algebra module.** New `src/temporal/allen.rs` exposing the 13 relations over bi-temporal intervals. Unlocks overlap, gap, and cycle detection. Deterministic, constant-time per pair. ADR: [`adr/0011-allen-interval-algebra.md`](design/adr/0011-allen-interval-algebra.md).
- ✅ [#234](https://github.com/dutiona/memory-engine/issues/234) **ME-P0-C — Document prospective memory as a first-class capability.** Added §Prospective Memory to [`architecture-overview.md`](design/architecture-overview.md#prospective-memory) covering time-based and scope-based firing, the scheduled→fired→re-read lifecycle, the polling-model justification, and the McDaniel & Einstein 2007 + DeepMind §7.5.4 mapping. Docs-only — code already shipped.

**P1 (issue-only — ADRs deferred until implementation phase).**

- 🔲 [#235](https://github.com/dutiona/memory-engine/issues/235) **ME-P1-D — Ataraxy-Labs `sem` as a code-fact supersession backend.** Add a `code_fact` subtype tag and a new `CodeEntityResolver` consumer trait. Engine stays language-agnostic; `sem` is consumer-provided. Proposed ADR: `adr/0012-code-fact-supersession-backend.md` (future).
- 🔲 [#236](https://github.com/dutiona/memory-engine/issues/236) **ME-P1-E — Event-based predicate DSL for prospective memory.** Extend prospective memory to fire on an ingest matching predicate P, not only on the clock. Builds on ADR-0011 (Allen) + `scope_id` / `entity_id` matching. Semantic predicates remain a consumer concern by design. Prereq: #233. Proposed ADR: `adr/0013-event-based-prospective-memory.md` (future).

**P2 (tracked, no issue yet).**

- **ME-P2-F — Cognitive-science citations.** Cite Tulving 1972, Cohen & Squire 1980, Bjork 1989, Nelson 1990, and McDaniel & Einstein 2007 in `docs/design/` narrative as the cognitive-science foundations for design decisions. Cited by Burnell et al. 2026.
- **ME-P2-G — `Auto()` callout mechanism for consumer traits.** Narrow `WisdomAutoEvaluator` consumer trait that lets ADR-0010 policies invoke the consumer's LLM at constraint-firing time. Sequenced after the deterministic-only core of ADR-0010 ships.
- **ME-P2-H — Community-voice phrasings in design docs.** Update `docs/design/` (or a new `philosophy.md`) to cite the u/JonnyJF epigraph and adopt "projections never silently become the truth they summarize" (r/Rag 1sgvvig OP, April 2026) as the Wisdom revision-gate invariant.

**P3 (track only — competitive landscape).**

- **ME-P3-I — Track Frona** (`github.com/fronalabs/frona`). First Rust-language peer for memory-engine's stack (Axum + embedded SurrealDB + RocksDB, two-tier user/agent facts). Lacks bi-temporal substrate and consolidation-to-wisdom, but validates the Rust choice. Track development and cite in paper #3 §Related Work.

---

### Phase 6: Knowledge Integration 🔲

**Design:** [`docs/design/plans/2026-03-09-future-phases-design.md`](design/plans/2026-03-09-future-phases-design.md)

**Gate:** Phase 5a minimum (DreamCycle default impl #49, cognitive MCP #225) should be complete before starting core Phase 6 work. #158 (`agent_id`) is a prerequisite for #166 (ACL).

| Issue                                                          | Category | Description                                                                                                               | Prereqs       |
| -------------------------------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------- | ------------- |
| 🔲 [#50](https://github.com/dutiona/memory-engine/issues/50)   | feat     | `KnowledgeBaseConnector` trait + `KnowledgeRef` + graceful degradation. Bidirectional linking                             | Phase 5a gate |
| 🔲 [#51](https://github.com/dutiona/memory-engine/issues/51)   | feat     | Knowledge change notification — KB updates trigger memory re-evaluation. Expand: pub/sub for ME→agent direction           | #50           |
| 🔲 [#52](https://github.com/dutiona/memory-engine/issues/52)   | feat     | `memory-kb-research-index` bridge crate implementing `KnowledgeBaseConnector`                                             | #50           |
| 🔲 [#164](https://github.com/dutiona/memory-engine/issues/164) | feat     | Pub/sub event emission on fact append — `FactWritten`/`FactExpired`/`FactSuperseded`. Includes DLQ design                 | #50           |
| 🔲 [#165](https://github.com/dutiona/memory-engine/issues/165) | feat     | Fact notification schema design — Schema Registry enforcement                                                             | #164          |
| 🔲 [#166](https://github.com/dutiona/memory-engine/issues/166) | feat     | MCP server ACL layer — capability-token auth, agent identity verification                                                 | #158          |
| 🔲 [#167](https://github.com/dutiona/memory-engine/issues/167) | research | Injection dosing framework — principled injection volume calibration (IBM: +28.5pp hard, -5.6pp over-injection)           | None          |
| 🔲 [#168](https://github.com/dutiona/memory-engine/issues/168) | research | Bayesian reputation as consumer trait — `trust_prior: Beta(alpha, beta)` per source pair                                  | None          |
| 🔲 [#226](https://github.com/dutiona/memory-engine/issues/226) | feat     | Cross-layer linking MCP endpoints (`link`, `query_linked`). Part of #221 umbrella                                         | #50           |
|                                                                | feat     | Cross-layer session propagation — propagate KB ingestion `session_id` (dutiona/knowledge-base#128) → #62 co-session edges | #50, #52      |

---

### Phase 7: Visualization 🔲

| Issue                                                        | Description                                                                                                                                                                          |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 🔲 [#13](https://github.com/dutiona/memory-engine/issues/13) | Web UI — WASM+Rust graph visualization. Hybrid: petgraph+fdg-sim (WASM) for layout, sigma.js (WebGL) for rendering. Scope filtering, fact editing, import/export, event log timeline |

---

### Deferred (not planned, trigger-based)

Tracked as individual GitHub issues. Not scheduled for any phase — each has a trigger condition.

| Issue                                                          | Trigger                                                                                                                                               |
| -------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| 🔲 [#14](https://github.com/dutiona/memory-engine/issues/14)   | **Auth** — multi-agent safety, not just multi-user deployment. Deployment layer concern, not engine core                                              |
| 🔲 [#15](https://github.com/dutiona/memory-engine/issues/15)   | **SaaS Sync** — product decision for multi-device. CRDT event-log merge + E2EE. Requires #19 (determinism) first                                      |
| 🔲 [#17](https://github.com/dutiona/memory-engine/issues/17)   | **Hierarchical summarization** — usage exceeds flat consolidation. Memento-style multi-level abstractions                                             |
| 🔲 [#19](https://github.com/dutiona/memory-engine/issues/19)   | **Determinism guarantees** — before sync work begins. Replay, merge, idempotency rules                                                                |
| 🔲 [#36](https://github.com/dutiona/memory-engine/issues/36)   | **Cross-session memory sharing** — multi-agent deployments. Session isolation or namespace via ScopeTree                                              |
| 🔲 [#37](https://github.com/dutiona/memory-engine/issues/37)   | **Multimodal memory** — non-text memories needed. Schema supports it (BLOB embeddings)                                                                |
| 🔲 [#38](https://github.com/dutiona/memory-engine/issues/38)   | **Multi-node sync** — multi-device eventual consistency. Event envelope fields are forward-compatible                                                 |
| 🔲 [#65](https://github.com/dutiona/memory-engine/issues/65)   | **GEPA meta-optimization** — evolutionary optimization of DreamCycle prompts. Trigger: >100 DreamCycle runs. Research: GEPA (arXiv:2507.19457)        |
| 🔲 [#66](https://github.com/dutiona/memory-engine/issues/66)   | **Keyword-weighted hybrid search** — weight FTS5 higher for `FactType::Procedural`. Trigger: after #42 ✅ (shipped). Research: APC (arXiv:2506.14852) |
| 🔲 [#135](https://github.com/dutiona/memory-engine/issues/135) | **Energy-based forgetting** — unify temporal + representational + structural saliency. Trigger: Phase 5a ships, empirical data on forgetting gaps     |
| 🔲 [#136](https://github.com/dutiona/memory-engine/issues/136) | **State-delta updates** — incremental deltas for high-frequency ops (JEPA). Trigger: performance profiling shows bottleneck                           |
| 🔲 [#137](https://github.com/dutiona/memory-engine/issues/137) | **Attention-based retrieval** — attention mechanism over memory store (JEPA). Trigger: Phase 5a eval shows retrieval quality gap                      |
| 🔲 [#139](https://github.com/dutiona/memory-engine/issues/139) | **Optimal decay-zone boundaries** — derive multipliers statistically. Trigger: sufficient forgetting telemetry                                        |
| 🔲 [#140](https://github.com/dutiona/memory-engine/issues/140) | **ANCHOR promotion threshold** — derive statistically rather than heuristic. Trigger: sufficient DreamCycle data (Q12)                                |
| 🔲 [#169](https://github.com/dutiona/memory-engine/issues/169) | **Context-triggered injection (Level 2)** — reasoning context against K/M between thinking blocks. Trigger: Phase 5a + KB Phase 2 complete            |
| 🔲 [#170](https://github.com/dutiona/memory-engine/issues/170) | **Byzantine fault tolerance for DreamCycle** — design needed before multi-agent DreamCycle. Trigger: multi-agent deployment decision                  |
| 🔲 [#171](https://github.com/dutiona/memory-engine/issues/171) | **Latency-aware memory materialization** — FAISS/in-memory cache tier. Trigger: latency profiling shows retrieval bottleneck                          |
| 🔲 [#172](https://github.com/dutiona/memory-engine/issues/172) | **Protocol conformance test suite** — LAM-style fixtures. Trigger: external consumers adopt the engine                                                |

---

### Code Quality Sweep (super-qa — parallel track)

Discovered via automated super-qa audit ([PR #131](https://github.com/dutiona/memory-engine/pull/131) auto-fixed 10 findings). These are non-blocking and can be addressed incrementally alongside any phase work.

**High severity — 6/6 resolved ✅**

| Issue                                                          | Category    | Description                                                                                                        |
| -------------------------------------------------------------- | ----------- | ------------------------------------------------------------------------------------------------------------------ |
| ✅ [#108](https://github.com/dutiona/memory-engine/issues/108) | refactoring | `engine.rs` god module → partial impl files. [PR #186](https://github.com/dutiona/memory-engine/pull/186)          |
| ✅ [#109](https://github.com/dutiona/memory-engine/issues/109) | refactoring | `add_fact` 8 params → `AddFactRequest` struct. [PR #194](https://github.com/dutiona/memory-engine/pull/194)        |
| ✅ [#110](https://github.com/dutiona/memory-engine/issues/110) | refactoring | All modules `pub` → `pub(crate)` encapsulation. [PR #188](https://github.com/dutiona/memory-engine/pull/188)       |
| ✅ [#111](https://github.com/dutiona/memory-engine/issues/111) | cleanup     | `proptest`/`insta` dev-deps unused → added tests. [PR #189](https://github.com/dutiona/memory-engine/pull/189)     |
| ✅ [#128](https://github.com/dutiona/memory-engine/issues/128) | testing     | `traits.rs` and `query.rs` zero unit tests → covered. [PR #185](https://github.com/dutiona/memory-engine/pull/185) |
| ✅ [#187](https://github.com/dutiona/memory-engine/issues/187) | bug         | Stale `depth_shaping` insta snapshots after #180. [PR #193](https://github.com/dutiona/memory-engine/pull/193)     |

**Medium severity — 0/23 resolved**

| Issue                                                          | Category    | Description                                                                      |
| -------------------------------------------------------------- | ----------- | -------------------------------------------------------------------------------- |
| 🔲 [#112](https://github.com/dutiona/memory-engine/issues/112) | security    | `VACUUM INTO` path interpolation in schema backup                                |
| 🔲 [#113](https://github.com/dutiona/memory-engine/issues/113) | refactoring | 6 constructor variants instead of builder                                        |
| 🔲 [#114](https://github.com/dutiona/memory-engine/issues/114) | refactoring | `Edge.relation_type` is stringly-typed                                           |
| 🔲 [#115](https://github.com/dutiona/memory-engine/issues/115) | refactoring | `MemoryError` variants are stringly-typed catch-alls                             |
| 🔲 [#116](https://github.com/dutiona/memory-engine/issues/116) | refactoring | `SummaryGenerator::embed` duplicates `EmbeddingProvider`                         |
| 🔲 [#117](https://github.com/dutiona/memory-engine/issues/117) | refactoring | Scope resolution duplicated across query, bootstrap, resume paths                |
| 🔲 [#118](https://github.com/dutiona/memory-engine/issues/118) | refactoring | Synthetic `Fact` construction duplicated                                         |
| 🔲 [#119](https://github.com/dutiona/memory-engine/issues/119) | correctness | `unreachable!()` in `infer_search_mode`                                          |
| 🔲 [#120](https://github.com/dutiona/memory-engine/issues/120) | refactoring | Test helpers duplicated across 15+ modules                                       |
| 🔲 [#121](https://github.com/dutiona/memory-engine/issues/121) | testing     | Zero runnable doc-tests in crate                                                 |
| 🔲 [#122](https://github.com/dutiona/memory-engine/issues/122) | docs        | Stale Phase 2 labels in trait docs                                               |
| 🔲 [#123](https://github.com/dutiona/memory-engine/issues/123) | refactoring | Bootstrap functions take 9+ parameters                                           |
| 🔲 [#126](https://github.com/dutiona/memory-engine/issues/126) | refactoring | Glob re-exports in `lib.rs` hide API surface                                     |
| 🔲 [#127](https://github.com/dutiona/memory-engine/issues/127) | docs        | `DumpFormat::Sqlite` doc contradicts implementation                              |
| 🔲 [#129](https://github.com/dutiona/memory-engine/issues/129) | testing     | Missing tests for inspect types, consolidation orchestrator, bootstrap           |
| 🔲 [#130](https://github.com/dutiona/memory-engine/issues/130) | testing     | Untested engine query/restore error paths                                        |
| 🔲 [#141](https://github.com/dutiona/memory-engine/issues/141) | security    | Compressed snapshot can bypass file size limit (decompression bomb)              |
| 🔲 [#142](https://github.com/dutiona/memory-engine/issues/142) | correctness | `usize::MAX` sentinel leaks via public `local_dedup` API                         |
| 🔲 [#149](https://github.com/dutiona/memory-engine/issues/149) | refactoring | Consider builder pattern for `EngineConfig` (related to #113)                    |
| 🔲 [#191](https://github.com/dutiona/memory-engine/issues/191) | bug         | `restore_sqlite` uses `exists()` instead of `is_file()`                          |
| 🔲 [#192](https://github.com/dutiona/memory-engine/issues/192) | refactoring | `should_use_hnsw` uses `map_or(false, ..)` instead of `is_some_and()`            |
| 🔲 [#203](https://github.com/dutiona/memory-engine/issues/203) | bug         | Duplicate `mod archive` declaration in `engine/mod.rs`                           |
| 🔲 [#231](https://github.com/dutiona/memory-engine/issues/231) | testing     | Integration tests for Phase 5a cognitive APIs (sample_dormant, promote, outcome) |

**Low/Info (batched):**

| Issue                                                          | Description              |
| -------------------------------------------------------------- | ------------------------ |
| 🔲 [#124](https://github.com/dutiona/memory-engine/issues/124) | 27 low-severity findings |
| 🔲 [#125](https://github.com/dutiona/memory-engine/issues/125) | 10 info-level findings   |

---

## Critical Path & Shortest Route to Phase 6

### Issue counts by area

| Area                      | Total   | Done   | Open   | On critical path |
| ------------------------- | ------- | ------ | ------ | ---------------- |
| Phase 1-3b                | 36      | 36     | 0      | —                |
| Phase 4 (all sub-phases)  | 42      | 39     | 3†     | —                |
| Phase 5a                  | 18      | 7      | 11     | 5                |
| Phase 5b                  | 5       | 0      | 5      | 0‡               |
| Phase 5 independent       | 6       | 0      | 6      | 0                |
| Phase 5 design            | 3       | 0      | 3      | 0                |
| April 2026 landscape gaps | 5       | 1      | 4      | 0♦               |
| Phase 6                   | 10      | 0      | 10     | 5                |
| Phase 7                   | 1       | 0      | 1      | 0                |
| Deferred                  | 18      | 0      | 18     | 0                |
| Code quality (super-qa)   | 31      | 6      | 25     | 0                |
| Phase 4c follow-ups       | 5       | 0      | 5      | 0                |
| **Total**                 | **180** | **89** | **91** |                  |

† Phase 4 open: #199, #200, #201, #204, #205 (snapshot follow-ups) + #225, #226 (part of #221 umbrella, tracked in Phase 5a/6).
‡ Phase 5b is not on the critical path to Phase 6 — it can run in parallel.
♦ April 2026 landscape gaps run as a parallel Phase 5 track, not strict-order critical path. #232 (Wisdom DSL) feeds #49/#57 identity output; #233 (Allen) is a substrate for #232 and #236. #234 (prospective-memory docs) closed by `3d3252b`. Elevate any of #232/#233 onto the strict path at user discretion during review.

### Critical path diagram

The **minimum chain** to unblock Phase 6 core, then complete it:

```text
                     NOW (2026-04-13)
                      │
     ┌────────────────┼──────────────────────┬───────────────┐
     │                │                      │               │
     ▼                ▼                      ▼               ▼
  Phase 5a         Phase 5          April 2026 gaps    Code Quality Sweep
  CRITICAL PATH    independent      (4 open, parallel)  (24 open, parallel)
     │             (6 issues,       #232 #233 #235 #236
     │              any order)      — user-review gated
     │
     ├─── #49 default impl ◄── NEXT (prereqs met: #55✅ #63✅)
     │         │
     │         ├─── #57 (three-layer identity, needs #49)
     │         ├─── #161 (adversarial review, needs #49)
     │         ├─── #225 (cognitive MCP, needs #49)
     │         │
     │    #158 (agent_id schema) ◄── PARALLEL with #49
     │         │
     │         └─── #166 (MCP ACL, needs #158) ── Phase 6
     │
     │    #132, #133, #159, #160, #206-#209 ◄── PARALLEL with #49
     │
     ▼ Phase 5a gate met (#49 impl + #225 shipped)
     │
     ├─── Phase 5b (5 issues, parallel with Phase 6)
     │
     ▼ Phase 6 START
     │
     ├─── #50 (KnowledgeBaseConnector) ◄── GATE
     │         │
     │         ├─── #51 (KB change notification)
     │         ├─── #52 (research-index bridge)
     │         ├─── #164 (pub/sub event emission)
     │         │         │
     │         │         └─── #165 (notification schema)
     │         │
     │         └─── #226 (cross-layer linking MCP)
     │
     ├─── #166 (MCP ACL) ◄── needs #158 from 5a
     ├─── #167 (injection dosing research) ◄── independent
     ├─── #168 (Bayesian reputation research) ◄── independent
     │
     ▼ Phase 6 COMPLETE
     │
     └─── #221 umbrella closes (when #224✅ + #225 + #226 all done)
```

### Shortest path (12 issues, strict sequence)

These are the minimum issues that must ship, in order, to complete Phase 6:

| Step | Issue | Description                       | Blocked by       |
| ---- | ----- | --------------------------------- | ---------------- |
| 1    | #49   | DreamCycle default DBSCAN impl    | Nothing (go)     |
| 2    | #158  | `agent_id` schema migration       | Nothing (‖ #49)  |
| 3    | #57   | Three-layer identity output       | #49              |
| 4    | #225  | Cognitive MCP endpoints           | #49              |
| 5    | #50   | `KnowledgeBaseConnector` trait    | #225 (5a gate)   |
| 6    | #51   | KB change notification            | #50              |
| 7    | #52   | Research-index bridge crate       | #50              |
| 8    | #164  | Pub/sub event emission            | #50              |
| 9    | #165  | Notification schema design        | #164             |
| 10   | #166  | MCP ACL layer                     | #158             |
| 11   | #226  | Cross-layer linking MCP endpoints | #50              |
| 12   |       | Close #221 umbrella               | #224✅ #225 #226 |

**Parallelism within the shortest path:**

- Steps 1-2 are independent → run in parallel
- Steps 3-4 both depend only on #49 → run in parallel after step 1
- Steps 6, 7, 8, 11 all depend only on #50 → run in parallel after step 5
- Step 9 depends on step 8
- Step 10 depends on step 2 (can run as soon as #158 ships, parallel with Phase 6 core)

**Issues NOT on the shortest path** but valuable before shipping Phase 6:

- #159, #160, #161 (schema enrichments + adversarial review) — improve DreamCycle quality
- #132, #133 (prediction facts + spreading activation) — improve retrieval quality
- #206-#209 (DreamCycle operational hardening) — improve DreamCycle reliability
- #167, #168 (research issues) — inform future design, not blocking

### Three parallel tracks (what to work on now)

1. **Critical path** → #49 (DreamCycle DBSCAN impl) + #158 (agent_id schema) — these unblock everything downstream
2. **Phase 5 independent** → #134, #153, #155, #156, #213 — retrieval and forgetting improvements, no dependencies
3. **Code quality** → 25 super-qa issues + 5 snapshot follow-ups — incremental, any order

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
│  explain · replay · dump · statistics   │  ← Phase 4a ✅
│  import · export · link_session_facts  │  ← Phase 4a ✅
│  dream_cycle · sample_dormant           │  ← Phase 5 (traits ✅, default impl 🔲)
│  record_outcome · dedup_semantic        │  ← Phase 5 (#63 ✅, #64 🔲)
├──────────────────────────────────────────┤
│  AsyncMemoryEngine (tokio wrapper)       │  ← Phase 3 ✅
├──────────────────────────────────────────┤
│  ConnectionPool (N readers + 1 writer)   │  ← Phase 3 ✅
├──────────────────────────────────────────┤
│  Search                                  │
│  ├─ FTS5 (BM25, scope-filtered)          │
│  ├─ Vector (cosine / HNSW ANN)           │
│  ├─ Hybrid (RRF k=60)                   │
│  └─ Reranker (cross-encoder, optional)  │  ← Phase 4a ✅
├──────────────────────────────────────────┤
│  Store                                   │
│  ├─ EventStore (append-only, upcasting)  │
│  ├─ FactStore (bi-temporal + pinned)     │
│  ├─ EdgeStore (graph persistence)        │
│  ├─ SummaryStore                         │
│  ├─ ScopeStore (hierarchical scoping)   │  ← Phase 3 ✅
│  └─ LineageTable (provenance sidecar)   │  ← Phase 5a ✅
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
│  Cognitive Pipelines                     │  ← Phase 5 (traits ✅)
│  ├─ InsightStream (fast-path capture)   │  ← PR #228 ✅
│  └─ DreamCycle (trait ✅, impl 🔲)       │  ← PR #228 / #49
├──────────────────────────────────────────┤
│  Knowledge Bridge                        │  ← Phase 6 🔲
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
Reranker::rerank(query, candidates) → Vec<ScoredFact>  ← Phase 4a ✅
InsightStream::record(insight) → Result<()>             ← Phase 5 ✅
DreamCycle::run(engine) → Result<CycleReport>           ← Phase 5 ✅ (trait)
KnowledgeBaseConnector::resolve(uri) → KnowledgeChunk  ← Phase 6 🔲
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
