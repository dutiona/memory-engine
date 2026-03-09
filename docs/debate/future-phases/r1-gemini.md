I propose the following roadmap for `memory-engine`, focusing on evolving it from a "retrieval engine" to a "cognitive substrate."

### Phase 3: Cognitive Presence (The Temporal Core)
**Themes:** *Future Memory, Unforgettable Memories, Wake-up Procedure*

*   **Rationale:** These themes are logically inseparable. A "Wake-up" procedure is useless without the "Future Memories" (intentions) and "Unforgettable Memories" (identity) that define an agent’s baseline state. 
*   **Dependencies:** None. These leverage existing Phase 1/2 bi-temporal and importance logic.
*   **Strategy:** Implement as the **"Context Reconstitution"** layer. 
    *   **Future/Unforgettable:** Add a `persistence_mode` bitmask to the `Fact` schema. 
    *   **Wake-up:** Create a `reconstitute_working_memory()` method that pulls the "Identity Seed" (Unforgettable) and "Due Intentions" (Future where `t_valid <= now`).
*   **Trade-off:** Minimal code changes, but high impact on agent "personality" and reliability.

### Phase 4: Extended Boundaries (Knowledge & Observability)
**Themes:** *KB Protocol, Web UI*

*   **Rationale:** Once the internal cognitive loop is stable, the agent needs to link to external data (KB) and the developer needs to see *why* the agent thinks what it thinks (UI).
*   **Dependencies:** Requires Phase 3 to ensure the "summaries" stored in memory are correctly linked to the "raw content" in the KB.
*   **Strategy:** 
    *   **KB Protocol:** Define a URI-based trait (`ExternalSourceResolver`). Memory stores the URI and a summary; the agent fetches raw text only when the summary hits a high relevance score.
    *   **Web UI:** Develop as a separate workspace member (`memory-engine-viz`). Use WASM to render the `petgraph` structure. This is essential for debugging Phase 2's consolidation logic.
*   **Trade-off:** High DX (Developer Experience) value, but carries the risk of "feature creep" into the UI layer.

### Phase 5: Distributed Sovereignty (Cloud-Local Bridge)
**Themes:** *Auth, SaaS Sync*

*   **Rationale:** This is the most complex transition. Syncing an event-sourced, graph-based system across devices requires robust identity (Auth) and conflict resolution.
*   **Dependencies:** Requires Phase 2 (Conflict Resolution) and Phase 4 (URI stability).
*   **Strategy:** 
    *   **Auth:** Keep it "Sovereign." Instead of a centralized Auth provider, use Ed25519 key-pairs. A "User" is a public key; "Auth" is signing the event log.
    *   **Sync:** Since the engine is append-only/event-sourced, sync becomes a **log-merging** problem. Use a `SyncProvider` trait to push encrypted event chunks.
*   **Trade-off:** Significant architectural overhead. Moving from single-writer to multi-device requires handling "split-brain" scenarios in the event log.

### Deferred / Separate Project
*   **Multi-user Auth:** Deferred. `memory-engine` should remain an *embedded* library for a single agent's mind. Multi-tenant logic belongs in the wrapper (e.g., a memory-server), not the engine core.

### Summary of Clusters
1.  **The Temporal Cluster (Phase 3):** Internal logic. Makes the agent "aware" of time and identity.
2.  **The Interface Cluster (Phase 4):** External links. Connects the "mind" to "books" (KB) and "eyes" (UI).
3.  **The Network Cluster (Phase 5):** Distributed state. Connects "minds" across devices.

**Assumption Challenge:** I assume "Future Memory" is near-zero code. However, the *trigger* mechanism (how does the library tell the agent "hey, it's March 19th") requires a background polling or hook system that `memory-engine` currently lacks. Phase 3 must define the `MemoryHeartbeat` trait to handle this.
