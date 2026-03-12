# Future Phases Design — memory-engine

**Date:** 2026-03-09
**Status:** Approved (pending implementation plans per phase)
**Process:** 4-advisor debate (Claude, Gemini, Codex/o3, GPT-5.4), 3 rounds + independent review, research synthesis

---

## Foundational Principle: Knowledge ≠ Memory ≠ Wisdom

| Layer         | Definition                                                                                            | System                                                      | Ownership                                                                                                                                                |
| ------------- | ----------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Knowledge** | Raw, unprocessed content — papers, code, documents, data                                              | Knowledge Base (e.g., research-index)                       | Stores original chunks. Never stores AI-processed summaries to avoid content corruption through recursive summarization.                                 |
| **Memory**    | What the agent internalized, connected, and made sense of — insights, relationships, learned patterns | Memory Engine (this project)                                | Stores summaries, facts, semantic links. Updated as the agent gains deeper understanding. Event log preserves full history of how understanding evolved. |
| **Wisdom**    | Meta-reasoning about when and how to apply knowledge and memory                                       | The model itself (training weights, architecture, tool use) | Out of scope. Wisdom is the model's job, not the engine's.                                                                                               |

This separation has deep implications:

- Memory can reference knowledge (`KnowledgeRef` URIs) but never duplicates raw content.
- Memory summaries evolve (consolidation, conflict resolution, deeper insight) while knowledge stays immutable.
- An agent's memory has independent value — years of internalized expertise, patterns, and connections are worth more than the raw documents they came from.
- Multiple agents can share a knowledge base but have distinct memories (different perspectives on the same knowledge).

---

## Phase Overview

> **Note (2026-03-12):** Phase numbering was restructured during roadmap reconciliation. This document's "Phase 5: Knowledge Integration" is now Phase 6 in ROADMAP.md. Phase 5 in ROADMAP.md is "Cognitive Pipelines" (added post-community research). See ROADMAP.md for canonical phase numbering.

| Phase | Name                                       | Status      | Scope                                                                                                                   |
| ----- | ------------------------------------------ | ----------- | ----------------------------------------------------------------------------------------------------------------------- |
| 1     | Ingest → Query Loop                        | Done        | Hybrid search, bi-temporal facts, event sourcing                                                                        |
| 2     | Graph, Consolidation, Forgetting, Conflict | Done        | petgraph, 3-pass consolidation, Ebbinghaus decay, ConflictArbiter                                                       |
| 3     | Hardening & Scoping                        | In progress | Thread safety, async, scoping, SQL filters, benchmarks, migration framework, `AddFactOptions`, basic `resume_context()` |
| 3b    | Temporal Memory & Agent Lifecycle          | Planned     | Unforgettable flag, future memory, scheduling API, `resume_context()` rework, event envelope forward-compat             |
| 4     | Operability & MCP Server                   | Planned     | Inspection APIs, CLI, MCP server adapter, import/export, archival compression                                           |
| 5     | Knowledge Integration                      | Planned     | KB protocol trait, `KnowledgeRef` on facts, graceful degradation, research-index bridge                                 |
| —     | Deferred                                   | Backlog     | ANN index, Web UI, Auth, SaaS Sync, evaluation harness, hierarchical summarization                                      |

---

## Phase 3b: Temporal Memory & Agent Lifecycle

**Theme:** Make the engine time-aware and give agents a proper cognitive boot sequence.

### Features

#### 1. Unforgettable Memories

**Problem:** Some facts must never decay — identity, critical user preferences, foundational knowledge. The current forgetting pipeline has no bypass mechanism.

**Design:**

- Add `is_pinned: bool` to the `facts` table (schema migration v2→v3, or vN→vN+1 depending on Phase 3 final version).
- Forgetting pipeline (`forget()`) skips facts where `is_pinned = true`.
- Manual pinning: `AddFactOptions::pinned(true)` sets the flag explicitly.
- Auto-pinning: New `PersistenceClassifier` trait.

**New trait:**

```rust
pub trait PersistenceClassifier {
    fn should_pin(&self, fact: &Fact) -> bool { false }
}
```

- Called during `add_fact()` if provided. Sets `is_pinned` based on consumer logic.
- Consumer can implement LLM-based classification, regex rules, or domain heuristics.
- Default: `false` — opt-in, zero behavior change for existing consumers.
- Consistent with existing trait pattern (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`).

#### 2. Future Memory

**Problem:** Agents need to set reminders — "check if software X released on March 19." Currently, facts with future `t_valid` exist in the schema but nothing surfaces them.

**Design:**

- Near-zero new code. The bi-temporal model already supports `t_valid` in the future.
- `resume_context(now)` includes a stage that queries `WHERE t_valid <= :now AND t_valid > :last_checked`.
- Facts with `t_valid > now` remain invisible until their date arrives.
- No background thread, no event loop. The library is passive — the consumer decides when to call.

#### 3. Scheduling API

**Problem:** Consumers need different retrieval granularities: full context rebuild vs. incremental check vs. scheduling hint.

**Design — three methods:**

| Method                | Purpose                                            | Returns                                                                |
| --------------------- | -------------------------------------------------- | ---------------------------------------------------------------------- |
| `resume_context(now)` | Rebuild full working context at session start      | Tiered facts: pinned + high-importance + recently-due + scope-filtered |
| `drain_due(now)`      | Get only newly-due future facts since last check   | Vec of facts where `t_valid <= now` and not yet surfaced               |
| `next_due_time()`     | Scheduling hint: when should consumer check again? | `Option<DateTime>` — earliest `t_valid` among future-dated facts       |

The library stays passive. `next_due_time()` lets the consumer set a precise timer without wasteful polling.

#### 4. `resume_context()` Rework

**Problem:** Current Phase 3 implementation is basic. Needs to compose multiple retrieval stages into a proper cognitive boot sequence.

**Design — tiered retrieval pipeline:**

1. Load pinned (unforgettable) facts — always present.
2. Load high-importance facts (materialized importance score, top-N).
3. Load future facts now due (`t_valid <= now`).
4. Load scope-relevant recent facts (filtered by active scope).
5. Annotate KB references with availability status (reachable / unavailable / not-yet-linked).
6. Return structured `ResumeContext` with categorized tiers, not a flat list.

Stage 5 is a forward-looking stub — implemented properly in Phase 5 (Knowledge Integration).

#### 5. Materialized Importance Score

**Problem:** Importance is computed on-the-fly during `forget()`. `resume_context()` needs to sort by importance without full recomputation.

**Design:**

- Add `importance_score: f64` to the `facts` table.
- Updated on: `add_fact()` (initial), `increment_access()`, `consolidate()`, `forget()`.
- Uses existing multi-signal formula: recency (Ebbinghaus) + frequency (ln_1p) + connectivity (graph degree).

#### 6. Event Envelope Forward-Compatibility

**Problem:** Future sync needs multi-writer metadata on events. Retrofitting after real usage is expensive.

**Design:**

- Add to event table: `origin_node_id TEXT NOT NULL DEFAULT 'local'`, `sequence_id INTEGER NOT NULL DEFAULT 0`, `created_at TEXT` (advisory, not for ordering).
- Zero behavioral change in single-node mode. These fields are metadata-only until sync is implemented.
- `origin_node_id` identifies the source device. `sequence_id` is a per-origin monotonic counter.
- `created_at` is explicitly advisory — wall clocks are unreliable for ordering across devices.

---

## Phase 4: Operability & MCP Server

**Theme:** Make the engine observable, debuggable, and network-accessible. Observability before external protocols.

### Features

#### 1. Inspection APIs (in core)

| API                    | Purpose                                                                                           |
| ---------------------- | ------------------------------------------------------------------------------------------------- |
| `explain_fact(id)`     | Why is this fact active / forgotten / due / pinned? Returns provenance chain.                     |
| `replay_events(range)` | Replay event log segment. For debugging consolidation, forgetting, conflict resolution decisions. |
| `dump_state(format)`   | Export full engine state. JSON for portability, SQLite backup for speed.                          |
| `statistics()`         | Fact count, edge count, scope tree depth, pinned count, due count, storage size.                  |

#### 2. CLI Inspector (`memory-engine-cli`)

Wraps inspection APIs. Subcommands: `inspect`, `dump`, `query`, `explain`, `stats`, `import`, `export`.

Design goal: as clean and useful as `gh` (GitHub CLI). Not an MCP server — a direct tool for operators.

#### 3. MCP Server Adapter (`memory-engine-mcp`)

- Initially in-tree as a workspace member. Planned to eventually become a separate repo once the Rust public API stabilizes and we can reason about MCP version compatibility.
- Maps 1:1 to engine API: `ingest`, `add_fact`, `query`, `resume_context`, `drain_due`, `consolidate`, `forget`, `explain_fact`.
- MCP version pinning: explicit supported version range in server metadata. Reject incompatible clients with clear error. No silent degradation.

#### 4. Import/Export

- **JSON event log dump** — portable, human-readable, interoperable. Primary format.
- **SQLite backup** — fast, binary, for same-version restore.
- Consider gzip/zstd compression for JSON exports (lossless).

#### 5. Archival Compression (Cold Storage)

**Problem:** Long-running agents (10 agents × 10 years) accumulate massive event logs. The live DB shouldn't bloat indefinitely.

**Design:**

- Not automatic. The agent must be hinted to run archival explicitly.
- Extract old, non-pinned memory chunks into aggressively compressed `.pak` files (zstd or similar) alongside the live DB.
- Live DB retains: all pinned facts (forever), recent facts, active graph edges, summaries.
- Archived `.pak` files are a **very slow fallback**. Queries that hit archived data should warn the consumer with an ETA.
- Unforgettable memories never get archived — they live in the live DB permanently.
- Archival is a form of compaction but preserves the event log (compressed, not discarded).

#### 6. Semantic Extraction Queries

**Problem:** Operators need high-level queries like "all memories for project X during period A to B, related to feature Y."

**Design:**

- Compose existing primitives: scope filtering + temporal range + semantic search (hybrid FTS+vector).
- New query builder: `MemoryQuery::scope("project-x").period(a, b).semantic("feature Y").execute()`.
- Limitations: accuracy depends on how well facts are scoped and tagged. Not magic — garbage in, garbage out.
- May need an LLM-in-the-loop for truly semantic extraction (consumer provides via trait, engine orchestrates).

---

## Phase 5: Knowledge Integration

**Theme:** Loosely link memory to external knowledge bases. Memory references knowledge; knowledge is independent.

### Features

#### 1. `KnowledgeBaseConnector` Trait (in core)

```rust
pub trait KnowledgeBaseConnector {
    fn resolve(&self, uri: &str) -> Result<KnowledgeChunk, KBError>;
    fn check_availability(&self, uri: &str) -> bool;
}
```

- Transport-agnostic. Consumer implements for their KB (HTTP+JSON, gRPC, MCP tool-to-tool, in-process).
- Reference implementation uses HTTP+JSON for maximum interoperability.
- Engine never calls this automatically — consumer triggers resolution, or `resume_context()` checks availability.

#### 2. `KnowledgeRef` on Facts

- New optional field on facts: `knowledge_ref: Option<String>` (URI).
- Engine stores the URI and the agent's summary/insight. KB stores the raw content.
- When KB is unreachable, the fact is still fully queryable — the summary stands on its own. An "unavailable" annotation is added for the consumer to handle.

#### 3. Graceful Degradation

- Memory-engine functions fully without any KB connected.
- When a referenced KB is unreachable: fact remains queryable, `knowledge_ref` annotated with "unavailable," consumer decides retry strategy.
- Metaphor: "I know I learned about X but I'm having a memory lapse about the details. Let me try again later."

#### 4. research-index Bridge Crate

- Separate middleware crate: `memory-kb-research-index`.
- Implements `KnowledgeBaseConnector` for the research-index MCP server.
- Maps: `resolve(uri)` → research-index `search_index` or `get_paper_tool`. `check_availability()` → health check endpoint.
- Neither project depends on the other directly. The bridge crate is the only coupling point.

---

## Deferred Items (to be filed as GitHub issues)

| Item                        | Trigger                              | Scope                                                                                                                                                                                                                                                                                             |
| --------------------------- | ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ANN Vector Index            | Benchmarks show >50ms at scale       | Replace brute-force cosine with LanceDB or similar. Phase 3 benchmarks provide baseline.                                                                                                                                                                                                          |
| Web UI                      | API stable, >1000 facts to visualize | Separate project. WASM+Rust. Hybrid approach: petgraph+fdg-sim (WASM) for layout, sigma.js (WebGL) for rendering. Scope filtering, fact editing, import/export, event log timeline.                                                                                                               |
| Auth                        | Multi-user deployment decision       | Not in engine core. Deployment layer concern (MCP server or REST API handles auth). Multi-tenancy = multiple engine instances or scope-based namespace.                                                                                                                                           |
| SaaS Sync                   | Product decision to go multi-device  | Separate product. CRDT event-log merge (per-device partitioned append, union merge). Asymmetric key encryption (one keypair per machine). Diff-based push/pull. Versioning with rollback. HMAC signature verification. Feasible because event-sourced. See research notes on Automerge + SecSync. |
| Evaluation Harness          | After Phase 4 ships                  | Regression corpus for retrieval quality, consolidation correctness, forgetting behavior, latency. "Feels good" is not enough (GPT-5.4).                                                                                                                                                           |
| Hierarchical Summarization  | Usage exceeds flat consolidation     | Memento-style multi-level abstractions. Currently consolidation is flat (local/cluster/global).                                                                                                                                                                                                   |
| Schema Evolution Discipline | Before Phase 4 ships                 | Versioning policy, migration rules, backwards-compat testing. Event envelope versioning.                                                                                                                                                                                                          |
| Determinism Guarantees      | Before sync work begins              | Crisp rules for replay, merge, clock skew, idempotency. Required for CRDT sync correctness.                                                                                                                                                                                                       |

---

## GPT-5.4 Blind Spots (addressed in roadmap)

| Blind Spot                  | Where Addressed                                           |
| --------------------------- | --------------------------------------------------------- |
| Snapshotting & compaction   | Phase 4: Archival Compression (cold storage `.pak` files) |
| Determinism guarantees      | Deferred issue, prerequisite for sync                     |
| Schema evolution discipline | Deferred issue, prerequisite for Phase 4                  |
| Evaluation harness          | Deferred issue, after Phase 4                             |
| Import/export & backup      | Phase 4: Import/Export (JSON + SQLite)                    |
| Explainability API          | Phase 4: `explain_fact(id)` inspection API                |

---

## Research References

- **ACC (Agent Cognitive Compressor)** — arxiv 2601.11653. Bounded Compressed Cognitive State. Informs `resume_context()` design: bounded, updateable, primes retrieval.
- **MCP Memory Servers** — Hindsight, Redis Agent Memory, Cognee, mcp-mem0. Validates MCP as transport. No standard for memory semantics yet.
- **Automerge + SecSync** — CRDT + E2EE for local-first sync. Most promising stack for future SaaS Sync.
- **cr-sqlite / SQLiteSync** — CRDT replication for SQLite. Relevant for sync, but encryption must be composed separately.
- **egui_graphs + fdg-sim** — Rust-native graph visualization. Sufficient for moderate graphs; sigma.js hybrid for scale.

---

## Debate Artifacts

All debate rounds archived in `docs/debate/future-phases/`:

- `r1-claude.md`, `r1-gemini.md`, `r1-codex.md` — Round 1: Initial phasing proposals
- `r2-claude.md`, `r2-gemini.md`, `r2-codex.md` — Round 2: Sharpening disagreements
- `r3-claude.md`, `r3-gemini.md`, `r3-codex.md` — Round 3: Final positions
- `r3-gpt54.md` — GPT-5.4 independent review (blind spots, alternative roadmap)
- `research-notes.md` — Research synthesis (4 topics)
