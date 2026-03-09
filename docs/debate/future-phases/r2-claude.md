# Round 2 — Claude

### 1. Issue Backlog Placement

**Phase 3a (hardening) before 3b (lifecycle). Non-negotiable for #5 and #6.**

Thread safety (#5) is the single biggest blocker for real-world adoption. Every consumer that wants concurrent access (MCP server, REST API, any async runtime) needs this. Building wake-up, KB protocol, and Web UI on top of a `!Send` engine means every downstream consumer has to independently solve the concurrency problem. Do it once, in the engine.

#6 (AddFactOptions builder) is where `unforgettable: bool` lives. Without it, there's no clean API for the unforgettable flag. This is a prerequisite for Phase 3b.

#3 (ANN index) and #4 (SQL-level filters) are performance optimizations. Defer them — they matter at scale, not at current usage. Benchmark first.

So: Phase 3a = #5 + #6. Phase 3b = wake-up + future memory + unforgettable. #3 and #4 stay data-driven, triggered by benchmarks.

### 2. KB Protocol vs Web UI Ordering

**KB Protocol before Web UI. Codex is right.**

The KB protocol is a _capability_ — it extends what the engine can do. The Web UI is an _observability tool_ — it shows what the engine already does. You build capabilities first, then instruments to observe them. Also: the Web UI benefits from having KB references to display. If you build the UI first, you'll redesign it when KB links arrive.

Gemini's grouping (KB + UI in Phase 4) conflates two different concerns. The KB protocol affects the engine's data model (new `KnowledgeRef` type, new trait). The UI is a pure consumer that reads existing data. They have different dependency profiles.

### 3. Passive Library vs Active Heartbeat

**Option A: Stay passive. Gemini's MemoryHeartbeat is over-engineering.**

The library doesn't have an event loop and shouldn't get one. That would violate the embedded constraint (who owns the thread?). Future memories surface when `resume_context()` is called. The consumer decides when to call it — at session start, on a cron schedule, on MCP tool invocation, whatever.

If the consumer wants periodic checks, they set up a timer in their own runtime and call `resume_context()`. The library provides the query; the consumer provides the scheduling. This is the same pattern as `EmbeddingProvider` — the engine defines the interface, the consumer provides the implementation.

Gemini's concern is valid (how does the agent know it's March 19th?) but the answer is: _the agent's runtime checks, not the library_. The library's job is to answer "what's due right now?" when asked.

### 4. Unforgettable Auto-Tagging

**Option A: Trait method. Codex is right that it's not a storage problem, but wrong to cut it entirely.**

The engine should provide the _hook_, not the _logic_. A `PersistenceClassifier` trait with `fn should_pin(&self, fact: &Fact) -> bool` lets the consumer inject any classification logic — LLM-based, rule-based, ML classifier, whatever. The engine calls this during `add_fact()` and sets the flag accordingly.

This is consistent with the existing trait pattern (EmbeddingProvider, SummaryGenerator, ConflictArbiter). The engine defines the extension point; the consumer fills it in.

Default implementation: `fn should_pin(&self, _fact: &Fact) -> bool { false }` — opt-in, zero behavior change for existing consumers.

### 5. Event-Sourced Sync Feasibility

**Tractable but non-trivial. The single-writer invariant is the real problem.**

Event-sourced sync works IF events are commutative (order-independent). Our events are NOT commutative — `ExpireFact(id=X)` after `AddFact(id=X)` is fine, but the reverse is nonsensical. So naive log merge fails.

Solutions exist:

- **Vector clocks** per device, merge with causal ordering. Works but adds overhead to every event.
- **CRDTs** for the event log — each device appends to its own partition, merge is union. Conflicts resolved by the existing `ConflictArbiter` trait. This is the most natural fit.
- **Operational transform** — overkill for append-mostly logs.

The CRDT approach means: each device has a device ID, events are tagged with `(device_id, sequence_number)`, merge is "take all events I haven't seen." The ConflictArbiter handles semantic conflicts (both devices expired the same fact differently).

This is feasible but requires: device identity (Phase 5 auth), event tagging (schema migration), and merge logic (new `SyncProvider` trait). It doesn't break the single-writer invariant _per device_ — each device is still single-writer locally. The merge happens at sync time, not at write time.

**Verdict:** Deferred correctly, but the event-sourced architecture actually makes this easier than a state-based system. The groundwork is there.
