## Gemini’s Rebuttal: System Integrity and Architectural Purity

The consensus on Phase 3 is a strong foundation, but the implementation details risk bloating the engine or compromising its library-first nature. Here is my stance on the points of contention:

### 1. Hardening Before Horizon (Backlog Placement)
Thread safety (**#5**) is not "hardening"—it is a foundational requirement. Any agent framework (LangChain, MetaGPT, or a custom MCP server) will use an async runtime. Providing a non-`Send + Sync` library is a non-starter. **Issue #5 and #6 (Builder pattern) must be Phase 3a.** Ergonomics and safety are prerequisite to the complex temporal logic of future memories. ANN (#3) and SQL filters (#4) are optimization targets for Phase 4.

### 2. Standards Over Surfaces (KB Protocol vs. Web UI)
I challenge the focus on a Web UI. For a memory *engine*, the **KB Protocol** is the primary interface. A Web UI is a debugging luxury; the KB Protocol is the integration reality. Implementing the protocol in Phase 4 provides an immediate test-bed for the "Unforgettable" and "Future Memory" features in a standardized way. Web UI should remain a Phase 5 candidate or, preferably, a separate consumer project.

### 3. The Library is a Clock-Slave (Passive vs. Active)
We must **stay passive (A)**. Introducing a background thread or executor dependency (Tokio/async-std) inside the engine is a leak of concerns. The engine should not have a heartbeat; it should have an **audit capability**. 
*   **Proposal:** `MemoryEngine::poll_temporal_events(now: DateTime) -> Vec<TemporalEffect>`. 
The consumer (the agent loop) decides how often to pulse. This preserves the library’s portability across different runtimes (including WASM).

### 4. Mechanisms, Not Morals (Unforgettable Auto-Tagging)
Codex is right to avoid storage-layer policy, but wrong to suggest we cut it. We should implement **(A) a trait-based policy**: `UnforgettablePolicy::is_permanent(&self, fact: &Fact) -> bool`.
Hardcoding heuristics (B) is too brittle for different domains (e.g., medical vs. creative writing). The engine provides the `unforgettable` bit in the schema; the `UnforgettablePolicy` trait allows the consumer to inject LLM-based classification or simple regex without the engine needing an NLP dependency.

### 5. Deterministic Convergence (Event-Sourced Sync)
The fear that event-merging breaks the single-writer invariant is overblown. Because we are event-sourced, sync is a **Conflict-free Replicated Data Type (CRDT)** problem. If we append `(sequence_id, origin_node_id, timestamp)` to every event, merging is a deterministic interleaving of logs. 
*   **The Invariant:** The *active* state (the SQL tables) can be wiped and replayed from the merged log at any time. 
Sync is tractable *because* we chose event-sourcing, but it requires Phase 3 to finalize the event schema to include these metadata fields before we bake the storage format.

### Summary of Disagreement
I disagree with Claude’s push for an MCP server as the "next big thing" and Codex’s dismissal of auto-tagging. We are building a *kernel* for memory. The kernel must be thread-safe, passive, and policy-agnostic. Everything else—including the MCP server—is a shell that lives on top. Phase 3 must prioritize the **Builder and Thread-Safety** to ensure the kernel doesn't crack under the weight of Phase 4's protocol requirements.
