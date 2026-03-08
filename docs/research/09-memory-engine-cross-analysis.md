# Cross-Analysis: Memory Engine Design Requirements

**Date:** 2026-03-08
**Purpose:** Synthesize all research (9 papers, 22 relationships, community sources) into concrete design requirements for the `memory-engine` Rust crate.
**Dual consumers:** Autonomous agent (Mac Mini M4 + Qwen 3.5) AND Claude Code / IDE workflow.

---

## 1. The Five Primitives

From our research, every memory system decomposes into 5 fundamental operations:

### 1.1 `ingest(event)` — Append to Log

**What the papers say:**

- **Event-sourced log** is the source of truth (Graphiti: "non-lossy dynamic updates", Survey §5.2)
- **Mem0** processes message pairs incrementally — extracts salient facts per interaction
- **A-Mem** constructs "comprehensive notes" with structured attributes + embeddings per new memory
- **AgeMem** uses ADD as one of its core memory tools

**Design implications:**

- Append-only event table: `events(id, timestamp, event_type, payload, source)`
- Every write to the memory system first creates an event
- Derived views (facts, summaries, graph) are materialized from events
- SQLite WAL mode for concurrent reads during writes

**Key insight from Survey §5.1:** Memory formation has 5 types — semantic summarization, knowledge distillation, structured construction, embedding encoding, raw storage. Our `ingest` should support all 5 as downstream transforms from the raw event.

### 1.2 `query(prompt, filters)` — Hybrid Retrieval

**What the papers say:**

- **Survey §5.3:** 4-step retrieval pipeline — timing/intent → query construction → retrieval strategy → post-retrieval processing
- **Memori** (Rust): hybrid FTS5 + cosine vector + Reciprocal Rank Fusion, 43μs reads
- **CoALA §4.3:** retrieval can be rule-based, sparse, or dense depending on memory type
- **Graphiti:** graph traversal + embedding similarity + temporal filtering
- **Survey §5.3.1:** retrieval timing ranges from always-on to explicit triggers to internal signals

**Design implications:**

- Three search backends: FTS5 (keyword), vector (semantic), graph traversal (relational)
- Reciprocal Rank Fusion to merge results (proven pattern from Memori)
- Temporal filtering: only return facts valid at time T (bi-temporal model)
- Query rewriting: decompose complex queries into sub-queries (Survey §5.3.2)
- Latency budget: <100ms for IDE consumer, relaxed for autonomous agent

**Consumer differences:**
| Aspect | Autonomous Agent | Claude Code / IDE |
|--------|-----------------|-------------------|
| Trigger | Self-directed (agent decides when to recall) | Hook-driven (session start, prompt submit) |
| Budget | Can afford multi-hop graph walks | Needs fast single-pass retrieval |
| Scope | All memory across all tasks | Project-scoped + cross-project patterns |

### 1.3 `consolidate()` — Merge, Cluster, Integrate

**What the papers say:**

- **Survey §5.2.1:** Three granularities:
  1. **Local:** merge near-duplicate fragments (RMM: top-K + LLM decides merge, cosine >0.92 threshold from Memori)
  2. **Cluster:** fuse related episodes into thematic summaries (CAM: cluster → representative summary)
  3. **Global:** update core understanding (MOOM: integrate snapshots with historical profiles)
- **A-Mem:** "entire memory continuously refines and deepens its understanding over time"
- **OpenClaw community:** pre-compaction flush + daily log → long-term MEMORY.md promotion

**Design implications:**

- Consolidation runs as a batch job (cron / dream cycle), not inline
- Three passes: local dedup → cluster fusion → global integration
- Raw events are NEVER deleted — consolidation creates derived records
- Lossy compression tradeoff: "prioritizes global coherence over local precision" (Survey §5.1.1)
- Mitigation: always keep raw event log, summaries are derived views

**Key tradeoff (Survey):** "Semantic summarization is lossy compression." Our engine must support both the compressed view AND the raw source, with provenance chains linking them.

### 1.4 `forget(policy)` — Decay, Prune, Archive

**What the papers say:**

- **Survey §5.2.3:** Three forgetting mechanisms:
  1. **Time-based:** exponential decay (MemGPT: evict oldest, MAICC: soft weight decay)
  2. **Frequency-based:** LFU/LRU (XMem: remove low-frequency, MemOS: archive highly active)
  3. **Importance-driven:** composite scores combining time + frequency + semantics (TiM, MemTool: LLM-assessed importance)
- **Ori Mnemos (community):** graph-aware forgetting — isolated nodes pruned based on connectivity
- **Memori (Rust):** Ebbinghaus decay with 69-day half-life + cosine >0.92 dedup

**Design implications:**

- Multi-signal importance score: `importance = f(recency, frequency, graph_degree, semantic_relevance)`
- Soft deletion (mark as archived) rather than hard deletion — preserves audit trail
- Graph connectivity as a forgetting signal: memories connected to many others decay slower
- LLM-assessed importance for high-value decisions (expensive, use sparingly)
- Configurable policy: consumer chooses aggressiveness

**Important caveat (Survey §5.2.3):** "Heuristic forgetting mechanisms like LRU may eliminate long-tail knowledge, which is seldom accessed but essential for correct decision-making." → When storage cost is not critical, prefer archiving over deleting.

### 1.5 `resolve_conflict(old_fact, new_fact)` — Temporal Arbitration

**What the papers say:**

- **Graphiti §2.2.3:** Bi-temporal model with 4 timestamps:
  - `t'created` / `t'expired`: system timeline (when recorded/invalidated)
  - `tvalid` / `tinvalid`: real-world timeline (when the fact was actually true)
  - "Graphiti consistently prioritizes new information when determining edge invalidation"
- **Mem0 §2.1:** LLM-arbitrated CRUD — ADD/UPDATE/DELETE/NOOP per candidate fact
- **Survey §5.2.2:** Evolution: destructive replacement → soft deletion → bi-temporal → learned policies
- **Stability-plasticity dilemma (Survey):** "determining when to overwrite existing knowledge versus when to treat new information as noise"

**Design implications:**

- Every fact has 4 timestamps (bi-temporal model from Graphiti)
- LLM arbitrates conflicts: compare new fact against semantically related existing facts
- Never hard-delete — set `t'expired` and `tinvalid` on the old fact
- Trust-level per source: some sources more authoritative than others
- Configurable policy: always-trust-new (simple), LLM-arbitrated (better), RL-learned (frontier)

**Open question:** Can Qwen 3.5 reliably arbitrate conflicts? This needs empirical testing.

---

## 2. Data Model

### 2.1 Core Schema (derived from research)

```
events (append-only, source of truth)
├── id: u64
├── timestamp: DateTime<Utc>
├── event_type: EventType  // Interaction, ToolCall, MemoryOp, SystemEvent
├── payload: serde_json::Value
├── source: String  // who/what generated this event
└── session_id: Option<String>

facts (derived from events, bi-temporal)
├── id: u64
├── content: String
├── embedding: Vec<f32>  // nomic-embed-text, 768-dim
├── fact_type: FactType  // Semantic, Episodic, Procedural
├── t_created: DateTime<Utc>   // system: when recorded
├── t_expired: Option<DateTime<Utc>>  // system: when invalidated
├── t_valid: Option<DateTime<Utc>>    // real-world: when became true
├── t_invalid: Option<DateTime<Utc>>  // real-world: when stopped being true
├── source_event_id: u64  // provenance to event log
├── importance: f32  // composite score
├── access_count: u32
├── last_accessed: DateTime<Utc>
└── metadata: serde_json::Value

edges (graph relationships between facts)
├── id: u64
├── source_fact_id: u64
├── target_fact_id: u64
├── relation_type: String  // "causes", "contradicts", "supports", "part_of"
├── weight: f32
└── t_created, t_expired (bi-temporal)

summaries (derived from consolidation)
├── id: u64
├── content: String
├── embedding: Vec<f32>
├── level: ConsolidationLevel  // Local, Cluster, Global
├── source_fact_ids: Vec<u64>  // provenance
└── created_at: DateTime<Utc>
```

### 2.2 Storage Mapping

| Concern          | Technology         | Why                                           |
| ---------------- | ------------------ | --------------------------------------------- |
| Event log        | SQLite WAL         | Append-only, durable, concurrent reads        |
| Facts + metadata | SQLite             | Temporal queries via SQL, battle-tested       |
| Full-text search | SQLite FTS5        | Keyword retrieval, BM25 ranking               |
| Vector search    | LanceDB            | Embedded Rust-native, columnar, versioned     |
| Graph operations | In-memory Petgraph | Fast traversal, loaded from SQLite on startup |

This is the SQLite+LanceDB+Petgraph stack from OQ1. Migrate to SurrealDB when it proves stable.

---

## 3. The Layer Question

### 3.1 What CoALA proposed (4 types)

| Type       | CoALA Definition                           | Memory Engine Mapping                                                             |
| ---------- | ------------------------------------------ | --------------------------------------------------------------------------------- |
| Working    | Short-term, in-context                     | NOT in engine — consumer's responsibility (context window)                        |
| Episodic   | "What happened" — event sequences          | `facts` with `fact_type=Episodic` + provenance to events                          |
| Semantic   | "What's true" — facts, entities, relations | `facts` with `fact_type=Semantic` + `edges` graph                                 |
| Procedural | "How to do X" — skills, workflows          | `facts` with `fact_type=Procedural` OR external versioned files indexed by engine |

### 3.2 Our revised view

The 5-layer hierarchy (L0-L4) was a useful scaffold. In the engine, it collapses to:

**One store, multiple projections.** A single fact can be:

- Episodic (linked to a specific event/session)
- Semantic (a general truth derived from episodes)
- Procedural (a workflow step that evolved from experience)

The `fact_type` is a tag, not a storage partition. Consolidation can PROMOTE facts: episodic → semantic (when a pattern recurs), semantic → procedural (when a workflow crystallizes).

**What stays OUTSIDE the engine:**

- L0 (context window) — consumer manages this
- L1 (SOUL.md, MEMORY.md) — consumer generates these FROM engine queries
- Procedure files — version-controlled separately, engine indexes but doesn't own

The engine exposes: "give me the top-K most important active facts matching this query, filtered by type and time" — the consumer decides how to format and inject them.

---

## 4. Consumer Integration Patterns

### 4.1 Autonomous Agent (Qwen 3.5 on Mac Mini)

```
Agent Loop:
  1. Receive task/input
  2. engine.query(input, filters={type: [semantic, procedural], top_k: 10})
  3. Inject results into context window
  4. LLM generates response + memory operations
  5. engine.ingest(event={interaction, tool_calls, decisions})
  6. If LLM suggests fact changes: engine.resolve_conflict(old, new)

Dream Cycle (nightly cron):
  1. engine.consolidate()  // 3-level pass
  2. engine.forget(policy={max_age: 90d, min_importance: 0.3})
  3. Generate updated SOUL.md / MEMORY.md from engine.query(global_summary)
```

### 4.2 Claude Code / IDE Workflow

```
Session Start Hook:
  engine.query(project_context, filters={scope: project_name, top_k: 5})
  → Inject into system prompt

Prompt Submit Hook:
  engine.query(user_prompt, filters={top_k: 3})
  → Append relevant memories to context

Session End Hook:
  engine.ingest(event={session_summary, decisions, learnings})

Weekly /insights Review:
  engine.query("friction patterns last 7 days", filters={type: episodic})
  → LLM analyzes → updates CLAUDE.md
```

---

## 5. What Makes This Novel

Compared to existing systems in the literature:

1. **Unified engine, two consumers** — no existing system serves both autonomous agents and developer workflows from the same memory backend
2. **Event-sourced with bi-temporal facts** — combines Graphiti's temporal model with event sourcing (no existing system does both)
3. **Graph-aware forgetting** — extends Ori Mnemos' graph topology signal with time-decay and frequency (Survey's 3 forgetting types + community's structural signal)
4. **Rust-native embedded** — no external services (no Neo4j, no separate vector DB). SQLite+LanceDB+Petgraph as a single embeddable stack
5. **Provenance chains** — every derived fact traces back to raw events. Every summary traces back to source facts. Full audit trail.

---

## 6. Open Design Questions for Planning Phase

| #   | Question                     | Options                                                     | Research Input                                            |
| --- | ---------------------------- | ----------------------------------------------------------- | --------------------------------------------------------- |
| DQ1 | Embedding model?             | nomic-embed-text (768d) vs MiniLM (384d) vs Qwen embed      | nomic already deployed, 768d is good balance              |
| DQ2 | Graph persistence format?    | SQLite adjacency table vs separate graph file               | SQLite for durability, Petgraph in-memory for speed       |
| DQ3 | Consolidation trigger?       | Time-based (nightly) vs size-based (>N new events) vs both  | Both — nightly + threshold                                |
| DQ4 | LLM for conflict resolution? | Qwen 3.5 (local) vs API (Opus/Haiku)                        | Local first, escalate complex conflicts                   |
| DQ5 | Fact type taxonomy?          | CoALA's 4 types vs simpler 2 (raw/derived) vs flexible tags | Start with 3 (episodic/semantic/procedural), allow custom |
| DQ6 | API surface?                 | Rust trait + MCP server vs Rust trait only                  | Rust trait first, MCP adapter later                       |
| DQ7 | Concurrency model?           | Single-writer (lane queue) vs MVCC                          | Single-writer for simplicity (OpenClaw pattern)           |

---

## 7. Next Steps

1. **Figure analysis** — review extracted figures for architecture patterns we missed
2. **Design doc** — formal design document for `memory-engine` crate
3. **API surface** — define Rust traits for the 5 primitives
4. **Prototype** — minimal viable engine with ingest + query (skip consolidation/forget initially)
