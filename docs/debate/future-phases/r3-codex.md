1. Scheduling API: ship the richer passive surface. `resume_context(now)` alone is too opinionated and hides useful host-level control. The right core is `next_due_time()` + `drain_due(now)`, with `resume_context(now)` as a convenience wrapper built on top. That preserves the “library, not event loop” constraint while supporting schedulers, CLIs, agents, and tests cleanly.

2. Phase 4: inspection tooling first, MCP in the same phase if it is just a thin adapter over stable inspection/query primitives. KB Protocol should not be the first external contract. You want observability, debuggability, and operator confidence before you freeze a protocol surface. So Phase 4 is local inspection plus MCP exposure; KB Protocol moves to Phase 5.

3. Event schema: add the forward-compatible envelope now. Gemini is right on this one. If the system is already event-shaped, adding `timestamp`, `origin_node_id`, and `sequence_id` now is cheap; retrofitting them after real usage is not. The key constraint is that these fields remain metadata in single-node mode, not semantics that complicate Phase 3 behavior.

```text
Phase 3a: Core Hardening — thread safety (#5), AddFactOptions builder (#6), forward-compatible event envelope metadata (`timestamp`, `origin_node_id`, `sequence_id`), concurrency/determinism tests

Phase 3b: Temporal Memory — wake-up, Future Memory, Unforgettable via manual flag + trait hook, passive scheduling API (`next_due_time()`, `drain_due(now)`, `resume_context(now)`)

Phase 4: Inspection & MCP — inspection/debug/statistics APIs, event/fact introspection tooling, benchmark harnesses and perf baselines, thin MCP server over the stabilized local API

Phase 5: Protocol & Replication — KB Protocol, event-log replication model, CRDT/event-sourced sync semantics, conflict resolution rules, remote adapters

Deferred: ANN indexing (#3) until benchmarks prove need, SQL filters (#4) until query pressure justifies complexity, auth, SaaS sync/productization, web UI
```

This ordering keeps Phase 3 focused on correctness and usable temporal semantics, Phase 4 on visibility and integration, and Phase 5 on distributed/external contracts once the core shape is stable.
