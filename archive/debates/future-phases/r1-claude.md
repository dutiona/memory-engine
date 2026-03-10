# Round 1 — Claude

## Proposed Phasing

### Phase 3: "Agent Lifecycle" (themes 2, 3, 4 + issue backlog #3-#6)

**Group rationale:** These three themes are tightly coupled and collectively define _how an agent uses memory_. Future memory is meaningless without a wake-up procedure to surface it. Unforgettable memories are meaningless without a forgetting system that respects them (we have forgetting, but no "pin" flag). Wake-up needs importance scoring to rank what to load — and Phase 2's importance computation needs to be materialized (persisted) rather than computed on-the-fly.

**Dependency chain:** Unforgettable flag (4) → Wake-up procedure uses it (3) → Wake-up surfaces future memories (2).

**Also in Phase 3:** The existing issue backlog (#3 ANN index, #4 SQL-level filters, #5 thread safety, #6 AddFactOptions builder). These are hardening tasks that unblock real-world usage. Thread safety (#5) in particular is prerequisite for any network-facing consumer (MCP server, REST API). The AddFactOptions builder (#6) is where the `unforgettable: bool` flag would live.

**Estimated scope:** Medium. Unforgettable is a schema migration + flag. Future memory is near-zero new code (query filter on `t_valid <= now`). Wake-up is a new `resume_context()` method composing existing primitives. The issue backlog items are well-scoped.

### Phase 4: "Knowledge Integration" (theme 1 + MCP server)

**Group rationale:** The KB protocol requires memory-engine to be network-accessible itself. An MCP server adapter (already in the "Future" section of the roadmap) is the natural first consumer. Once memory-engine has an MCP interface, the KB protocol becomes a trait (`KnowledgeBaseConnector`) that the MCP server uses to bridge to external KBs.

**Dependency chain:** Thread safety (Phase 3, #5) → MCP server (needs async/concurrent access) → KB protocol (needs network transport).

**Key design decision:** The protocol should be defined as a Rust trait first, then projected onto a wire format (probably gRPC with protobuf for type safety and streaming, or just HTTP+JSON for simplicity). The middleware crate idea maps to a `memory-kb-bridge` crate that implements the trait for specific KBs.

**Graceful degradation:** The `KnowledgeRef` on facts stores a URI. When the KB is unreachable, the fact is still queryable — it just has a "knowledge unavailable" annotation. A background retry mechanism (or wake-up stage) can periodically attempt to resolve pending refs.

### Phase 5: "Visualization" (theme 5)

**Group rationale:** Web UI is a separate concern from the core library. It consumes the API but doesn't affect the engine's design. It should come after the API stabilizes (post Phase 4).

**Separate crate/project:** Yes. A `memory-ui` crate or workspace member. WASM compilation of a subset of engine types for client-side rendering, with a REST/WebSocket backend for data.

### Deferred / Separate Product: Auth (6) and SaaS Sync (7)

**Cut from the library roadmap entirely.** These are product concerns, not library concerns.

- **Auth** is a deployment concern. The MCP server or REST API layer handles auth, not the engine. The engine doesn't know about users — it knows about facts. Multi-tenancy = multiple engine instances or a namespace prefix. Don't pollute the core with auth.
- **SaaS Sync** is a separate product built _on top of_ memory-engine. It needs: a sync server, key management, CRDT merge logic, certificate infrastructure. None of this belongs in the engine crate. The event-sourced architecture makes this feasible (sync = exchange event logs + merge), but the implementation is a distinct project.

## Key Challenge

The biggest risk is Phase 3 scope creep. The issue backlog (#3-#6) plus three new themes is a lot. I'd argue: split Phase 3 into 3a (issue backlog: hardening) and 3b (agent lifecycle: wake-up + unforgettable + future memory). Ship 3a first because thread safety unblocks everything downstream.
