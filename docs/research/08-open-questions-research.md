# Open Questions Research (OQ1-OQ4)

**Date:** 2026-03-08
**Method:** search_index queries on 9 ingested papers (576+153+122+110+107+87+56+52+21 = 1284 chunks)

---

## OQ4: Temporal Contradiction Resolution

**Status:** Partially answered. Clear progression exists in the literature.

### Evolution of Approaches (from Memory Survey §5.2.2)

The survey identifies a clear trajectory:

1. **Rule-based corrections (early):** MemGPT, D-SMART, Mem0g — LLM detects conflicts, invokes replace/delete. **Destructive replacement** erases historical context, breaks temporal continuity.

2. **Temporally-aware soft deletion (mid):** Zep/Graphiti — marks conflicting facts with invalid timestamps rather than deleting. Preserves semantic consistency AND temporal integrity. **Bi-temporal modeling:**
   - `t'created` / `t'expired` — system timeline (when recorded/invalidated)
   - `tvalid` / `tinvalid` — real-world timeline (when the fact was actually true)
   - New edges invalidate old edges by setting `tinvalid` to the new edge's `tvalid`
   - **"Graphiti consistently prioritizes new information when determining edge invalidation"**

3. **Delayed-consistency strategies:** Handle high-frequency updates without real-time I/O burden.

4. **Fully learned update policies (frontier):** RL-optimized systems that learn WHEN to update.

### Mem0's Four-Operation Model (from Mem0 paper §2.1)

For each candidate fact, LLM chooses:

- **ADD** — no equivalent exists
- **UPDATE** — augment existing with complementary info
- **DELETE** — contradicted by new information
- **NOOP** — no change needed

Mem0g extends this with graph-based conflict detection: marks relationships as invalid rather than removing them. **Under 1 minute** for graph construction (vs Zep's "several hours" of async processing).

### Graphiti's Bi-Temporal Model (from Graphiti paper §2)

Knowledge graph G = (N, E, φ) with three hierarchical tiers:

- Episode subgraph (raw interactions)
- Semantic entity subgraph (extracted entities + relations)
- Community subgraph (higher-level clusters)

Each edge has 4 timestamps. When new edges contradict existing ones, the system:

1. Compares new edges against semantically related existing edges (LLM-based)
2. Identifies temporally overlapping contradictions
3. Sets `tinvalid` of old edge to `tvalid` of new edge
4. **Never deletes** — preserves full history

### The Stability-Plasticity Dilemma (Survey §5.2.2)

> "The key challenge is the stability–plasticity dilemma: determining when to overwrite existing knowledge versus when to treat new information as noise. Incorrect updates can overwrite critical information, leading to knowledge degradation and faulty reasoning."

No system fully solves this. Current approaches range from:

- Always trust new info (Graphiti) — simple, but vulnerable to noise
- LLM-arbitrated (Mem0) — better, but expensive per update
- RL-learned policies — best in theory, but requires training data

### Recommendation for Our Agent

**Adopt Graphiti's bi-temporal model** with Mem0's LLM-arbitrated CRUD:

- 4 timestamps on every fact/relationship
- LLM decides ADD/UPDATE/DELETE/NOOP per candidate fact
- Never hard-delete — soft-invalidate with temporal markers
- Store in SurrealDB which natively supports temporal queries

**Open sub-question:** Can Qwen 3.5 reliably arbitrate conflicts? Need to test with contradictory fact pairs.

---

## OQ3: Memory Consolidation Quality

**Status:** Well-covered in literature. Multiple approaches.

### Consolidation Spectrum (Survey §5.2.1)

Three granularities:

1. **Local consolidation** — fine-grained updates between highly similar memory fragments
   - RMM: top-K retrieval + LLM decides whether to merge
   - Reduces risk of incorrect generalization

2. **Cluster fusion** — mid-level grouping
   - CAM: merges all nodes within a cluster into representative summary
   - "Higher-level and consistent cross-sample representations"

3. **Global integration** — holistic consolidation for system-level insights
   - MOOM: constructs stable role profiles by integrating snapshots with historical profiles
   - Difference from §5.1.1 semantic summarization: integrating NEW info into EXISTING summary

### Summarization Quality (Survey §5.1.1)

| Method     | Approach    | Key Mechanism                         |
| ---------- | ----------- | ------------------------------------- |
| MemGPT     | Incremental | Merge new chunks into working context |
| Mem0       | Incremental | LLM-driven summarization              |
| Mem1       | Incremental | RL-optimized (PPO)                    |
| MemAgent   | Incremental | RL-optimized (GRPO)                   |
| MemoryBank | Partitioned | Daily/session boundaries              |

**Key tradeoff:** "Semantic summarization is lossy compression — it prioritizes global semantic coherence over local factual precision. The primary strength is efficiency; the trade-off is resolution loss."

### A-Mem's Approach (from A-Mem paper)

Each new memory triggers:

1. Construct comprehensive note (structured textual attributes + embedding vectors)
2. Analyze historical memory repository for connections (semantic similarity + shared attributes)
3. Dynamic evolution: new memories trigger updates to existing memories' contextual representations
4. "The entire memory continuously refines and deepens its understanding over time"

This is the **self-organizing memory** approach — no fixed schema, memory structure emerges from content.

### Recommendation for Our Agent

**Nightly dream cycle should use 3-level consolidation:**

1. **Local:** Merge duplicate/near-duplicate memories (cosine > 0.92 threshold, from Memori)
2. **Cluster:** Group related episodes into thematic summaries (daily → weekly → monthly)
3. **Global:** Update the agent's core understanding (SOUL.md equivalent) from accumulated patterns

**Quality concern:** Qwen 3.5 at Q4_K_M may lose nuance during summarization. Mitigation: keep raw event log, summaries are DERIVED views (never delete source data).

---

## OQ1: SurrealDB vs Alternatives for Concurrent Agent Memory

**Status:** Informed by paper findings on storage patterns.

### What the Papers Use

| Paper        | Storage                                  | Why                                      |
| ------------ | ---------------------------------------- | ---------------------------------------- |
| Mem0         | Vector DB (unspecified) + optional graph | ADD/UPDATE/DELETE/NOOP per fact          |
| Mem0g        | Graph DB (Neo4j-compatible)              | Entity-relationship + conflict detection |
| Graphiti/Zep | Custom temporal KG (Neo4j)               | Bi-temporal edges, 3-tier subgraphs      |
| A-Mem        | Custom (notes + embeddings + links)      | Self-organizing, no fixed schema         |
| AgeMem       | External memory store (unspecified)      | Tool-based access                        |
| MemGPT/Letta | SQLite/PostgreSQL + vector               | Hierarchical tiers                       |

**Key finding from Mem0 vs Zep comparison:** Zep's graph construction requires "multiple asynchronous LLM calls and extensive background processing" — "several hours" delay before queries work. Mem0 completes in "under a minute." **Real-time access matters.**

### Concurrent Access Patterns for Our Agent

The agent needs:

1. **Fast reads** during inference (retrieve relevant memories in <100ms)
2. **Writes** during and after interactions (persist new memories, update existing)
3. **Batch processing** during dream cycle (consolidation, decay, pruning)
4. **Graph traversal** for multi-hop reasoning (e.g., "what decisions affected project X?")
5. **Temporal queries** for contradiction resolution (bi-temporal model)

### Assessment Update

| Option                    | Pros                                                 | Cons                                                               | Fit                       |
| ------------------------- | ---------------------------------------------------- | ------------------------------------------------------------------ | ------------------------- |
| SurrealDB 3.0             | Rust-native, vector+graph+KV+temporal, single binary | Young (Feb 2026), untested at our scale                            | Best if stable            |
| SQLite + Petgraph         | Battle-tested, embedded, simple                      | No native vector, need separate vector index, graph in-memory only | Safest fallback           |
| LanceDB + Petgraph        | Embedded vector (Rust native), columnar, versioned   | No graph persistence, need Petgraph for graph layer                | Good hybrid               |
| Neo4j (like Graphiti/Zep) | Mature graph, temporal support                       | Heavy (JVM), external process, not embeddable                      | Overkill for single agent |

**Updated recommendation:** Start with **SQLite (FTS5) + LanceDB (vector) + in-memory Petgraph** as the safe path. This gives:

- SQLite: durable storage, FTS5 keyword search, temporal queries via SQL
- LanceDB: vector search, embedded, Rust SDK, columnar with versioning
- Petgraph: in-memory graph operations, loaded from SQLite on startup

Migrate to SurrealDB 3.0 when it proves stable. The event-sourced log guarantees we can replay into any storage backend.

---

## OQ2: Event-Sourcing in Rust

**Status:** Not directly addressed in papers (they focus on memory, not implementation).

### What Papers Imply

- **Graphiti:** "dynamically updates the knowledge graph with new information in a non-lossy manner" — this IS event sourcing (new facts don't destroy old ones)
- **Mem0:** ADD/UPDATE/DELETE/NOOP — the NOOP + UPDATE-not-DELETE pattern preserves history
- **A-Mem:** "dynamic evolution — new memories trigger updates to existing memories" — derived views from events
- **Memory Survey §5.2.3 on Forgetting:** "deliberate removal of outdated, redundant, or low-value information" — implies you need the raw data to decide what to forget

### Practical Assessment

Event sourcing for our agent means:

1. **Append-only log:** Every interaction, tool call, memory operation recorded
2. **Derived views:** SurrealDB/LanceDB/Petgraph are materialized from the log
3. **Replay capability:** Can reconstruct any view from the log
4. **Audit trail:** Know exactly what happened and when

**Rust options:**

- `event-store-rs` — EventStoreDB client, requires external service (overkill)
- Custom on SQLite WAL — append-only table, WAL mode for concurrent reads, simple
- Custom on NATS JetStream — distributed, persistent streams (overkill for single agent)
- **Simplest:** SQLite table `events(id, timestamp, event_type, payload_json)` in WAL mode

**Recommendation:** Roll own on SQLite WAL. It's 20 lines of Rust with rusqlite. The event log IS the source of truth. Everything else is derived. This is exactly what OpenClaw does (markdown files as source, SQLite as derived index) but with stronger guarantees.

---

## Forgetting Mechanisms (bonus — informed by Survey §5.2.3)

The survey identifies forgetting as a first-class operation:

> "Memory forgetting refers to the deliberate removal of outdated, redundant, or low-value information to free capacity and maintain focus."

### Active Memory Policies (Survey §7)

> "Passive mechanisms are being superseded by Active Memory Policies. Instead of treating memory as a fixed buffer that blindly stores recent history, new models are designed as Cognitive Workspaces that actively curate, summarize, and discard information based on task relevance."

This validates:

1. Our graph-aware forgetting idea (from Ori Mnemos)
2. The ACE curation finding (active > passive, 10.6% improvement)
3. kubrador's "capture layer matters" — write-time quality gate

### Dual-System Architecture (Survey §7)

> "Inspired by cognitive science, world models could be bifurcated into fast and slow components. System 1 handles immediate physics using efficient backbones like SSMs. System 2 handles complex reasoning using large-scale VLMs or explicit memory databases."

For our agent: System 1 = Qwen 3.5 35B (fast inference), System 2 = Opus/Codex via escalation (deep reasoning). The memory system bridges both.

---

## Summary: OQ Resolution Status

| OQ  | Question                  | Status                 | Answer                                                                               |
| --- | ------------------------- | ---------------------- | ------------------------------------------------------------------------------------ |
| OQ1 | SurrealDB vs alternatives | **Revised**            | Start with SQLite+LanceDB+Petgraph, migrate to SurrealDB when stable                 |
| OQ2 | Event-sourcing in Rust    | **Answered**           | Roll own on SQLite WAL, 20 lines of rusqlite                                         |
| OQ3 | Consolidation quality     | **Answered**           | 3-level consolidation (local/cluster/global), keep raw log as source of truth        |
| OQ4 | Temporal contradictions   | **Partially answered** | Adopt bi-temporal model (Graphiti) + LLM-arbitrated CRUD (Mem0). Test with Qwen 3.5. |
