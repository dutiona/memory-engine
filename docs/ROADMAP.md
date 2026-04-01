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

### Phase 4: Operability & MCP Server (4a ✅ 4b ✅ 4c 🔲)

**Design:** [`docs/design/plans/2026-03-09-future-phases-design.md`](design/plans/2026-03-09-future-phases-design.md)

#### Prerequisites (gate Phase 4 — can be done in parallel)

| Item                                                                                    | Description                                                                                                                                                               |
| --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Documentation gap ([#35](https://github.com/dutiona/memory-engine/issues/35))           | ✅ Updated 13 doc files for Phase 3b features (pinned facts, 5-tier resume, scheduling API, classifier). [PR #60](https://github.com/dutiona/memory-engine/pull/60)       |
| Schema evolution discipline ([#18](https://github.com/dutiona/memory-engine/issues/18)) | ✅ Storage epoch versioning, WAL-safe backup, event envelope versioning, upcaster registry, migration testing. [PR #61](https://github.com/dutiona/memory-engine/pull/61) |

#### Phase 4a: Introspection & Data (library) ✅

| Feature                                                                                    | Description                                                                                                                                                                                 |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ Inspection APIs ([#39](https://github.com/dutiona/memory-engine/issues/39))             | `explain_fact()`, `fact_history()`, `replay_events()`, `dump_state()`, `statistics()`                                                                                                       |
| ✅ Import/export ([#40](https://github.com/dutiona/memory-engine/issues/40))               | JSON event log + SQLite backup, gzip/zstd compression                                                                                                                                       |
| ✅ Semantic extraction queries ([#41](https://github.com/dutiona/memory-engine/issues/41)) | `MemoryQuery` fluent builder: scope + temporal period + FTS/vector + importance/pinned filters. `execute_query()` on engine + async mirror. `#[non_exhaustive]` `MatchType::ImportanceRank` |
| ✅ `Reranker` trait ([#42](https://github.com/dutiona/memory-engine/issues/42))            | Cross-encoder reranking on top-K candidates after RRF (+5-15% nDCG@10). Consumer-provided                                                                                                   |
| ✅ Session log bootstrap ([#43](https://github.com/dutiona/memory-engine/issues/43))       | Parse Claude Code JSONL session logs into historical memory facts. Success-gated ingestion (AWM), workflow extraction (AWM/APC), pre-warming semantics (APC)                                |
| ✅ Co-session edges ([#62](https://github.com/dutiona/memory-engine/issues/62))            | Auto-create `co_session` edges between facts sharing a `session_id`. [PR #67](https://github.com/dutiona/memory-engine/pull/67). Pairs with #43                                             |

#### Phase 4a Follow-ups (polish & hardening)

Spawned during Phase 4a implementation reviews. All non-blocking for Phase 4b/4c. **9/14 resolved.**

| Issue                                                          | Category  | Description                                                                                                                          |
| -------------------------------------------------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| ✅ [#73](https://github.com/dutiona/memory-engine/issues/73)   | refactor  | Scope-aware session lookup in `link_session_facts`. [PR #101](https://github.com/dutiona/memory-engine/pull/101)                     |
| ✅ [#76](https://github.com/dutiona/memory-engine/issues/76)   | perf      | Streaming JSON dump for large databases. [PR #98](https://github.com/dutiona/memory-engine/pull/98)                                  |
| ✅ [#77](https://github.com/dutiona/memory-engine/issues/77)   | feat      | Populate `source_event` in `FactProvenance`. [PR #89](https://github.com/dutiona/memory-engine/pull/89)                              |
| ✅ [#78](https://github.com/dutiona/memory-engine/issues/78)   | feat      | Dedicated `surfaced_at` column for due facts. [PR #92](https://github.com/dutiona/memory-engine/pull/92)                             |
| ✅ [#79](https://github.com/dutiona/memory-engine/issues/79)   | refactor  | Drop `RwLock` guards before DB read in `explain_fact`. [PR #94](https://github.com/dutiona/memory-engine/pull/94)                    |
| ✅ [#80](https://github.com/dutiona/memory-engine/issues/80)   | fix       | Allow `VACUUM INTO` from in-memory databases. [PR #88](https://github.com/dutiona/memory-engine/pull/88)                             |
| ✅ [#82](https://github.com/dutiona/memory-engine/issues/82)   | hardening | Harden sequential fallback pairing in bootstrap `filter.rs`. [PR #97](https://github.com/dutiona/memory-engine/pull/97)              |
| ✅ [#83](https://github.com/dutiona/memory-engine/issues/83)   | hardening | Propagate interrupted flag through bootstrap `filter.rs`. [PR #100](https://github.com/dutiona/memory-engine/pull/100)               |
| ✅ [#85](https://github.com/dutiona/memory-engine/issues/85)   | hardening | Reranker output validation — subset/permutation guard. [PR #102](https://github.com/dutiona/memory-engine/pull/102)                  |
| ✅ [#93](https://github.com/dutiona/memory-engine/issues/93)   | fix       | Stamp `surfaced_at` for due facts in non-due `resume_context` tiers. [PR #174](https://github.com/dutiona/memory-engine/pull/174)    |
| ✅ [#104](https://github.com/dutiona/memory-engine/issues/104) | perf      | Add `LIMIT` to `list_active_facts` query. [PR #173](https://github.com/dutiona/memory-engine/pull/173)                               |
| ✅ [#105](https://github.com/dutiona/memory-engine/issues/105) | docs      | Mark issue #82 as complete in ROADMAP.md                                                                                             |
| ✅ [#106](https://github.com/dutiona/memory-engine/issues/106) | docs      | Fix incorrect `MemoryEngine::open` API usage in GEMINI.md. [commit bb35f03](https://github.com/dutiona/memory-engine/commit/bb35f03) |
| ✅ [#144](https://github.com/dutiona/memory-engine/issues/144) | hardening | Reranker output validation — index-based trait redesign. [PR #175](https://github.com/dutiona/memory-engine/pull/175)                |

#### Phase 4b: Tooling (new workspace binaries) ✅

| Feature                                                                              | Description                                                                                                                                                     |
| ------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ✅ Read-only open path ([#103](https://github.com/dutiona/memory-engine/issues/103)) | `MemoryEngine::open_readonly()` — defense-in-depth for CLI/MCP. [PR #145](https://github.com/dutiona/memory-engine/pull/145)                                    |
| ✅ CLI inspector ([#44](https://github.com/dutiona/memory-engine/issues/44))         | `memory-engine-cli` — operator tool (inspect, dump, query, explain, stats, import, export). [PR #99](https://github.com/dutiona/memory-engine/pull/99)          |
| ✅ MCP server ([#45](https://github.com/dutiona/memory-engine/issues/45))            | `memory-engine-mcp` — P0 tools (query, add_fact, ingest, resume_context, explain, stats, resolve). [PR #148](https://github.com/dutiona/memory-engine/pull/148) |

#### Phase 4b Follow-ups (MCP completeness)

| Issue                                                          | Priority | Description                                                                                                                |
| -------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------- |
| ✅ [#95](https://github.com/dutiona/memory-engine/issues/95)   | P1       | MCP tools: consolidate, forget, dump_state, pin/unpin. [PR #177](https://github.com/dutiona/memory-engine/pull/177)        |
| ✅ [#150](https://github.com/dutiona/memory-engine/issues/150) | P1       | MCP: batch embedding + batch `add_fact` for `flush_insights`. [PR #178](https://github.com/dutiona/memory-engine/pull/178) |
| ✅ [#96](https://github.com/dutiona/memory-engine/issues/96)   | P2       | MCP tools: replay_events, fact_history, bootstrap. [PR #179](https://github.com/dutiona/memory-engine/pull/179)            |
| ✅ [#151](https://github.com/dutiona/memory-engine/issues/151) | P2       | MCP: integration tests for tool handlers. [PR #176](https://github.com/dutiona/memory-engine/pull/176)                     |
| ✅ [#152](https://github.com/dutiona/memory-engine/issues/152) | P1       | Abstention type exposure in Query results (4-type taxonomy). [PR #180](https://github.com/dutiona/memory-engine/pull/180)  |

#### Phase 4c: Quality & Cold Storage

| Feature                                                                          | Description                                                                                                                                                                                                                  |
| -------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Evaluation harness ([#16](https://github.com/dutiona/memory-engine/issues/16))   | Regression corpus for retrieval quality, consolidation correctness, forgetting behavior. After 4a/4b ship. **Research update:** context collapse detection (R4, DC/ACE), outcome-based retrieval quality (R5, ACE/Reflexion) |
| Archival compression ([#46](https://github.com/dutiona/memory-engine/issues/46)) | Cold storage `.pak` files for old non-pinned facts (zstd, explicit trigger, slow fallback)                                                                                                                                   |
| Fast cold-start ([#31](https://github.com/dutiona/memory-engine/issues/31))      | Snapshot + incremental replay for rapid engine boot                                                                                                                                                                          |

#### Code Quality Sweep (super-qa — parallel track)

Discovered via automated super-qa audit ([PR #131](https://github.com/dutiona/memory-engine/pull/131) auto-fixed 10 findings). Remaining issues are non-blocking and can be addressed incrementally alongside Phase 4b/5 work.

**High severity:**

| Issue                                                       | Category    | Description                                                 |
| ----------------------------------------------------------- | ----------- | ----------------------------------------------------------- |
| [#108](https://github.com/dutiona/memory-engine/issues/108) | refactoring | `engine.rs` is a 3573-line god module — extract subsystems  |
| [#109](https://github.com/dutiona/memory-engine/issues/109) | refactoring | `add_fact` takes 8 parameters — introduce builder or struct |
| [#110](https://github.com/dutiona/memory-engine/issues/110) | refactoring | All modules are `pub` in `lib.rs` — no encapsulation        |
| [#111](https://github.com/dutiona/memory-engine/issues/111) | cleanup     | `proptest` and `insta` dev-dependencies never used          |
| [#128](https://github.com/dutiona/memory-engine/issues/128) | testing     | `traits.rs` and `query.rs` have zero unit tests ✅ (#185)   |

**Medium severity:**

| Issue                                                       | Category    | Description                                                            |
| ----------------------------------------------------------- | ----------- | ---------------------------------------------------------------------- |
| [#112](https://github.com/dutiona/memory-engine/issues/112) | security    | `VACUUM INTO` path interpolation in schema backup                      |
| [#113](https://github.com/dutiona/memory-engine/issues/113) | refactoring | 6 constructor variants instead of builder                              |
| [#114](https://github.com/dutiona/memory-engine/issues/114) | refactoring | `Edge.relation_type` is stringly-typed                                 |
| [#115](https://github.com/dutiona/memory-engine/issues/115) | refactoring | `MemoryError` variants are stringly-typed catch-alls                   |
| [#116](https://github.com/dutiona/memory-engine/issues/116) | refactoring | `SummaryGenerator::embed` duplicates `EmbeddingProvider`               |
| [#117](https://github.com/dutiona/memory-engine/issues/117) | refactoring | Scope resolution duplicated across query paths                         |
| [#118](https://github.com/dutiona/memory-engine/issues/118) | refactoring | Synthetic `Fact` construction duplicated                               |
| [#119](https://github.com/dutiona/memory-engine/issues/119) | correctness | `unreachable!()` in `infer_search_mode`                                |
| [#120](https://github.com/dutiona/memory-engine/issues/120) | refactoring | Test helpers duplicated across 15+ modules                             |
| [#121](https://github.com/dutiona/memory-engine/issues/121) | testing     | Zero runnable doc-tests in crate                                       |
| [#122](https://github.com/dutiona/memory-engine/issues/122) | docs        | Stale Phase 2 labels in trait docs                                     |
| [#123](https://github.com/dutiona/memory-engine/issues/123) | refactoring | Bootstrap functions take 9+ parameters                                 |
| [#126](https://github.com/dutiona/memory-engine/issues/126) | refactoring | Glob re-exports in `lib.rs` hide API surface                           |
| [#127](https://github.com/dutiona/memory-engine/issues/127) | docs        | `DumpFormat::Sqlite` doc contradicts implementation                    |
| [#129](https://github.com/dutiona/memory-engine/issues/129) | testing     | Missing tests for inspect types, consolidation orchestrator, bootstrap |
| [#130](https://github.com/dutiona/memory-engine/issues/130) | testing     | Untested engine query/restore error paths                              |
| [#141](https://github.com/dutiona/memory-engine/issues/141) | security    | Compressed snapshot can bypass file size limit (decompression bomb)    |
| [#142](https://github.com/dutiona/memory-engine/issues/142) | correctness | `usize::MAX` sentinel leaks via public `local_dedup` API               |
| [#149](https://github.com/dutiona/memory-engine/issues/149) | refactoring | Consider builder pattern for `EngineConfig` (related to #113)          |

**Low/Info (batched):**

| Issue                                                       | Description              |
| ----------------------------------------------------------- | ------------------------ |
| [#124](https://github.com/dutiona/memory-engine/issues/124) | 27 low-severity findings |
| [#125](https://github.com/dutiona/memory-engine/issues/125) | 10 info-level findings   |

---

### Execution Order & Critical Path (as of 2026-03-23)

Phase 4a ✅ and 4b ✅ are complete. Phase 5 is **unblocked on the critical path**. The remaining Phase 4 follow-ups, Phase 4c, and the super-qa sweep are all parallel tracks that do not gate Phase 5.

```
                    NOW
                     │
    ┌────────────────┼─────────────────┐
    │                │                 │
    ▼                ▼                 ▼
 Phase 4            Phase 4c         super-qa
 follow-ups         (#16,#46,#31)    (25 issues)
 (#95-#151)         independent      parallel track
 parallel track     of each other
    │                │
    │           ┌────┴────┐
    │           │         │
    │        eval #16   cold-start #31
    │        (needs      (needs
    │         data)       #46 first)
    │
    ▼
 Phase 5a ◀── CRITICAL PATH
 (#48,#49,#55,#56,#57,#63,#132,#133
  + #158,#159,#160,#161)
    │
    ▼
 Phase 5b
 (#64,#138, quarantine/suppress
  + #162,#163)
    │
    ▼
 Phase 6  (#50,#51,#52
  + #164,#165,#166,#167,#168)
    │
    ▼
 Phase 7  (#13)
```

**Parallelizable right now (4 independent tracks):**

<<<<<<< HEAD

1. **Phase 4 follow-ups** (2 open: #150, #152; #95 ✅, #96 ✅, #151 ✅) — all non-blocking
1. **Phase 4c** (#16, #46, #31) — #16 evaluation harness benefits from MCP being live; #31 depends on #46
1. **Super-qa sweep** (25 open issues) — incremental, any order
1. # **Phase 5a design + implementation** — the critical path forward
1. **Phase 4 follow-ups** (all resolved: #95 ✅, #96 ✅, #150 ✅, #151 ✅, #152 ✅)
1. **Phase 4c** (#16, #46, #31) — #16 evaluation harness benefits from MCP being live; #31 depends on #46
1. **Super-qa sweep** (25 open issues) — incremental, any order
1. **Phase 5a design + implementation** — the critical path forward

**Phase 5 internal dependencies:**

- #55 (provenance) + #63 (outcome tracking) are prerequisites for #49 (DreamCycle)
- #48 (InsightStream) is independent, can ship first
- #132 (FactType::Prediction) and #133 (spreading activation) can be parallel with #49
- #54 (sample_dormant) and #134 (vitality boosts) are independent of everything, any time

---

### Phase 5: Cognitive Pipelines 🔲

**Theme:** Close the Memory → Wisdom gap identified by the [four-layer cognitive architecture](https://github.com/dutiona/research-index/blob/master/docs/insights/four-layer-cognitive-architecture.md). Make the engine self-improving.

**Design:** Community research synthesis (5 projects) + three-way debate (Claude/Codex/Gemini, 2 rounds, 7 questions) + context adaptation survey (6 papers, 2026-03-19). See `docs/design/debate-phase5/synthesis.md`, `docs/design/2026-03-12-community-research-synthesis.md`, and `~/dev/autonomous-agent-project/docs/summaries/05-context-adaptation-research.md`.

| Feature                                                                                                                                                               | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| InsightStream trait — fast-path capture ([#48](https://github.com/dutiona/memory-engine/issues/48))                                                                   | `record()` method for high-value observations. Consumer-implemented. **Note:** Gemini dissent — may simplify to `FactType::Insight` via `add_fact()` during implementation. **Known blocker:** context compaction vulnerability — `PreCompaction` hook does not exist                                                                                                                                                                                                        |
| DreamCycle trait — cognitive pipeline ([#49](https://github.com/dutiona/memory-engine/issues/49), [#47](https://github.com/dutiona/memory-engine/issues/47) absorbed) | Full batch pipeline: consolidation → pattern detection → promotion → rescoring. Returns delta-based `CycleReport`. **Research update:** delta-based output (R7, ACE), retrieve-before-reflect (R8, DC), abstract pattern extraction (R9, AWM/APC/GEPA), hierarchical composition (R13, AWM). **Competitive urgency:** Signet, Hermes, Auto Dream implement similar pipelines. Single-agent design — multi-agent DreamCycle requires Byzantine fault tolerance (see Deferred) |
| Outcome tracking ([#63](https://github.com/dutiona/memory-engine/issues/63))                                                                                          | `EventType::OutcomeSignal` for fact feedback loops. `record_outcome(fact_id, outcome)` API. Feeds DreamCycle rescoring. **Research basis:** ACE, Reflexion, AWM, GEPA                                                                                                                                                                                                                                                                                                        |
| `sample_dormant()` API ([#54](https://github.com/dutiona/memory-engine/issues/54))                                                                                    | Passive resonance for autonomous agents. HNSW search filtered for dormant facts. Consumer-driven                                                                                                                                                                                                                                                                                                                                                                             |
| Provenance infrastructure ([#55](https://github.com/dutiona/memory-engine/issues/55))                                                                                 | `PromotionProvenance` envelope + sidecar `LineageTable` in SQLite. Source fact expiry (`t_expired` set) with lineage preservation. **Structured evidence typing:** `EvidenceBasis { Observed, Inferred, Synthesized }`. **Adversarial self-review** step before promotion                                                                                                                                                                                                    |
| `DreamCycleConfig` ([#56](https://github.com/dutiona/memory-engine/issues/56))                                                                                        | ±2 symmetric rescoring, quarantine path for contradictions. **Research update:** compression as opt-in archival (R10, ACE/DC), Pareto-diverse promotion (R11, GEPA)                                                                                                                                                                                                                                                                                                          |
| Three-layer identity output ([#57](https://github.com/dutiona/memory-engine/issues/57))                                                                               | ANCHORS/CORE/PREDICTIONS structure in `CycleReport`. Each item: `{pattern, directive, false_positive}`                                                                                                                                                                                                                                                                                                                                                                       |
| `FactType::Prediction` ([#132](https://github.com/dutiona/memory-engine/issues/132))                                                                                  | Predictive memory with `t_predicted` timestamp. JEPA-inspired gap — facts that encode expectations for future validation                                                                                                                                                                                                                                                                                                                                                     |
| Spreading activation on retrieval ([#133](https://github.com/dutiona/memory-engine/issues/133))                                                                       | Return clusters not isolated facts — graph-walk activation propagation on retrieval (RMH). **Structure-first retrieval:** use graph topology to shape candidate pool BEFORE vector/keyword search (Signet pattern, gap G-C5). Improves coherence of returned context                                                                                                                                                                                                         |
| Vitality boosts on access ([#134](https://github.com/dutiona/memory-engine/issues/134))                                                                               | Access-triggered importance boost with distance decay to graph neighbors (RMH). Strengthens frequently-used clusters                                                                                                                                                                                                                                                                                                                                                         |
| Grow-and-refine semantic dedup ([#64](https://github.com/dutiona/memory-engine/issues/64))                                                                            | Lightweight maintenance between DreamCycles — incremental dedup without full batch pipeline (R12)                                                                                                                                                                                                                                                                                                                                                                            |
| Recursive sub-query decomposition ([#138](https://github.com/dutiona/memory-engine/issues/138))                                                                       | Multi-hop retrieval via automatic sub-query decomposition (RMH constraint 2). Consumer-provided decomposer trait                                                                                                                                                                                                                                                                                                                                                             |
| Graph-walk pruning ([#153](https://github.com/dutiona/memory-engine/issues/153))                                                                                      | BFS from seed facts through relationship edges — prune low-relevance subgraphs before returning results. Complements spreading activation (#133). Source: note 18 §6                                                                                                                                                                                                                                                                                                         |
| Reasoning-strategy-aware reranking ([#155](https://github.com/dutiona/memory-engine/issues/155))                                                                      | Extend `Reranker` trait with reasoning-strategy signal — consumer injects task-type context so reranking adapts to CoT vs. direct retrieval. Source: note 18 Table 7                                                                                                                                                                                                                                                                                                         |
| Decay-as-deliberate-abstention ([#156](https://github.com/dutiona/memory-engine/issues/156))                                                                          | Formalize forgetting as deliberate abstention — decayed facts surfaced as "I used to know this" rather than silently omitted. 4th abstention type. Source: note 18 §10.3                                                                                                                                                                                                                                                                                                     |
| Mimir 5-signal retrieval weight study ([#157](https://github.com/dutiona/memory-engine/issues/157))                                                                   | Research: explicit signal weighting (BM25, semantic, vividness, mood, recency) vs RRF for episodic memory. Source: note 19 §2.1                                                                                                                                                                                                                                                                                                                                              |

#### Sub-phasing

- **Phase 5a (Minimum Viable Cognitive Pipeline):**
  - InsightStream trait (or `FactType::Insight` — decide during implementation)
  - DreamCycle trait with delta-based `CycleReport` (R7) and retrieve-before-reflect (R8)
  - Outcome tracking — `EventType::OutcomeSignal` (#63, R6)
  - `PromotionProvenance` + `LineageTable`
  - `DreamCycleConfig` with compression as opt-in archival (R10)
  - Three-layer identity output
  - `FactType::Prediction` with `t_predicted` (#132)
  - Spreading activation on retrieval (#133)
  - `agent_id` on Event and Fact schemas (#158) — schema migration, `agent_id: Option<String>`, prerequisite for all multi-agent work
  - Evidence-basis enum on Fact (#159) — `EvidenceBasis { Observed, Inferred, Synthesized }`, prevents frequency-based strengthening of ungrounded claims, pairs with #55
  - Metacognitive rationale field on Fact ([#160](https://github.com/dutiona/memory-engine/issues/160)) — `importance_rationale: Option<String>`, WHY rated important, improves DreamCycle promotion quality
  - Adversarial self-review in DreamCycle promotion gate (#161) — "Wait a minute" pattern before promoting to Wisdom tier, references Cheng et al. sycophancy findings
- **Phase 5b (Behavioral Intelligence):** Targeted scanning (correction pairs, avoidance patterns), quarantine/suppress path, grow-and-refine semantic dedup (#64, R12), hierarchical workflow composition (R13), recursive sub-query decomposition (#138), retrieval-induced forgetting (#154), behavioral feedback loop ([#162](https://github.com/dutiona/memory-engine/issues/162)) — usage outcomes feed retrieval weights (#63 records, #134 boosts, this closes the loop), shadow/dry-run mode ([#163](https://github.com/dutiona/memory-engine/issues/163)) — full pipeline execution without committing, returns what would change (Signet pattern)
- **Phase 5 (independent, any time):** `sample_dormant()` API, vitality boosts on access (#134), graph-walk pruning (#153), reasoning-strategy-aware reranking (#155), decay-as-deliberate-abstention (#156), Mimir 5-signal weight study (#157)
- **Deferred (not in Phase 5):** `compress_behavior()` hook on DreamCycle (depends on consumer LLM integration)

---

### Phase 6: Knowledge Integration 🔲

**Design:** [`docs/design/plans/2026-03-09-future-phases-design.md`](design/plans/2026-03-09-future-phases-design.md)

| Feature                                                                                                                            | Description                                                                                                                                                                                            |
| ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `KnowledgeBaseConnector` trait + `KnowledgeRef` + graceful degradation ([#50](https://github.com/dutiona/memory-engine/issues/50)) | Transport-agnostic trait, optional URI field on facts, "memory lapse" when KB unreachable. **Bidirectional linking** (NYX12: 10,577 bridge_links): `link(fact_id, knowledge_uri)` and `query_linked()` |
| Knowledge change notification ([#51](https://github.com/dutiona/memory-engine/issues/51))                                          | When KB content is superseded/updated, notify memory to re-evaluate dependent facts. **Expand:** pub/sub emission for ME→agent direction (not just KB→ME)                                              |
| research-index bridge ([#52](https://github.com/dutiona/memory-engine/issues/52))                                                  | `memory-kb-research-index` middleware crate implementing `KnowledgeBaseConnector`                                                                                                                      |
| Cross-layer session propagation                                                                                                    | Propagate KB ingestion `session_id` (dutiona/knowledge-base#128) through bridge → #62 co-session edges                                                                                                 |
| Pub/sub event emission on fact append ([#164](https://github.com/dutiona/memory-engine/issues/164))                                | Emit `FactWritten`/`FactExpired`/`FactSuperseded` notifications. Transport: Kafka vs NATS JetStream vs embedded. Includes DLQ design                                                                   |
| Fact notification schema design ([#165](https://github.com/dutiona/memory-engine/issues/165))                                      | Schema for notifications with Schema Registry enforcement                                                                                                                                              |
| MCP server ACL layer ([#166](https://github.com/dutiona/memory-engine/issues/166))                                                 | Capability-token-based auth. Agent identity verification before scope access                                                                                                                           |
| Injection dosing framework ([#167](https://github.com/dutiona/memory-engine/issues/167))                                           | Principled injection volume calibration. IBM: +28.5pp hard, -5.6pp over-injection easy                                                                                                                 |
| Bayesian reputation as consumer trait ([#168](https://github.com/dutiona/memory-engine/issues/168))                                | `trust_prior: Beta(alpha, beta)` per consumer-source pair. RAPS shows 5.2% drop under 60% adversarial                                                                                                  |

---

### Phase 7: Visualization 🔲

| Feature                                                            | Description                                                                                                                                                                 |
| ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Web UI ([#13](https://github.com/dutiona/memory-engine/issues/13)) | WASM+Rust graph visualization. Hybrid: petgraph+fdg-sim (WASM) for layout, sigma.js (WebGL) for rendering. Scope filtering, fact editing, import/export, event log timeline |

---

### Deferred (not planned, trigger-based)

Tracked as individual GitHub issues. Not scheduled for any phase — each has a trigger condition.

| Item                                                                                                          | Trigger                                                                                                                                                  |
| ------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Auth ([#14](https://github.com/dutiona/memory-engine/issues/14))                                              | **Trigger update:** multi-agent safety is the trigger, not just multi-user deployment. Deployment layer concern, not engine core                         |
| SaaS Sync ([#15](https://github.com/dutiona/memory-engine/issues/15))                                         | Product decision for multi-device. CRDT event-log merge + E2EE. Requires determinism guarantees first                                                    |
| Hierarchical summarization ([#17](https://github.com/dutiona/memory-engine/issues/17))                        | Usage exceeds flat consolidation. Memento-style multi-level abstractions                                                                                 |
| Determinism guarantees ([#19](https://github.com/dutiona/memory-engine/issues/19))                            | Before sync work begins. Replay, merge, idempotency rules                                                                                                |
| Cross-session memory sharing ([#36](https://github.com/dutiona/memory-engine/issues/36))                      | Multi-agent deployments. Session isolation or namespace via ScopeTree                                                                                    |
| Multimodal memory ([#37](https://github.com/dutiona/memory-engine/issues/37))                                 | Non-text memories needed. Schema supports it (BLOB embeddings)                                                                                           |
| Multi-node sync ([#38](https://github.com/dutiona/memory-engine/issues/38))                                   | Multi-device eventual consistency. Event envelope fields are forward-compatible                                                                          |
| GEPA meta-optimization ([#65](https://github.com/dutiona/memory-engine/issues/65))                            | Evolutionary optimization of DreamCycle consumer prompts. Trigger: sufficient DreamCycle execution data (>100 runs). Research: GEPA (arXiv:2507.19457)   |
| Keyword-weighted hybrid search ([#66](https://github.com/dutiona/memory-engine/issues/66))                    | Weight FTS5 higher than vector for `FactType::Procedural` retrieval. Trigger: after #42 (Reranker) ships. Research: APC (arXiv:2506.14852)               |
| Energy-based forgetting ([#135](https://github.com/dutiona/memory-engine/issues/135))                         | Unify temporal + representational + structural saliency into single energy metric. Trigger: Phase 5a ships, empirical data on current forgetting gaps    |
| State-delta updates ([#136](https://github.com/dutiona/memory-engine/issues/136))                             | High-frequency memory operations via incremental deltas instead of full fact replacement. JEPA-inspired. Trigger: performance profiling shows bottleneck |
| Attention-based retrieval ([#137](https://github.com/dutiona/memory-engine/issues/137))                       | Attention mechanism over memory store for context-sensitive retrieval. JEPA-inspired. Trigger: Phase 5a evaluation shows retrieval quality gap           |
| Optimal decay-zone boundaries ([#139](https://github.com/dutiona/memory-engine/issues/139))                   | Derive identity/knowledge/operations multipliers statistically. Trigger: sufficient forgetting telemetry from deployed agents                            |
| ANCHOR promotion threshold ([#140](https://github.com/dutiona/memory-engine/issues/140))                      | Derive promotion threshold statistically rather than heuristic. Trigger: sufficient DreamCycle data (Q12 from debate)                                    |
| Context-triggered injection prototype - Level 2 ([#169](https://github.com/dutiona/memory-engine/issues/169)) | Harness runs reasoning context against K/M between thinking blocks. Cross-repo (ME + KB + harness). Trigger: Phase 5a + KB Phase 2 complete              |
| Byzantine fault tolerance for DreamCycle ([#170](https://github.com/dutiona/memory-engine/issues/170))        | Design needed before multi-agent DreamCycle. Trigger: multi-agent deployment decision                                                                    |
| Latency-aware memory materialization ([#171](https://github.com/dutiona/memory-engine/issues/171))            | FAISS/in-memory cache tier for real-time use cases. Trigger: latency profiling shows retrieval bottleneck in production                                  |
| Protocol conformance test suite ([#172](https://github.com/dutiona/memory-engine/issues/172))                 | LAM-style fixtures for memory protocol contract. Trigger: external consumers adopt the engine                                                            |

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
│  dream_cycle · sample_dormant           │  ← Phase 5
│  record_outcome · dedup_semantic        │  ← Phase 5 (#63, #64)
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
Reranker::rerank(query, candidates) → Vec<ScoredFact>  ← Phase 4a ✅
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
