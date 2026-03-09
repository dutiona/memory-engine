# Design Choices

Each decision below was informed by the research foundation (9 papers, community synthesis, multi-AI debate) and validated through implementation. Status markers indicate whether the decision is realized in code.

---

## No Layers -- One Store, Multiple Projections **{Implemented}**

Early design explored a 5-layer hierarchy (L0-L4) inspired by CoALA and community patterns. This collapsed during implementation. Instead, `fact_type` is a tag (`Episodic`, `Semantic`, `Procedural`), not a partition. All facts live in one table, one FTS5 index, one vector space.

**Rationale:** A-Mem demonstrated that self-organizing memory without predefined schemas outperforms rigid layering. The tag approach gives consumers full flexibility to query across types or filter by type, without maintaining separate storage backends or synchronization logic.

**Trade-off:** No physical isolation between memory types. If a consumer needs hard boundaries, they use scopes instead.

---

## Traits for LLM Operations **{Implemented}**

The engine defines three traits (`EmbeddingProvider`, `SummaryGenerator`, `ConflictArbiter`) and has zero network or LLM dependencies. All intelligence is injected by the consumer.

**Rationale:** AgeMem validated this pattern -- memory operations as tool-based actions where the agent decides what/when to store. Keeping LLM calls out of the engine means: (1) the core is deterministic and testable with mocks, (2) consumers can use any embedding model (local, API, quantized), (3) the engine works in constrained environments (embedded, WASM).

**Trade-off:** Consumers must implement these traits, which adds integration work. But the alternative -- baking in a specific model -- would make the engine opinionated and fragile.

---

## Event-Sourced -- Append-Only Log as Source of Truth **{Implemented}**

Every interaction enters the system through an append-only event log (`EventStore`). Facts are explicitly derived from events by the consumer calling `add_fact`, not auto-projected.

**Rationale:** Multi-AI debate consensus: event-sourcing provides an audit trail, enables replay into alternative storage backends (the migration safety net), and separates raw observations from derived knowledge. Graphiti's "non-lossy dynamic updates" pattern is the academic equivalent.

**Trade-off:** The consumer bears the burden of deciding what to extract from events. This is intentional -- the research consistently shows that active curation produces better memory than passive logging.

---

## Soft Deletion **{Implemented}**

Facts are expired by setting `t_expired`, never hard-deleted. This applies to forgetting (Ebbinghaus decay), conflict resolution (superseded facts), and deduplication (consolidation).

**Rationale:** Graphiti's bi-temporal model requires full history for temporal reasoning. Hard deletion destroys audit trails and makes it impossible to answer "what did the agent believe at time T?" Soft deletion also makes undo trivial (clear `t_expired`).

**Trade-off:** Storage grows monotonically. Mitigated by planned archival compression (Phase 4) where cold non-pinned facts are moved to `.pak` files.

---

## Brute-Force Vector Search **{Implemented}**

Vector search uses O(N) cosine similarity scan with `select_nth_unstable_by` partial sort for top-K. No approximate nearest neighbor (ANN) index.

**Rationale:** At expected scale (sub-50ms for thousands of facts), brute-force is fast enough and has zero complexity overhead. LanceDB was the original ANN candidate but was deferred to keep the dependency surface small. The event-sourced architecture means we can always replay into an ANN backend later.

**Trigger for migration:** Benchmarks show >50ms at scale. This is tracked as a deferred item, not a planned phase.

---

## Send+Sync via ConnectionPool **{Implemented}**

`MemoryEngine` is `Send + Sync`. Thread safety comes from `ConnectionPool` (N readers + 1 writer via `parking_lot::Mutex`) and `RwLock` for the in-memory graph and scope tree.

**History:** Phases 1-2 used a single-writer `!Send` design where the engine owned one `Connection`. This was adequate for single-threaded testing but blocked integration with async runtimes (tokio) and multi-threaded consumers. Phase 3 replaced it with the current pool-based design.

**Trade-off:** More complex internal locking. Mitigated by keeping lock scopes narrow -- embedding computation happens outside the write lock, scope resolution uses short-lived read locks, and graph queries never hold the pool.

---

## Hierarchical Scoping **{Implemented}**

Scopes form a tree (not a flat namespace). Scope paths like `"user:michael/project:demo"` resolve to integer IDs. `ScopeQuery` supports Exact, Subtree, Ancestors, and Inherited resolution modes.

**Rationale:** Mem0's hierarchical user/session/agent levels proved that flat scoping is insufficient. An agent working on multiple projects for multiple users needs isolation without running separate databases. The tree structure with path-based resolution gives consumers a natural namespace model.

**Trade-off:** Scope resolution adds a lookup step to every query. Mitigated by the in-memory `ScopeTree` cache -- resolution is a tree walk, not a database query.

---

## Unforgettable Facts **{Planned}**

An `is_pinned: bool` flag on facts, with a `PersistenceClassifier` trait hook that lets the consumer decide which facts should never be forgotten.

**Rationale:** Identity facts ("the user's name is Michael", "this agent runs on Mac Mini M4") must survive all forgetting cycles. Without pinning, the Ebbinghaus decay curve would eventually expire them if they are not accessed frequently enough.

**Design:** The `PersistenceClassifier::should_pin(fact) -> bool` trait lets the consumer use LLM-based classification, rule-based logic, or a combination.

---

## Future Memory **{Planned}**

Facts can have `t_valid` set to a future timestamp. They exist in storage but do not surface in queries until their date arrives. `resume_context(now)` will use the `now` parameter to filter.

**Rationale:** Agents need to schedule reminders ("meeting on Friday") and defer knowledge ("this API deprecation takes effect in March"). Graphiti's bi-temporal model already provides the `t_valid` field; this feature activates it for scheduling.

**API:** `drain_due(now)` returns facts whose `t_valid` has arrived. `next_due_time()` returns the earliest pending `t_valid` for timer-based polling.

---

## MCP Server **{Planned}**

A `memory-engine-mcp` workspace member that exposes the engine's API as MCP tools. Separate from the core crate.

**Rationale:** MCP is the standard protocol for agent-to-tool communication. The engine should be usable as an MCP server so any MCP-compatible agent can use it without Rust integration. Keeping it as a separate workspace member prevents the core crate from depending on MCP/async/networking.

---

## Knowledge Base Integration **{Planned}**

A `KnowledgeBaseConnector` trait that lets the engine reference external knowledge bases. Facts can carry a `KnowledgeRef` URI field pointing to raw content.

**Rationale:** Memory and knowledge are distinct (see [Research Basis](research-basis.md)). The engine stores what the agent has internalized; knowledge bases store raw content. But facts should be able to cite their sources. The connector trait keeps the boundary clean: the engine never fetches content directly, but it can tell the consumer where to look.

**Degradation:** When the KB is unreachable, the engine returns a "memory lapse" marker. The consumer retries later.
